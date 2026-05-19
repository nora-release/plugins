# Sandbox VM

`packages/sandbox` 是 Nora 使用的纯 sandbox VM 控制库。Swift 侧负责 agent SDK、工具定义、模型调用和产品逻辑；Go 侧只负责启动 VM、创建 sandbox、执行命令、文件 IO、workspace diff、accept/discard 和 guest `vmd` 控制面。

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
cd packages/sandbox

make framework      # build the local-vm runtime artifact (current filename: dist/SandboxVM.xcframework)
make vm             # 构建 resources/vm/{kernel.bin,rootfs.img}
make proto          # vmrpc proto 变更后再执行
make go-test        # Go 单元测试
```

## 相关文档

- `docs/VM-GUIDE.md`
- `docs/TROUBLESHOOTING_SSH.md`
- `docs/VSCODE_REMOTE_SSH.md`
