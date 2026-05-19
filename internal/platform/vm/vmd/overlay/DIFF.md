# Overlay Diff Analyzer

## 需求背景

Agent 在虚拟机内执行任务时，工作区通过 overlayfs 挂载：lower 层是宿主机共享的只读目录（通过 virtio-fs），upper 层记录 agent 的所有修改。任务完成后，宿主机需要知道 agent 修改了哪些文件，用于：

1. **ExportDiff**：将变更导出为 tar 包，apply 到宿主机的实际文件系统
2. **文件变更通知**：告知上层系统哪些文件被创建、修改、删除或移动

难点在于 overlayfs 的 upper 层不是简单的文件副本——内核使用 opaque 目录、whiteout 设备文件、redirect xattr、metacopy xattr 等私有语义来记录变更。直接读取 upper 层文件无法正确理解实际的用户操作。Diff Analyzer 的职责就是解析这些内核语义，还原出用户视角的文件变更列表。

## 概述

`OverlayDiffAnalyzer` 分析 overlay 文件系统 upper/lower 层之间的差异，输出文件的创建、更新、删除和移动操作。它理解 overlay 的所有语义：opaque 目录、whiteout、redirect 和 metacopy。

## 核心算法：Merge Walk

同时遍历 upper 和 lower 目录树，按字典序合并条目，逐层递归生成统一的差异视图。

### 遍历流程

1. `mergeDir()` 读取某一层级的 upper 和 lower 条目，合并为 `mergedEntry` 列表
2. `mergeWalk()` 对每个 `mergedEntry` 判定类型（创建/更新/删除/移动/陈旧），并递归处理子目录
3. `Analyze()` 汇总结果，匹配 move 的源和目标，排序输出

### mergedEntry 字段

```go
type mergedEntry struct {
    Name         string      // 合并视图中的名称
    UpperAbsPath string      // upper 层绝对路径（可能为空）
    LowerAbsPath string      // lower 层绝对路径（可能为空）
    IsWhiteout   bool        // 是否为 whiteout
    IsOpaque     bool        // 是否为 opaque 目录
    Redirect     string      // redirect xattr 值
}
```

## Overlay 语义处理

### Whiteout

表示删除。检测方式：
- 字符设备 major:minor = 0:0（Linux 标准）
- 零字节文件 + `trusted.overlay.whiteout=y` xattr（macOS 测试用）

处理逻辑：
- whiteout + lower 存在 → 文件被删除
- whiteout + lower 不存在 → 孤立 whiteout（陈旧条目）

### Opaque 目录

`trusted.overlay.opaque=y`。表示目录被 `rm -rf` 后重建，隐藏 lower 同名目录的全部内容。

处理逻辑：
- lower 中存在但 upper 中不存在的条目 → 视为删除
- upper 中的条目正常处理

### Redirect

`trusted.overlay.redirect` xattr。表示文件或目录的移动/重命名。

路径格式：
- 绝对路径（`/src/components`）：相对于 overlay root
- 相对路径（`oldname`）：相对于当前 lower 目录

目录 redirect 会改变子条目的 lower 查找路径。当父目录有 redirect 时，子条目的 `lowerRelPath` 和 `entryRelPath` 不同，`pathDiffers=true`，据此检测移动。

### Metacopy

`trusted.overlay.metacopy` xattr。表示仅元数据被 copy-up（如 chmod），数据仍从 lower 读取。分析器将其视为已更新文件。

## 差异分类逻辑

对每个 mergedEntry：

| 条件 | 分类 |
|------|------|
| whiteout + lower 存在 | 删除 |
| whiteout + lower 不存在 | 陈旧（孤立 whiteout） |
| 仅 upper + 有 redirect + 目标存在 | 移动（目录递归，文件直接标记） |
| 仅 upper + 有 redirect + 目标不存在 | 陈旧（孤立 redirect） |
| 仅 upper + 无 redirect | 创建 |
| 双层 + 有 redirect | 移动（重定向到 lower 实际位置） |
| 双层 + pathDiffers（父 redirect 导致） | 移动 |
| 双层 + 同为目录 | 递归 |
| 双层 + 类型不同 | 更新（类型变更） |
| 双层 + 同为文件 | 更新 |
| 仅 lower + pathDiffers | 移动（未修改内容） |
| 仅 lower + 同路径 | 无变化，跳过 |

### 移动检测

移动 = 创建 + 删除的匹配。`Analyze()` 最后阶段从 `deletedFiles` 中移除已被 `MovedFrom` 引用的路径。

## 输出

```go
type OverlayDiffAnalyzerResult struct {
    FileDiff          []FileDiff       // 所有变更，按路径排序
    StaleUpperEntries []StaleUpperEntry // 可清理的陈旧条目
}

type FileDiff struct {
    Path      string    // 合并视图中的路径
    Mode      uint32    // 文件权限
    IsDir     bool
    IsUpdated bool      // 更新（非新建）
    IsDeleted bool      // 删除
    MovedFrom string    // 移动来源路径（空=非移动）
    Timestamp time.Time
    Size      int64
}
```

## 使用

```go
analyzer, _ := NewOverlayDiffAnalyzer(upperDir, lowerDir)
defer analyzer.Close()
result, _ := analyzer.Analyze()

for _, d := range result.FileDiff {
    switch {
    case d.MovedFrom != "":
        // 移动: d.MovedFrom → d.Path
    case d.IsDeleted:
        // 删除: d.Path
    case d.IsUpdated:
        // 更新: d.Path
    default:
        // 创建: d.Path
    }
}
```
