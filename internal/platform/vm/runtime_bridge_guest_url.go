package vm

import (
	"net"
	"net/url"
	"strings"

	"github.com/nora/sandbox-vm/internal/platform/vm/guestnetwork"
)

func guestReachableURL(raw string) string {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return ""
	}

	parsed, err := url.Parse(raw)
	if err != nil || parsed.Host == "" {
		return raw
	}

	host := strings.TrimSpace(parsed.Hostname())
	if !isHostLoopbackName(host) {
		return raw
	}

	port := parsed.Port()
	if port == "" {
		port = defaultPortForScheme(parsed.Scheme)
	}
	if port == "" {
		return raw
	}

	parsed.Host = net.JoinHostPort(guestnetwork.HostLoopbackIP, port)
	return parsed.String()
}

func isHostLoopbackName(host string) bool {
	if host == "" {
		return false
	}
	host = strings.TrimSpace(strings.Trim(host, "[]"))
	switch strings.ToLower(host) {
	case "localhost", "::1":
		return true
	}
	ip := net.ParseIP(host)
	return ip != nil && ip.IsLoopback()
}

func defaultPortForScheme(scheme string) string {
	switch strings.ToLower(strings.TrimSpace(scheme)) {
	case "http":
		return "80"
	case "https":
		return "443"
	default:
		return ""
	}
}
