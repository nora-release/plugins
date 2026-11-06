# VM 管理的网络改进

本文档描述了为解决 IP 地址解析和 DHCP 租约耗尽问题而实现的网络改进。

## 解决的问题

### 1. 硬编码的 IP 地址猜测

**改进前**: VM 管理器尝试通过遍历硬编码的 IP 列表 (192.168.64.20-24) 来猜测 VM 的 IP 地址。

**改进后**: 现在通过 VM 的 MAC 地址动态解析 IP 地址:
- DHCP leases 文件解析 (主要方法)
- ARP 缓存查询 (备用方法)

### 2. DHCP IP 池耗尽

**改进前**: macOS DHCP 服务器默认分配 86,400 秒 (1 天) 的租约时间,运行多个 VM 时会导致 IP 池耗尽。

**改进后**: 可以使用提供的配置脚本将租约时间缩短至 600 秒 (10 分钟)。

### 3. 不确定的网络配置

**改进前**: VM 获得随机的 MAC 地址,无法预测或跟踪 IP 分配。

**改进后**: VM 现在使用固定的 MAC 地址 (在配置中指定或一致性生成),允许可靠的 IP 解析。

## 实现细节

### 1. MAC 地址管理

每个 VM 现在在配置中都有一个固定的 MAC 地址:

```go
type Config struct {
    // ...
    MACAddress string // VM 的固定 MAC 地址
    // ...
}
```

MAC 地址的特点:
- 使用 `ensureMACAddress()` 生成一次 (本地管理的 MAC 地址)
- 在 VM 创建时设置到网络设备上
- 用于通过 DHCP leases 和 ARP 解析 IP

### 2. DHCP Lease 文件解析器

位置: `internal/network/dhcp_leases.go`

解析 `/var/db/dhcpd_leases` 文件,将 MAC 地址映射到 IP 地址:

```go
// 解析 DHCP leases 文件
leases, err := network.ParseDefaultDHCPLeases()
ip, found := leases.GetIPByMAC(macAddress)
```

功能特性:
- 处理 macOS DHCP lease 文件格式
- 过滤过期的租约
- 如果存在重复租约,优先使用较新的
- 通过 MAC 地址进行高效的 O(1) 查找

### 3. ARP 解析器 (备用方案)

位置: `internal/network/arp_resolver.go`

当 DHCP leases 不可用时,使用系统 ARP 缓存:

```go
arpResolver, err := network.NewARPResolver()
ip, err := arpResolver.GetIPByMAC(macAddress)
```

适用场景:
- DHCP lease 文件尚未更新
- VM 使用不同的 DHCP 配置
- 使用桥接网络进行测试

### 4. IP 解析策略

`connectSSH()` 函数现在的工作流程:

1. 等待 VM 启动并获取网络配置 (3 秒延迟)
2. 尝试使用 MAC 地址解析 IP:
   - 首先: 解析 DHCP leases 文件
   - 备用: 查询 ARP 缓存
   - 最多重试 30 次,间隔 2 秒
3. 找到 IP 后,通过 SSH 连接
4. 记录解析的 IP 以便调试

```go
log.Printf("Resolving IP address for MAC: %s", m.macAddress.String())
// 先尝试 DHCP,然后是 ARP
log.Printf("Resolved IP from DHCP: %s", vmIP.String())
log.Printf("Connecting to SSH at %s...", vmIP.String())
```

## 配置

### 减少 DHCP 租约时间

为了防止 IP 耗尽,配置更短的 DHCP 租约:

```bash
sudo ./scripts/configure-dhcp-lease.sh
```

这会将租约时间设置为 600 秒 (10 分钟)。你可能需要重启网络或重启系统才能使更改生效。

### 指定自定义 MAC 地址

在你的 VM 配置中:

```go
config := vm.DefaultConfig(resourcesDir)
config.MACAddress = "52:54:00:12:34:56" // 可选的自定义 MAC
```

如果未指定,将自动生成一个随机的本地管理 MAC 地址。

## 优势

1. **可靠**: 不再猜测 IP 地址 - 基于 MAC 地址解析
2. **高效**: 不会浪费时间尝试多个错误的 IP
3. **可扩展**: 缩短的 DHCP 租约时间防止 IP 耗尽
4. **可调试**: 清晰记录 MAC 地址和解析的 IP
5. **鲁棒**: 当 DHCP leases 不可用时回退到 ARP
6. **可维护**: 无需更新硬编码的 IP 范围

## 与 Tart 的对比

本实现基于 [Tart 的方案](https://tart.run/faq/):

| 功能 | Tart | 本实现 |
|------|------|--------|
| DHCP Lease 解析 | ✓ | ✓ |
| ARP 解析 | ✓ | ✓ |
| 固定 MAC 地址 | ✓ | ✓ |
| Guest Agent | ✓ | ✗ (不需要) |
| DHCP Lease 配置 | 文档说明 | 脚本自动化 |

## 测试

测试新的 IP 解析功能:

```bash
make go-test
```

**注意**: 对于生产部署,你可能需要使用 Apple Developer 证书签名二进制文件。参见 [entitlements.plist](../entitlements.plist) 了解所需的权限。

你应该看到类似以下的输出:
```
VM MAC address: 52:54:00:12:34:56
Resolving IP address for MAC: 52:54:00:12:34:56
Resolved IP from DHCP: 192.168.64.25
Connecting to SSH at 192.168.64.25...
Successfully connected to VM at 192.168.64.25
```

## 未来改进

1. **持久化 MAC 存储**: 将生成的 MAC 地址保存到配置文件,使同一个 VM 在重启后始终获得相同的 MAC
2. **静态 IP 分配**: 配置 DHCP 为特定 MAC 始终分配相同的 IP (DHCP 预留)
3. **多 VM 管理**: 跟踪和管理多个具有不同 MAC 地址的并发 VM
4. **网络命名空间隔离**: 使用独立的网络命名空间实现更好的隔离 (类似 Tart 的 softnet)
5. **IPv6 支持**: 添加 IPv6 地址解析支持
6. **桥接网络**: 除 NAT 外还支持桥接模式

## 相关文档

- [VM-GUIDE.md](VM-GUIDE.md) - VM 隔离功能的完整用户指南
- [TOOLS.md](TOOLS.md) - 可用工具的文档 (bash、file 等)
- [configure-dhcp-lease.sh](../scripts/configure-dhcp-lease.sh) - 配置 DHCP 租约时间的脚本

## 参考资料

- [Tart FAQ - DHCP 配置](https://tart.run/faq/#connecting-to-a-service-running-on-host)
- [Tart GitHub 仓库](https://github.com/cirruslabs/tart)
- [macOS Virtualization Framework](https://developer.apple.com/documentation/virtualization)
