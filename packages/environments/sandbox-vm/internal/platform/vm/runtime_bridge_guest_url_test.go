package vm

import (
	"testing"

	"github.com/nora/sandbox-vm/internal/platform/vm/guestnetwork"
)

func TestGuestReachableURLRewritesLoopbackHosts(t *testing.T) {
	tests := []struct {
		name string
		in   string
		want string
	}{
		{
			name: "localhost with explicit port",
			in:   "http://localhost:8080",
			want: "http://" + guestnetwork.HostLoopbackIP + ":8080",
		},
		{
			name: "loopback ip with explicit port",
			in:   "http://127.0.0.1:8080",
			want: "http://" + guestnetwork.HostLoopbackIP + ":8080",
		},
		{
			name: "ipv6 loopback with explicit port",
			in:   "http://[::1]:8080",
			want: "http://" + guestnetwork.HostLoopbackIP + ":8080",
		},
		{
			name: "localhost with default port",
			in:   "https://localhost/path",
			want: "https://" + guestnetwork.HostLoopbackIP + ":443/path",
		},
		{
			name: "non-loopback host unchanged",
			in:   "http://192.168.1.20:8080",
			want: "http://192.168.1.20:8080",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := guestReachableURL(tc.in); got != tc.want {
				t.Fatalf("guestReachableURL(%q) = %q, want %q", tc.in, got, tc.want)
			}
		})
	}
}
