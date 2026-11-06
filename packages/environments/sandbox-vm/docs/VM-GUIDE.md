# VM 隔离执行环境指南

VM 隔离功能允许在安全的虚拟机环境中执行 bash 命令和文件操作，避免直接在主机上执行可能不安全的代码。本文档全面介绍了如何配置、使用和维护 VM 隔离环境。

## 目录

- [架构概览](#架构概览)
- [前置条件](#前置条件)
- [快速开始](#快速开始)
- [核心组件](#核心组件)
- [配置说明](#配置说明)
- [VM 镜像准备](#vm-镜像准备)
- [会话管理](#会话管理)
- [共享目录与工作空间](#共享目录与工作空间)
- [OverlayFS 差异分析与导出](#overlayfs-差异分析与导出)
- [在代码中使用](#在代码中使用)
- [网络与通信](#网络与通信)
- [安全特性](#安全特性)
- [性能优化](#性能优化)
- [故障排查](#故障排查)
- [未来改进](#未来改进)

## 架构概览

当前主模型是：

- `internal/envhost/sandbox.ManagedHost` 持有 VM lifecycle
- `internal/framework/runtimebridge` 负责 capability URL 和 host-side callback bridge
- `internal/platform/vm` 是 sandbox host 使用的底层 VM 适配层

```text
┌────────────────────────────────────────────────────────────────────┐
│                         Host (macOS)                              │
│                                                                    │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │ Local VM Runtime                                            │  │
│  │ - 创建 sandbox host                                         │  │
│  │ - 直接持有 local connector / envhost                        │  │
│  └──────────────────────────────┬───────────────────────────────┘  │
│                                 │                                  │
│  ┌──────────────────────────────▼───────────────────────────────┐  │
│  │ envhost + runtimebridge                                      │  │
│  │ - sandbox host                                               │  │
│  │ - environment routing                                        │  │
│  │ - runtime bridge                                             │  │
│  └──────────────────────────────┬───────────────────────────────┘  │
│                                 │                                  │
│  ┌──────────────────────────────▼───────────────────────────────┐  │
│  │ internal/envhost/sandbox.ManagedHost                                  │  │
│  │ - 拥有 VM manager                                            │  │
│  │ - 管理 SSH config / peer services / signal handler           │  │
│  │ - 挂接 runtime bridge HTTP server                            │  │
│  │ - 向 framework 暴露 EnvironmentHost                          │  │
│  └───────────────┬───────────────────────┬──────────────────────┘  │
│                  │                       │                         │
│            ┌─────▼────────────┐   ┌──────▼───────┐                 │
│            │ internal/platform/vm.Manager│  │ RuntimeBridge│                 │
│            │ - VM lifecycle     │  │ HTTP server  │                 │
│            │ - mount / exec     │  │ /tool /api   │                 │
│            └─────┬─────────────┘   └──────┬───────┘                 │
└──────────────────┼────────────────────────┼─────────────────────────┘
                   │                        │
               Vsock / SSH              loopback HTTP
                   │                        │
        ┌──────────▼────────────────────────▼───────────────────────┐
        │                    VM (Linux ARM64)                        │
        │                                                            │
        │  vmd/init + vmrpc + overlay 管理                           │
        │  - sandbox mount namespace                                 │
        │  - command / python / file ops                             │
        │  - diff / apply / discard                                  │
        │  - guest 侧将 runtime bridge 端口转发回 host               │
        └────────────────────────────────────────────────────────────┘
```

## 前置条件

- macOS 系统（使用 Apple Virtualization Framework）
- Go 1.24+
- 已准备好的 VM 镜像文件（kernel + rootfs）

## 快速开始

### 1. 准备 VM 镜像文件

将以下文件放在 `resources/vm/` 目录下：

```
resources/vm/
├── kernel.bin      # Linux 内核（支持 gzip 压缩）
└── rootfs.img      # 根文件系统磁盘镜像（ext4）
```

**注意**：

- `kernel.bin` 支持 gzip 压缩格式，程序会自动检测并解压
- `rootfs.img` 需要是完整的磁盘镜像文件（ext4 文件系统）
- 镜像内必须包含 vmd daemon 并配置好 SSH 服务器

### 2. 编译和运行测试

```bash
make go-test
```

测试覆盖：

- 检测并解压 gzip 压缩的内核
- VM 配置生成
- 生成 SSH 密钥对
- Vsock / gRPC 辅助逻辑
- sandbox 文件操作和 diff 逻辑

### 3. 查看输出示例

```
2025/10/30 16:00:00 VM Test Program
2025/10/30 16:00:00 VM Config: CPU=2, Memory=2048MB
2025/10/30 16:00:00 Kernel: .../resources/vm/kernel.bin
2025/10/30 16:00:00 Rootfs: .../resources/vm/rootfs.img
2025/10/30 16:00:00 Starting VM...
2025/10/30 16:00:01 Kernel decompressed to: .../kernel-xxx.bin
2025/10/30 16:00:01 VM started successfully
2025/10/30 16:00:05 gRPC connection established via Vsock
2025/10/30 16:00:05 SSH connection established
2025/10/30 16:00:05 VM is running!
```

## 核心组件

### 1. Sandbox Managed Host (`internal/envhost/sandbox/inprocess_host.go`)

这是当前 VM ownership 的真正入口。它负责：

- 创建并持有 `internal/platform/vm.Manager`
- 创建 sandbox host store
- 安装 SSH config 和 signal handler
- 启动 peer services
- 将 runtime bridge 挂到 host 本地 HTTP server
- 在 `Close()` 时回收 VM 和附带资源

对外它暴露的是 `envhost.EnvironmentHost`，而不是直接暴露 `vm.Manager` 给 framework。

### 2. VM Manager (`internal/platform/vm/manager.go`)

`vm.Manager` 现在是 sandbox host 使用的底层 VM 适配器，负责 VM 的完整生命周期：

```go
type Manager struct {
    config          *Config
    vm              atomic.Pointer[vz.VirtualMachine]
    executor        vmExecutor                     // SSH 执行器
    peer            *vmrpc.Peer                    // gRPC 双向通信
    mountedSandboxes map[string]bool               // 跟踪已挂载的沙箱
}
```

核心方法：

- `Start(ctx)` - 启动 VM 并建立连接
- `Stop(ctx)` - 停止 VM
- `CreateSandbox(ctx)` - 创建新沙箱（返回生成的 sandboxID）
- `MountSandbox(ctx, sandboxID)` - 挂载沙箱工作空间
- `UnmountSandbox(ctx, sandboxID)` - 卸载沙箱工作空间
- `GetSandboxExecutor(sandboxID)` - 获取沙箱专用执行器
- `GetSandboxState(ctx, sandboxID)` - 获取工作空间状态
- `ApplySandboxDiff(ctx, sandboxID, paths, hostBaseDir)` - 应用差异到主机
- `DiscardSandboxAllChanges(ctx, sandboxID)` - 丢弃所有变更

### 3. vmd Daemon (`internal/platform/vm/vmd/`)

运行在 VM 内部的守护进程，作为 PID 1 初始化进程：

```
vmd/
├── init/           # VM 初始化进程
├── ops/            # 操作实现
│   ├── sandbox.go  # 沙箱管理（OverlayFS）
│   ├── mount.go    # 挂载操作
│   ├── paths.go    # 路径配置
│   ├── workspace.go# 工作空间操作
│   └── python.go   # Python 执行支持
├── overlay/        # OverlayFS 实现
│   ├── diff.go     # 差异分析器
│   └── housekeeper.go  # 清理和优化
└── server/         # gRPC 服务器
```

### 4. gRPC / vmrpc 通信 (`internal/platform/vm/vmrpc/`)

通过 Vsock 实现 Host 和 VM 之间的双向 gRPC 通信：

```protobuf
// vmrpc.proto
service HostService {
    rpc CallTool(ToolRequest) returns (ToolResponse);
}

service VMService {
    rpc Exec(ExecRequest) returns (ExecResponse);
    rpc ExecStream(ExecRequest) returns (stream ExecOutput);
    rpc CreateSandbox(CreateSandboxRequest) returns (CreateSandboxResponse);
    rpc DeleteSandbox(DeleteSandboxRequest) returns (DeleteSandboxResponse);
    rpc SandboxExists(SandboxExistsRequest) returns (SandboxExistsResponse);
    rpc MountSandbox(MountSandboxRequest) returns (MountSandboxResponse);
    rpc UnmountSandbox(UnmountSandboxRequest) returns (UnmountSandboxResponse);
    rpc GetSandboxFileDiff(GetSandboxFileDiffRequest) returns (GetSandboxFileDiffResponse);
    rpc ExportSandboxDiff(ExportSandboxDiffRequest) returns (stream ExportSandboxDiffResponse);
    rpc DiscardSandboxAllChanges(DiscardSandboxAllChangesRequest) returns (DiscardSandboxAllChangesResponse);
    rpc RunSandboxHousekeeper(RunSandboxHousekeeperRequest) returns (RunSandboxHousekeeperResponse);
    rpc SetupWorkspaces(SetupWorkspacesRequest) returns (SetupWorkspacesResponse);
    rpc SetProxyEnv(SetProxyEnvRequest) returns (SetProxyEnvResponse);
    // ... 更多 RPC
}
```

**通信流程**：

1. Host 在 Vsock 50052 监听 HostService
2. VM (vmd) 在 Vsock 50051 监听 VMService
3. 双向调用：Host ↔ VM 通过 gRPC

### 4. Executor 接口 (`pkg/types/sandbox.go`)

定义了统一的命令执行接口：

```go
type Executor interface {
    // ExecuteCommand executes a command with arguments safely without shell interpretation.
    // The first element of args is the command, the rest are arguments.
    ExecuteCommand(ctx context.Context, args []string, workingDir string) (stdout, stderr string, exitCode int, err error)
    ReadFile(ctx context.Context, path string) ([]byte, error)
    WriteFile(ctx context.Context, path string, content []byte, append bool) error
    DeleteFile(ctx context.Context, path string) error
    FileExists(ctx context.Context, path string) (bool, error)
    Close() error
}
```

### 5. SandboxExecutor

沙箱特定的执行器，所有操作通过 gRPC 路由到特定沙箱的 mount namespace：

```go
type SandboxExecutor struct {
    manager   *Manager
    sandboxID string
}
```

## 配置说明

### 默认配置

```go
config := vm.DefaultConfig("./resources")

// 默认值：
// - CPUCount: 2
// - MemorySize: 2GB
// - KernelPath: resources/vm/kernel.bin
// - RootfsPath: resources/vm/rootfs.img
// - BootCommand: "console=hvc0 root=/dev/vda rw"
// - SSHUser: root
// - SSHPort: 22
// - StartupTimeout: 60s
// - RootfsOverlaySize: 32GB
```

### 完整配置选项

```go
type Config struct {
    // VM 资源
    CPUCount   uint                        // CPU 核心数
    MemorySize uint64                      // 内存大小（字节）

    // VM 镜像
    KernelPath  string                     // 内核路径（支持 gzip）
    RootfsPath  string                     // 根文件系统镜像
    BootCommand string                     // 内核启动参数

    // 网络配置
    MACAddress string                      // 固定 MAC 地址（可选）

    // 代理配置
    HTTPProxy  string                      // HTTP 代理 URL
    HTTPSProxy string                      // HTTPS 代理 URL
    NoProxy    string                      // 不走代理的主机列表

    // SSH 配置
    SSHUser string                         // SSH 用户名（通常 root）
    SSHPort int                            // SSH 端口（通常 22）

    // 宿主机目录
    HostHomeDir      string                // 宿主机主目录（skills、SSH 密钥等）
    HostHomeMountDir string                // VM 内的挂载路径

    // 目录挂载（多个）
    Mounts []Mount                         // VirtioFS 挂载配置

    // 超时
    StartupTimeout time.Duration           // VM 启动超时

    // Rootfs overlay
    RootfsOverlaySize int64               // 默认 32GB
    RootfsOverlayDir  string              // 存储位置
}

type Mount struct {
    HostPath string                        // 宿主机路径
    VMPath   string                        // VM 内路径
    ReadOnly bool                          // 只读挂载（跳过 overlay）
}
```

### 配置示例

```go
config := &vm.Config{
    // VM 资源
    CPUCount:   2,
    MemorySize: 2 * 1024 * 1024 * 1024,  // 2GB

    // VM 镜像
    KernelPath:  "./resources/vm/kernel.bin",
    RootfsPath:  "./resources/vm/rootfs.img",
    BootCommand: "console=hvc0 root=/dev/vda rw",

    // 代理配置（继承宿主机设置）
    HTTPProxy:  os.Getenv("HTTP_PROXY"),
    HTTPSProxy: os.Getenv("HTTPS_PROXY"),
    NoProxy:    os.Getenv("NO_PROXY"),

    // 多目录挂载
    Mounts: []vm.Mount{
        {
            HostPath: "/Users/john/project",
            VMPath:   "/Users/john/project",  // 保持路径一致
            ReadOnly: false,
        },
        {
            HostPath: "/Users/john/shared-data",
            VMPath:   "/data",
            ReadOnly: true,  // 只读挂载，跳过 overlay
        },
    },

    // 超时设置
    StartupTimeout: 120 * time.Second,
}
```

## VM 镜像准备

### 1. 获取 Linux 内核

你可以从以下来源获取内核镜像：

- Ubuntu cloud images
- Alpine Linux（推荐用于轻量级场景）
- 或自己编译

### 2. 创建根文件系统镜像

```bash
# 创建空镜像（512MB）
dd if=/dev/zero of=rootfs.img bs=1m count=512

# 格式化为 ext4
mkfs.ext4 rootfs.img

# 挂载并安装系统
sudo mount -o loop rootfs.img /mnt
sudo debootstrap --arch=arm64 focal /mnt

# 配置系统
sudo chroot /mnt

# 安装 SSH 服务器
apt-get update
apt-get install -y openssh-server

# 配置 SSH 允许 root 登录和密钥认证
cat >> /etc/ssh/sshd_config <<EOF
PermitRootLogin yes
PubkeyAuthentication yes
AuthorizedKeysFile .ssh/authorized_keys
EOF

# 创建 SSH 目录
mkdir -p /root/.ssh
chmod 700 /root/.ssh

# 配置网络（DHCP）
cat > /etc/network/interfaces <<EOF
auto lo
iface lo inet loopback

auto eth0
iface eth0 inet dhcp
EOF

# 安装必要工具
apt-get install -y curl wget vim

# 退出并卸载
exit
sudo umount /mnt
```

### 3. 内核配置要求

为了使用共享目录和 overlay 功能，内核必须启用以下配置：

```bash
# 在内核配置文件中添加或确认以下选项
CONFIG_FUSE_FS=y          # FUSE 文件系统支持（VirtioFS 依赖）
CONFIG_VIRTIO_FS=y        # VirtioFS 驱动
CONFIG_OVERLAY_FS=y       # OverlayFS 支持
CONFIG_EXT4_FS=y          # ext4 文件系统
```

**使用项目提供的内核配置**：

项目在 `scripts/kbuild/microvm-kernel-ci-aarch64-6.10.config` 中提供了完整的内核配置。

```bash
# 使用 Docker 编译内核
cd scripts
./kbuild.sh

# 编译完成后，内核会输出到 dist/kernel.arm64
cp ../dist/kernel.arm64 ../resources/vm/kernel.bin
```

## 沙箱管理

沙箱（Sandbox）是 VM 中的隔离执行环境，每个沙箱有独立的 OverlayFS 层。每个 Session 关联一个沙箱，沙箱 ID 由 VM 端自动生成。

### 沙箱生命周期

```
创建沙箱 → 挂载工作空间 → 执行命令 → 卸载工作空间 → [清理/导出] → 删除沙箱
```

### 使用示例

```go
ctx := context.Background()

// 1. 创建沙箱（ID 由 VM 自动生成）
sandboxID, err := manager.CreateSandbox(ctx)
if err != nil {
    log.Fatal(err)
}
log.Printf("Created sandbox: %s", sandboxID)

// 2. 挂载工作空间（在执行命令前必须调用）
err = manager.MountSandbox(ctx, sandboxID)
if err != nil {
    log.Fatal(err)
}

// 3. 获取沙箱专用执行器并执行命令
executor := manager.GetSandboxExecutor(sandboxID)
stdout, stderr, exitCode, err := executor.ExecuteCommand(ctx, []string{"pwd"}, "/")
log.Printf("stdout: %s, stderr: %s, exit: %d", stdout, stderr, exitCode)

// 4. 卸载工作空间（执行后，以便修改 upper/work）
err = manager.UnmountSandbox(ctx, sandboxID)
if err != nil {
    log.Fatal(err)
}

// 5. 获取工作空间状态（文件差异）
state, err := manager.GetSandboxState(ctx, sandboxID)
if err != nil {
    log.Fatal(err)
}
for _, diff := range state.FileDiff {
    log.Printf("Changed: %s", diff.Path)
}

// 6. 应用差异到主机或丢弃
err = manager.ApplySandboxDiff(ctx, sandboxID, []string{"path/to/file"}, "/host/base")
// 或
err = manager.DiscardSandboxAllChanges(ctx, sandboxID)

// 7. 运行 Housekeeper 清理（可选，优化性能）
err = manager.RunSandboxHousekeeper(ctx, sandboxID)

// 8. 删除沙箱
err = manager.DeleteSandbox(ctx, sandboxID)
```

### 多沙箱并发

多个沙箱可以并发运行，每个沙箱有独立的 mount namespace：

```go
// 创建多个沙箱
sandboxID1, _ := manager.CreateSandbox(ctx)
sandboxID2, _ := manager.CreateSandbox(ctx)

// 并发执行（各自隔离）
go func() {
    manager.MountSandbox(ctx, sandboxID1)
    executor1 := manager.GetSandboxExecutor(sandboxID1)
    executor1.ExecuteCommand(ctx, []string{"npm", "install"}, "/project")
}()

go func() {
    manager.MountSandbox(ctx, sandboxID2)
    executor2 := manager.GetSandboxExecutor(sandboxID2)
    executor2.ExecuteCommand(ctx, []string{"pip", "install", "-r", "requirements.txt"}, "/project")
}()
```

## 共享目录与工作空间

### 多层 OverlayFS 结构

VM 通过 VirtioFS + 多层 OverlayFS 实现"沙箱隔离 / 输出可导出"：

```
主机侧 (macOS):
/Users/john/project           ← 用户工作目录
  │
  ├─ 通过 VirtioFS 共享（只读）
  │
VM 侧 (Linux):
  │
  ├─ /tmp/workspace-lower/mount-0/       ← VirtioFS 挂载点（只读）
  │  └─ project 完整内容
  │
  ├─ 沙箱 1:
  │  ├─ /mnt/temp-overlay/nora/sandboxes/<sandbox-id-1>/
  │  │  └─ overlay-0/
  │  │     ├─ upper/                     ← 修改和新文件
  │  │     ├─ work/                      ← OverlayFS 元数据
  │  │     └─ merged/                    ← 合并视图（挂载点）
  │  │
  │  └─ 通过 rbind 挂载到:
  │     └─ /Users/john/project           ← 与主机路径相同
  │
  └─ 沙箱 2:
     └─ 独立的 overlay-0（不同的 upper/work）
```

### OverlayFS 工作原理

```
对于每个 mount：
┌─────────────────────────────────────────┐
│        OverlayFS 合并视图                │
│  /Users/john/project (在沙箱中)          │
│  - 包含所有 lower + upper 的修改        │
└──────────────┬──────────────────────────┘
               │
        ┌──────┴──────┐
        ▼             ▼
┌──────────────┐  ┌──────────────┐
│  Upper 层    │  │  Lower 层    │
│              │  │              │
│ Sandbox upper│  │ VirtioFS    │
│ /mnt/.../    │  │ /tmp/...     │
│ upper/       │  │ workspace-   │
│              │  │ lower/mount-0│
│  新文件      │  │              │
│  修改的副本  │  │  原始只读    │
│              │  │  主机文件    │
└──────────────┘  └──────────────┘

xattr:
upper 中的条目可以有：
- trusted.overlay.opaque  (值='y') → 目录完全隐藏下层
- .wh.filename            → whiteout 标记（删除标记）
```

### 配置多个挂载

```go
config.Mounts = []vm.Mount{
    {
        HostPath: "/Users/john/project",
        VMPath:   "/Users/john/project",  // 保持路径一致
        ReadOnly: false,                   // 可写（使用 overlay）
    },
    {
        HostPath: "/Users/john/data",
        VMPath:   "/data",
        ReadOnly: true,                    // 只读（跳过 overlay）
    },
}
```

每个可写 mount 有独立的 OverlayFS：

- VirtioFS tag：`mount-0`, `mount-1`, ...
- 独立的 upper/work/merged 目录

### 注意事项

1. **内核支持要求**

   - 必须启用 `CONFIG_VIRTIO_FS=y`
   - 必须启用 `CONFIG_OVERLAY_FS=y`

2. **权限问题**

   - VirtioFS 共享的文件权限与主机一致
   - 确保 VM 内的用户（通常是 root）有权限访问

3. **性能考虑**

   - 读取操作直接从主机文件系统，性能较好
   - 写入和修改操作需要 Copy-on-Write，稍慢
   - 适合读多写少的场景

4. **临时性**
   - upper 层的修改在 VM 重启后会丢失
   - 如需保留修改，使用导出功能

## OverlayFS 差异分析与导出

### 差异分析器 (`vmd/overlay/diff.go`)

分析 upper 层相对于 lower 层的文件变更：

```go
type OverlayDiffAnalyzer struct {
    upperDir, lowerDir string              // 两层目录
    cacheDir string                        // 缓存目录（可共享）
    cfg OverlayDiffConfig                  // 配置
}

type OverlayDiffConfig struct {
    MoveSimilarityThreshold float64         // 0.8 (80%)
    BaseChunkSize int64                     // 64B，按等级倍增
    SmallFileThreshold int64                // 512B（字节比较）
}
```

**分析过程**：

1. **遍历 upper 层** - 收集所有条目
2. **检测 whiteout** - 识别删除操作（`.wh.*` 文件）
3. **计算文件哈希**
   - 小文件 (<512B)：完整字节比较
   - 大文件：分块哈希（64B, 128B, ...）
4. **检测移动** - 相似度匹配（>80%）
5. **识别更新** - 内容变化检测
6. **输出 FileDiff 列表**

### 差异类型

```go
type DiffType string

const (
    DiffTypeCreated  DiffType = "created"   // 新创建的文件
    DiffTypeModified DiffType = "modified"  // 修改的文件
    DiffTypeDeleted  DiffType = "deleted"   // 删除的文件
    DiffTypeMoved    DiffType = "moved"     // 移动/重命名的文件
)
```

### Housekeeper 清理

清理 overlay 中不必要的文件，提高性能：

```go
// 运行 Housekeeper
err := manager.RunSandboxHousekeeper(ctx, sandboxID)
```

**四步清理流程**：

1. **展开 opaque 目录**
   - 找到所有带 `trusted.overlay.opaque` 的目录
   - 为 lower 中存在但 upper 中不存在的条目创建 whiteout

2. **分析差异**
   - 再次调用 OverlayDiffAnalyzer
   - 现在可以安全地检测 opaque 目录内的相同文件

3. **移除相同文件**
   - 删除 upper 中与 lower 完全相同的文件
   - 节省空间，加速后续操作

4. **清理空目录**
   - 删除在 lower 中存在但为空的目录
   - 保留用户创建的新空目录

### 导出差异

将 upper 层的修改导出为 tar 流：

```go
// 获取差异列表
state, _ := manager.GetSandboxState(ctx, sandboxID)
for _, diff := range state.FileDiff {
    fmt.Printf("Changed: %s\n", diff.Path)
}

// 应用特定文件到主机
paths := []string{"src/main.go", "README.md"}
err := manager.ApplySandboxDiff(ctx, sandboxID, paths, "/host/project")

// 或丢弃所有变更
err = manager.DiscardSandboxAllChanges(ctx, sandboxID)
```

**导出内容**：

1. **新创建的文件** - 在 VM 中创建的所有新文件
2. **修改的文件** - Copy-on-Write 后的完整文件（不是差异）
3. **删除标记** - OverlayFS 的 `.wh.*` 文件

## 在代码中使用

### 基础使用示例

```go
package main

import (
    "context"
    "log"

    "github.com/nora/sandbox-vm/internal/platform/vm"
)

func main() {
    // 1. 创建 VM 配置
    config := vm.DefaultConfig("./resources")
    config.Mounts = []vm.Mount{
        {
            HostPath: "/Users/john/project",
            VMPath:   "/Users/john/project",
        },
    }

    // 2. 创建 VM 管理器
    manager, err := vm.NewManager(config)
    if err != nil {
        log.Fatal(err)
    }

    // 3. 启动 VM
    ctx := context.Background()
    if err := manager.Start(ctx); err != nil {
        log.Fatal(err)
    }
    defer manager.Stop(ctx)

    // 4. 创建沙箱（ID 由 VM 自动生成）
    sandboxID, err := manager.CreateSandbox(ctx)
    if err != nil {
        log.Fatal(err)
    }

    // 5. 挂载工作空间
    if err := manager.MountSandbox(ctx, sandboxID); err != nil {
        log.Fatal(err)
    }

    // 6. 获取执行器并执行命令
    executor := manager.GetSandboxExecutor(sandboxID)
    stdout, stderr, exitCode, err := executor.ExecuteCommand(
        ctx,
        []string{"ls", "-la"},
        "/Users/john/project",
    )
    if err != nil {
        log.Fatal(err)
    }
    log.Printf("Exit: %d\nStdout:\n%s\nStderr:\n%s", exitCode, stdout, stderr)

    // 7. 卸载工作空间
    if err := manager.UnmountSandbox(ctx, sandboxID); err != nil {
        log.Fatal(err)
    }

    // 8. 查看变更
    state, _ := manager.GetSandboxState(ctx, sandboxID)
    for _, diff := range state.FileDiff {
        log.Printf("Changed: %s", diff.Path)
    }

    // 9. 删除沙箱
    if err := manager.DeleteSandbox(ctx, sandboxID); err != nil {
        log.Fatal(err)
    }
}
```

### 直接使用 Executor 接口

```go
// 执行命令
workDir := "/Users/john/project"
stdout, stderr, exitCode, err := executor.ExecuteCommand(
    ctx,
    []string{"echo", "Hello"},
    workDir,
)

// 读取文件
content, err := executor.ReadFile(ctx, workDir+"/test.txt")

// 写入文件
err = executor.WriteFile(ctx, workDir+"/test.txt", []byte("content"), false)

// 追加文件
err = executor.WriteFile(ctx, workDir+"/test.txt", []byte("more content"), true)

// 检查文件是否存在
exists, err := executor.FileExists(ctx, workDir+"/test.txt")

// 删除文件
err = executor.DeleteFile(ctx, workDir+"/test.txt")
```

## 网络与通信

### NAT 网络

VM 使用 NAT 网络模式，提供以下特性：

- VM 可以访问外部网络
- 主机可以通过 SSH 连接到 VM
- VM 与主机网络隔离

### Vsock 通信

主要通信通过 Vsock（Virtual Socket）进行，绕过网络栈，低延迟：

| 端口 | 用途 |
|------|------|
| 22 | SSH 连接和密钥传输 |
| 50051 | VM 侧 gRPC 服务（VMService） |
| 50052 | Host 侧 gRPC 服务（HostService） |

### gRPC 消息大小

本地 IPC 消息大小限制：64MB

### IP 地址解析（备用）

当需要通过网络连接时（非 Vsock），使用双重机制解析 VM 的 IP 地址：

1. **DHCP Leases（主要方式）**
   - 解析 macOS 的 DHCP leases 文件
   - 位置：`/var/db/dhcpd_leases`

2. **ARP Cache（备用方式）**
   - 通过 `arp -an` 命令查询
   - 匹配 VM 的 MAC 地址

### MAC 地址管理

- 可以在配置中指定固定的 MAC 地址
- 如果不指定，系统会自动生成本地管理的 MAC 地址
- 格式：`x2:xx:xx:xx:xx:xx`（第二位为 2，表示本地管理）

### 代理配置

VM 支持继承宿主机的代理设置：

```go
config.HTTPProxy = os.Getenv("HTTP_PROXY")
config.HTTPSProxy = os.Getenv("HTTPS_PROXY")
config.NoProxy = os.Getenv("NO_PROXY")
```

代理设置在 VM 启动时通过 `SetProxyEnv` gRPC 调用配置。

## 安全特性

### VM 隔离提供的保护

- **文件系统隔离**：
  - VM 无法访问主机文件系统（除非通过 VirtioFS 明确共享）
  - 共享目录以**只读**方式挂载，主机文件得到保护
  - VM 的修改存储在独立的 upper 层，不影响主机

- **沙箱隔离**：
  - 每个沙箱有独立的 mount namespace
  - 沙箱之间的文件修改互不影响

- **网络隔离**：使用 NAT，VM 只能访问允许的网络

- **资源限制**：CPU 和内存受限

- **进程隔离**：VM 内的进程无法影响主机

- **SSH 密钥认证**：使用 Ed25519 密钥，每次启动自动生成

- **Copy-on-Write 保护**：
  - 修改主机文件时触发 Copy-on-Write
  - 原始文件始终保持不变
  - VM 只能修改自己的副本

### 最佳实践

1. **谨慎共享目录**：
   - 只共享必要的目录
   - 避免共享敏感信息（如密钥、配置文件）
   - 即使是只读共享，也要注意数据泄露风险

2. **使用只读挂载**：对于不需要修改的数据目录

3. **定期清理沙箱**：删除不再需要的沙箱释放资源

4. **最小权限原则**：限制 VM 内的操作权限

5. **监控资源使用**：防止资源耗尽

6. **导出前检查**：
   - 导出前检查 upper 层内容
   - 避免导出意外的敏感数据
   - 使用差异分析确认变更

## 性能优化

### TRIM 支持

临时 overlay 磁盘支持 TRIM 命令，允许 macOS 回收未使用的磁盘空间：

```go
// VM init 中使用 discard 挂载选项
syscall.Mount(device, mountpoint, "ext4", 0, "discard")
```

这对于频繁创建和删除文件的场景非常有效。

### 启动时间优化

1. **使用轻量级发行版**
   - Alpine Linux（推荐，启动最快）
   - Tiny Core Linux
   - BusyBox-based 系统

2. **内核压缩**
   - 保持内核为 gzip 压缩格式可节省磁盘空间
   - 程序会自动处理解压

3. **预热 VM**
   - 提前启动 VM 并保持运行
   - 复用 gRPC 连接

### 资源配置建议

```go
// 轻量级任务（文件操作、简单命令）
config.CPUCount = 1
config.MemorySize = 512 * 1024 * 1024  // 512MB

// 中等任务（编译、测试）
config.CPUCount = 2
config.MemorySize = 1 * 1024 * 1024 * 1024  // 1GB

// 重量级任务（大型编译、数据处理）
config.CPUCount = 4
config.MemorySize = 4 * 1024 * 1024 * 1024  // 4GB
```

### Housekeeper 优化

定期运行 Housekeeper 清理 overlay：

```go
// 在卸载工作空间后运行
manager.UnmountSandbox(ctx, sandboxID)
manager.RunSandboxHousekeeper(ctx, sandboxID)
```

这会：
- 移除与 lower 层相同的文件
- 清理空目录
- 减少后续差异分析的开销

### I/O 优化

- 差异分析器使用 128KB 固定读缓冲区
- 文件哈希缓存减少重复计算
- 流式 tar 导出减少内存占用

### 配置 DHCP 租约

如果需要频繁启动和停止 VM，建议配置更短的 DHCP 租约时间：

```bash
sudo ./scripts/configure-dhcp-lease.sh
```

这会将租约时间从默认的 1 天缩短为 10 分钟，防止 IP 池耗尽。

## 故障排查

### 1. 内核相关问题

**症状**：内核文件加载失败

```bash
# 检查内核文件是否存在
ls -l resources/vm/kernel.bin

# 检查文件权限
chmod 644 resources/vm/kernel.bin

# 测试是否为 gzip 压缩
file resources/vm/kernel.bin
```

**解决方案**：
- 确保内核文件存在且可读
- 支持 gzip 压缩和未压缩的内核
- 程序会自动检测并处理

### 2. VM 启动失败

**症状**：VM 无法启动或立即退出

```bash
# 检查 rootfs 镜像
ls -l resources/vm/rootfs.img

# 尝试挂载检查
sudo mount -o loop resources/vm/rootfs.img /mnt
ls -la /mnt
sudo umount /mnt
```

**常见原因**：
- rootfs.img 损坏或格式错误
- 磁盘镜像内没有有效的 Linux 系统
- 启动参数不正确

### 3. gRPC 连接失败

**症状**：VM 启动成功但 gRPC 连接失败

**诊断步骤**：
- 检查 vmd daemon 是否正常启动
- 查看 VM 控制台输出
- 确认 Vsock 端口配置正确

**常见原因**：
- vmd daemon 未包含在 rootfs 中
- vmd 启动时崩溃
- Vsock 端口冲突

### 4. SSH 连接失败

**症状**：无法建立 SSH 连接

```bash
# 检查 SSH 密钥
ls -l /tmp/nora-vm-key*

# 手动测试 SSH 连接（如果知道 IP）
ssh -i /tmp/nora-vm-key -o StrictHostKeyChecking=no root@192.168.64.x
```

**常见原因**：
- VM 内的 SSH 服务未启动
- SSH 配置不允许 root 登录
- 公钥未正确部署

### 5. OverlayFS 问题

**症状**：文件修改不可见或挂载失败

**诊断步骤**：
```bash
# 在 VM 中检查挂载
mount | grep overlay

# 检查 upper/work 目录权限
ls -la /mnt/temp-overlay/nora/sandboxes/
```

**常见原因**：
- 内核不支持 OverlayFS
- upper/work 目录不在同一文件系统
- 权限问题

### 6. 命令执行超时

**症状**：长时间运行的命令被中断

**解决方案**：

```go
// 使用自定义超时
ctx, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
defer cancel()

stdout, _, _, _ := executor.ExecuteCommand(ctx, []string{"long-running-command"}, "/")
```

### 查看详细日志

程序会输出详细的日志，包括：

- VM 配置信息
- 内核解压过程
- VM 启动状态
- gRPC 连接状态
- SSH 连接尝试
- 命令执行结果

建议在调试时查看完整的日志输出。

## 未来改进

### 已完成 ✅

- [x] VirtioFS 共享目录支持
- [x] OverlayFS 可写工作空间
- [x] 工作空间导出功能
- [x] 内核自动检测和解压
- [x] Copy-on-Write 文件保护
- [x] gRPC 双向通信（替代纯 SSH）
- [x] 多目录挂载支持
- [x] 沙箱管理和隔离
- [x] TRIM 支持（磁盘空间回收）
- [x] OverlayFS 差异分析器
- [x] Housekeeper 清理功能
- [x] 代理配置继承

### 计划中

- [ ] 支持多个并发 VM 实例
- [ ] VM 快照和恢复功能
- [ ] 更细粒度的资源限制（CPU、磁盘 I/O）
- [ ] 网络流量监控和限制
- [ ] 自动清理和回收机制
- [ ] 支持其他虚拟化技术（QEMU、KVM）
- [ ] 支持 Windows 主机
- [ ] VM 池管理（预热、复用）
- [ ] Cloud-init 支持
- [ ] 更多的镜像模板
- [ ] 增量导出（只导出差异）
- [ ] 双向文件同步
- [ ] 可配置的共享目录读写权限

## 参考资料

### 项目文档

- [scripts/README.md](../scripts/README.md) - 脚本使用说明
- [NETWORK-IMPROVEMENTS.md](NETWORK-IMPROVEMENTS.md) - 网络改进技术文档

### 外部资源

- [Code-Hex/vz](https://github.com/Code-Hex/vz) - Go bindings for macOS Virtualization Framework
- [Apple Virtualization Framework](https://developer.apple.com/documentation/virtualization) - 官方文档
- [VirtioFS](https://virtio-fs.gitlab.io/) - VirtioFS 项目主页
- [OverlayFS](https://www.kernel.org/doc/html/latest/filesystems/overlayfs.html) - Linux 内核 OverlayFS 文档
- [golang.org/x/crypto/ssh](https://pkg.go.dev/golang.org/x/crypto/ssh) - SSH library for Go
- [macOS DHCP](https://www.unix.com/man-page/osx/8/bootpd/) - macOS DHCP server documentation

### 相关技术

- [Copy-on-Write](https://en.wikipedia.org/wiki/Copy-on-write) - Copy-on-Write 机制
- [Union mount](https://en.wikipedia.org/wiki/Union_mount) - 联合挂载文件系统
- [Virtio](https://wiki.libvirt.org/page/Virtio) - 虚拟化 I/O 标准
- [Vsock](https://man7.org/linux/man-pages/man7/vsock.7.html) - Virtual Socket 通信

## 贡献

如果你在使用过程中发现问题或有改进建议，欢迎提交 Issue 或 Pull Request。
