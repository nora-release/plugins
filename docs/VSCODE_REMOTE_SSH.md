# 使用 VSCode Remote SSH 连接 VM

本文档说明如何通过 VSCode 的 Remote - SSH 扩展连接到 VM，以便在 VM 环境中进行开发和调试。

## 自动配置（推荐）

### 使用 Nora App

在 **DEBUG 构建**或**启用 debug mode** 时， Nora app 会自动配置 SSH config。

### 连接到 VM

1. 查看日志中的 SSH host 名称（如 `nora-vm-abc123`）
2. 在 VSCode 中按 `Cmd+Shift+P` → "Remote-SSH: Connect to Host"
3. 选择显示的 host 名称
4. 等待连接建立

连接成功后，VSCode 左下角会显示 "SSH: nora-vm-xxx"。
