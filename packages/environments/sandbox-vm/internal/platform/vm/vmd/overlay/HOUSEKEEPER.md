# Overlay Housekeeper

## 需求背景

Agent 的每次任务在 overlayfs 上执行。一个 session 可能包含多轮任务，每轮任务结束后：

1. 宿主机通过 Diff Analyzer 读取变更，将变更 apply 到 lower 层（宿主机文件系统）
2. 理想情况下 upper 层应该被清空（因为变更已经合并到 lower），下一轮任务从干净状态开始

**问题**：apply 到 lower 后，upper 层中仍然残留大量内核私有状态——opaque 目录、redirect xattr、与 lower 内容相同的文件副本、指向已不存在目标的 whiteout 等。这些残留物会导致：

- **语义冲突**：opaque 目录会遮蔽 lower 中新 apply 的内容，redirect 指向已经移动过的旧路径
- **空间浪费**：与 lower 完全相同的文件副本占用 upper 层空间
- **累积膨胀**：多轮任务后 upper 层越来越大，性能下降

Housekeeper 在每轮任务结束、ExportDiff 之后、下一轮任务开始之前运行（overlay 处于 umount 状态）。它的职责是清理这些残留物，使 upper 层尽可能干净地反映当前的实际差异。

## 概述

Housekeeper 在 overlay 文件系统卸载后运行，清理 upper 层中的冗余状态（opaque、redirect 等内核私有语义），将其转换为普通文件操作（whiteout、metacopy），使 upper 层在重新挂载后仍能正确工作。

**核心目标**：消除 upper 层对内核 overlay 私有语义的依赖，使得 upper 层可以被独立操作（如 apply 到 lower 后重置）。

## 处理步骤（严格顺序）

### 1. 扁平化 Opaque 目录

将 `trusted.overlay.opaque=y` 的目录转为普通目录 + whiteout。

```
之前:  upper/proj/ [OPAQUE]
         keep.txt, new.txt
       lower/proj/
         keep.txt, old.txt, subdir/

之后:  upper/proj/
         keep.txt, new.txt
         old.txt [whiteout]    ← 新建
         subdir  [whiteout]    ← 新建
```

逻辑：
1. 遍历 lower 中同路径目录的所有条目
2. 对 upper 中不存在的条目创建 whiteout（字符设备 0:0，失败则回退到 xattr 方式）
3. 如果 lower 条目是目录且 upper 不存在，对整个目录创建一个 whiteout（`SkipDir`）
4. 移除 `trusted.overlay.opaque` xattr

### 2. 扁平化 Redirect 目录

将目录级 redirect 转为文件级 metacopy + redirect。

```
之前:  upper/newname/ [redirect=oldname]
         (空)
       lower/oldname/
         f1.txt, f2.txt, sub/f3.txt

之后:  upper/newname/
         f1.txt [metacopy, redirect=/oldname/f1.txt]
         f2.txt [metacopy, redirect=/oldname/f2.txt]
         sub/
           f3.txt [metacopy, redirect=/oldname/sub/f3.txt]
```

**处理顺序**：按深度从深到浅排序，先处理最深层的 redirect。这对嵌套 redirect 至关重要。

**嵌套 redirect 的路径解析**：

当处理 `x/y`（redirect=b）且父目录 `x` 也有 redirect=a 时，`y` 的 redirect 目标 `b` 需要相对于 `a`（而非 `x`）来解析。`resolveParentLowerDir()` 递归遍历祖先目录的 redirect 链来找到真实的 lower 路径。

```
lower/a/b/file.txt
upper/x/ [redirect=a]
  upper/x/y/ [redirect=b]

处理 x/y 时：
  parentLowerDir = resolveParentLowerDir("x") = lower/a  （因为 x 有 redirect=a）
  lowerDirPath = resolveRedirect("b", "lower/a") = lower/a/b  ✓
```

**Metacopy 文件创建**（`createMetacopy()`）：
1. 创建 sparse 文件（`Truncate` 到正确 size，不写入数据，blocks=0）
2. 复制权限和修改时间
3. 设置 `trusted.overlay.metacopy=""` xattr
4. 设置 `trusted.overlay.redirect=/原始lower路径` xattr

