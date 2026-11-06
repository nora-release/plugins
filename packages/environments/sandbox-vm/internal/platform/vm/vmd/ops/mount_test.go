//go:build linux

package ops

import "testing"

func TestIsMountPointFromMountInfoDetectsBindMountOnSameDevice(t *testing.T) {
	t.Parallel()

	mountInfo := []byte(`36 25 0:32 / / rw,relatime - ext4 /dev/vda1 rw
57 36 0:32 / /root/.local/state/bridge/sandboxes/abc/root rw,relatime - ext4 /dev/vda1 rw
`)

	if !isMountPointFromMountInfo("/root/.local/state/bridge/sandboxes/abc/root", mountInfo) {
		t.Fatal("expected bind mount to be detected from mountinfo")
	}
}

func TestIsMountPointFromMountInfoUnescapesPath(t *testing.T) {
	t.Parallel()

	mountInfo := []byte(`36 25 0:32 / / rw,relatime - ext4 /dev/vda1 rw
58 36 0:32 / /tmp/with\040space rw,relatime - ext4 /dev/vda1 rw
`)

	if !isMountPointFromMountInfo("/tmp/with space", mountInfo) {
		t.Fatal("expected escaped mount path to match")
	}
}
