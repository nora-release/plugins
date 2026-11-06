# Sandbox VM

沙盒 VM 控制库：启动 VM、创建 sandbox、执行命令、文件 IO、workspace diff、accept/discard 和 guest `vmd` 控制面。

## 状态

Nora 从 `nora-release/nora` 的 #97 起不再内置这个 VM，agent 默认以 Full Access 直接在 Mac 上运行。代码连同历史搬到这里，目标是做成一个**可选的环境插件**，需要隔离执行的用户自己安装。

现在它还**不是**可安装的插件：Nora 还没有"执行环境"类插件的接口。下面这些都还没做：

- Nora 侧的环境插件接口：插件提供一个 `environment` 别名（比如 `sandbox`），agent 的文件和命令工具按别名路由过去，不装插件时只有 `local`。
- 插件形态：签名的 `SandboxVM.framework` + `kernel.bin` / `rootfs.img`，安装到插件目录，由 Nora 按需加载。
- 变更审核：插件自己提供 accept/discard 的界面或命令，而不是写死在 Nora 的时间线里。

移除前 Nora 里的接入代码可以在 `nora-release/nora` 的提交 `ccee6a1ff` 找到：

- `macos/Nora/Agent/LocalRuntime/EmbeddedVMRuntimeBridge.swift`：调用本 framework 的 Swift 桥
- `macos/Nora/Agent/LocalRuntime/LocalAgentSession+Workspace.swift`：workspace 审核、accept/discard
- `macos/Nora/Backend/SettingsManager/Models/LocalVMMount.swift`：默认挂载
- `macos/DevKit/Scripts/sync-sandbox-vm.sh`：把 framework 和镜像放进 App

下文是搬迁前的说明，路径里的 `packages/sandbox` 现在对应 `packages/environments/sandbox-vm`；`scripts/build-framework.sh` 与 `scripts/build-vm.sh` 里复制到 `macos/` 的步骤在这个仓库里会自动跳过。

## 当前职责

- `sdk/apple`
  - 导出本地 VM runtime framework，给 macOS `Nora.app` 使用
- `internal/localconnector`
  - 管理 local VM session runtime、overlay、apply/discard
- `internal/envhost`
  - 当前只保留 `sandbox` 执行宿主
- `internal/framework/runtimebridge`
  - execution-scoped runtime callback bridge
- `internal/platform/vm`
  - Virtualization、vsock、overlay、guest `vmd`
- `cmd/vmd`
  - guest daemon

## 当前结构

```text
macos (Swift)
  └─ local-vm runtime framework (sdk/apple/)
       ├─ internal/localconnector   local VM runtime ownership
       ├─ internal/envhost/         sandbox host + runtime bridge server
       ├─ internal/framework/
       │   └─ runtimebridge/        capability bridge + local callbacks
       └─ internal/platform/vm/     VM + vsock + guest daemon
```

## 命令入口

当前保留的 `cmd/`：

- `cmd/vmd`

`agent-cli`、`slack-agentd`、`meetd`、`gcal-auth`、meeting joiner、prompt/history agent 类型都不属于这个库。

## 构建与测试

```bash
cd packages/environments/sandbox-vm

make framework      # build the local-vm runtime artifact (current filename: dist/SandboxVM.xcframework)
make vm             # 构建 resources/vm/{kernel.bin,rootfs.img}
make proto          # vmrpc proto 变更后再执行
make go-test        # Go 单元测试
```

## 相关文档

- `docs/VM-GUIDE.md`
- `docs/TROUBLESHOOTING_SSH.md`
- `docs/VSCODE_REMOTE_SSH.md`