内核在挂载后读取文件时，发现 metacopy xattr 后通过 redirect 路径从 lower 读取实际数据。

**Metacopy 内核行为实测**：
- 稀疏文件 + metacopy + redirect：**可工作**，stat size 正确
- 零长度文件 + metacopy + redirect：可工作，但 stat size=0（不正确）
- 全量数据复制 + metacopy + redirect：可工作，但浪费空间
- 无 metacopy xattr + redirect：**不工作**，内核读 upper 数据（全零）
- **结论**：metacopy xattr 是必须的；Truncate 到正确 size 是必须的；不需要写入实际数据

**对于 symlink**：symlink 不支持 metacopy，直接复制 link target（`os.Symlink`）。

### 3. 移除陈旧条目

遍历 upper 层，删除不再需要的条目：

| 类型 | 条件 | 操作 |
|------|------|------|
| 孤立 whiteout | whiteout 的目标在 lower 中不存在 | 删除 |
| 死 redirect | 文件的 redirect 目标在 lower 中不存在 | 删除 |
| 相同文件 | 与 lower 同路径文件内容完全相同 | 删除 |
| 相同 symlink | 与 lower 同路径 symlink 目标相同 | 删除 |

**注意**：带有 redirect 的文件即使内容与 lower 相同，也不会被当作"相同文件"删除——它们先走 redirect 检查路径。

文件比较（`filesAreIdentical`）：类型相同 → 大小相同 → 逐块比较内容（32KB buffer）。

### 4. 清理空目录

自底向上遍历。对于空目录：
- lower 中存在同路径目录 → 删除（冗余 copy-up）
- lower 中不存在 → 保留（用户新建的目录）

## 统计输出

```go
type HousekeepResult struct {
    IdenticalFilesRemoved int // 步骤 3 移除的相同文件/symlink/whiteout/redirect 数量
    OpaqueDirsFlattened   int // 步骤 1 扁平化的 opaque 目录数量
    WhiteoutsCreated      int // 步骤 1 创建的 whiteout 数量
    RedirectDirsFlattened int // 步骤 2 扁平化的 redirect 目录数量
}
```

## 使用

```go
hk := NewHousekeeper(upperDir, lowerDir)
result, err := hk.Run()
```

## 集成测试

`TestHousekeeperIntegration` 在 Docker 容器中运行真实的 overlayfs 测试：

1. 交叉编译 `housekeeper-runner` 二进制
2. 构建包含 `attr` 工具的 Docker 镜像（`testdata/Dockerfile`）
3. 在 `--privileged` 容器中运行 `testdata/docker_overlay_housekeeper_integration_test.sh`
4. 脚本创建 overlayfs（`metacopy=on,redirect_dir=on`），执行文件操作，umount 后运行 housekeeper，remount 验证结果

运行：
```bash
go test -v -run TestHousekeeperIntegration -timeout=5m ./internal/platform/vm/vmd/overlay/
```

### 测试覆盖场景

| # | 场景 | 验证点 |
|---|------|--------|
| 1 | 相同文件删除（metacopy copy-up） | 文件内容不变 |
| 2 | 孤立 whiteout 删除 | 不影响其他文件 |
| 3 | 空目录清理 | lower 中的文件仍可访问 |
| 4 | Opaque 目录扁平化 | whiteout 正确屏蔽 lower 文件 |
| 5 | Redirect 目录扁平化 | metacopy 文件可正确读取 |
| 6 | Rename + 修改 + 删除 | 组合操作正确 |
| 7 | 嵌套 redirect | 父子 redirect 链正确解析 |
| 8 | chmod metacopy | 权限保留，内容可读 |
| 9-10 | Symlink 处理 | 相同删除，不同保留 |
| 11 | 多次 remount 稳定性 | 3 次 remount 结果一致 |
| 12 | 大文件（128KB）| 内容完整 |
| 13 | Opaque 但 lower 无对应目录 | 仅移除 xattr |
| 14 | 文件 rename（redirect + whiteout）| 内容可读 |
| 15 | 复杂场景：rename 目录 + 修改 + 删除 + 新增 | 全部正确 |

## 前置条件

1. Overlay 必须已卸载
2. 对 upper/lower 有读写权限
3. 文件系统支持 xattr
