# VM SSH 连接故障排查指南

## 问题现象
尝试通过 `ssh -i /tmp/nora-vm-key root@<VM_IP>` 连接到VM时出现 "Permission denied" 或连接超时。

## 可能的原因

### 1. VM未运行或SSH服务未启动
**症状**: `nc -zv <VM_IP> 22` 显示连接被拒绝或超时

**解决方案**:
```bash
# 重新构建 VM assets，并通过应用侧 sandbox runtime 启动 VM
make vm

# 从控制台输出查看:
# - VM是否成功启动
# - 网络是否配置 (看到 "inet 192.168.64.x")
# - SSH是否启动 (看到 "✓ sshd running")
```

### 2. SSH公钥未正确安装到rootfs
**症状**: SSH端口可达，但认证失败

**原因**: rootfs.img 中的 `/root/.ssh/authorized_keys` 文件不存在或内容不匹配

**解决方案**:
```bash
# 1. 确保SSH密钥存在
ls -la /tmp/nora-vm-key*

# 2. 查看公钥内容
cat /tmp/nora-vm-key.pub

# 3. 重建rootfs并安装公钥
./scripts/rebuild-rootfs-with-ssh.sh resources/vm/rootfs.img /tmp/nora-vm-key.pub

# 4. 验证安装成功（查看输出中应该有）:
#    ✓ SSH 公钥已安装
#    ✓ authorized_keys 已安装
```

### 3. SSH密钥权限问题
**症状**: SSH提示密钥权限过于开放

**解决方案**:
```bash
chmod 600 /tmp/nora-vm-key
chmod 644 /tmp/nora-vm-key.pub
```

### 4. VM Manager生成的密钥与手动生成的密钥不匹配
**症状**: rootfs中安装的是一个公钥，但使用的私钥是另一个

**解决方案**: 统一使用同一对密钥

```bash
# 删除旧密钥
rm -f /tmp/nora-vm-key*

# 重新生成
ssh-keygen -t ed25519 -f /tmp/nora-vm-key -N "" -C "nora-vm"

# 重建rootfs
./scripts/rebuild-rootfs-with-ssh.sh resources/vm/rootfs.img /tmp/nora-vm-key.pub

# 重建 VM assets 后通过应用侧 sandbox runtime 重启 VM
make vm
```

## 完整的测试流程

### 步骤1: 准备SSH密钥
```bash
# 生成新的SSH密钥对
ssh-keygen -t ed25519 -f /tmp/nora-vm-key -N "" -C "nora-vm"

# 验证密钥
cat /tmp/nora-vm-key.pub
```

### 步骤2: 重建rootfs
```bash
./scripts/rebuild-rootfs-with-ssh.sh resources/vm/rootfs.img /tmp/nora-vm-key.pub
```

**期望输出应包含**:
- `✓ sshd 已安装`
- `✓ SSH 公钥已安装`
- `✓ authorized_keys 已安装`

### 步骤3: 启动VM
```bash
make vm
```

**从控制台输出中查找**:
```
==> Network:
    inet 192.168.64.X/24 scope global eth0
==> Generating SSH keys...
==> Starting sshd...
✓ sshd running (PID: 77)
==> System ready
```

记下IP地址（例如 192.168.64.5）

### 步骤4: 测试SSH连接
```bash
# 测试端口
nc -zv 192.168.64.5 22

# 连接SSH
ssh -i /tmp/nora-vm-key -o StrictHostKeyChecking=no root@192.168.64.5

# 如果成功，应该看到Alpine Linux提示符
```

## 调试命令

### 查看详细SSH连接日志
```bash
ssh -vvv -i /tmp/nora-vm-key root@192.168.64.5 2>&1 | less
```

关键日志行:
- `debug1: Offering public key` - 提供公钥
- `debug1: Server accepts key` - 服务器接受密钥
- `debug1: Authentication succeeded` - 认证成功
- `Permission denied` - 认证失败

### 从VM内部检查（如果可以访问控制台）

如果你能看到VM的控制台输出，可以检查:

1. SSH服务状态:
```bash
ps aux | grep sshd
```

2. 查看authorized_keys:
```bash
cat /root/.ssh/authorized_keys
ls -la /root/.ssh/
```

3. 查看SSH配置:
```bash
cat /etc/ssh/sshd_config | grep -E "PermitRootLogin|PubkeyAuthentication|PasswordAuthentication"
```

4. 查看SSH日志:
```bash
tail -f /var/log/messages  # 或者
dmesg | tail -20
```

## 常见错误及解决方法

### 错误: "Permission denied (publickey)"
- 公钥未安装或不匹配
- 解决: 重建rootfs并确保公钥正确安装

### 错误: "Connection timed out"
- VM未运行或网络未配置
- 解决: 检查VM是否成功启动并获得IP

### 错误: "Connection refused"
- SSH服务未启动
- 解决: 检查VM控制台是否显示 "✓ sshd running"

### 错误: "Host key verification failed"
- Host key变化
- 解决: 使用 `-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null`

## 当前实现的限制

1. **NAT网络IP**: VM通过NAT获得 192.168.64.x 范围的IP，每次启动可能不同
2. **Serial Console**: 目前通过serial console查看VM输出，无法交互
3. **密钥管理**: 需要手动确保密钥一致性

## 下一步改进

1. 添加自动IP发现机制
2. 实现端口转发（localhost:2222 -> VM:22）
3. 添加VM快照功能，避免每次重建rootfs
