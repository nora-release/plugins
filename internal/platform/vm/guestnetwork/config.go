package guestnetwork

import "net"

const (
	HostLoopbackIP = "100.64.0.1"
	GuestCIDR      = "100.64.0.2/30"
)

func ResolveHostDialIP(ip net.IP) net.IP {
	if ip == nil {
		return nil
	}
	if ip.Equal(net.ParseIP(HostLoopbackIP)) {
		return net.IPv4(127, 0, 0, 1)
	}
	return ip
}
