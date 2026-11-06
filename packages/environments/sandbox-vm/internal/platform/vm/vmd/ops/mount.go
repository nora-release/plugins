//go:build linux

package ops

import (
	"bufio"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
)

// systemBinds are the system directories to mount into the overlay.
var systemBinds = []string{"proc", "sys", "dev"}

var mountInfoPathUnescaper = strings.NewReplacer(
	`\040`, " ",
	`\011`, "\t",
	`\012`, "\n",
	`\134`, `\`,
)

// IsMountPoint checks if the given path is a mount point.
// /proc/self/mountinfo is authoritative and correctly handles bind mounts that
// reuse the same backing device as their parent. The device-ID fallback keeps
// the old behavior available if mountinfo cannot be read.
func IsMountPoint(path string) bool {
	path = filepath.Clean(path)

	if mountInfo, err := os.ReadFile("/proc/self/mountinfo"); err == nil {
		if isMountPointFromMountInfo(path, mountInfo) {
			return true
		}
	}

	pathStat, err := os.Stat(path)
	if err != nil {
		return false
	}
	parentStat, err := os.Stat(filepath.Dir(path))
	if err != nil {
		return false
	}
	pathSys, ok1 := pathStat.Sys().(*syscall.Stat_t)
	parentSys, ok2 := parentStat.Sys().(*syscall.Stat_t)
	if !ok1 || !ok2 {
		return false
	}
	return pathSys.Dev != parentSys.Dev
}

func isMountPointFromMountInfo(path string, mountInfo []byte) bool {
	for _, line := range strings.Split(string(mountInfo), "\n") {
		if strings.TrimSpace(line) == "" {
			continue
		}

		fields := strings.Fields(line)
		if len(fields) < 5 {
			continue
		}

		mountPoint := mountInfoPathUnescaper.Replace(fields[4])
		if filepath.Clean(mountPoint) == path {
			return true
		}
	}

	return false
}

// EnsureOverlaySupport ensures the overlay filesystem is available.
func EnsureOverlaySupport() error {
	data, err := os.ReadFile("/proc/filesystems")
	if err != nil {
		return fmt.Errorf("failed to read /proc/filesystems: %w", err)
	}
	if strings.Contains(string(data), "overlay") {
		return nil
	}

	// Try to load the overlay module
	cmd := exec.Command("modprobe", "overlay")
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("overlay filesystem not supported by this kernel")
	}
	return nil
}

// MountOverlay mounts an overlay filesystem.
func MountOverlay(lowerDir, upperDir, workDir, mergedDir string) error {
	options := fmt.Sprintf("lowerdir=%s,upperdir=%s,workdir=%s,redirect_dir=on,metacopy=on", lowerDir, upperDir, workDir)
	return syscall.Mount("overlay", mergedDir, "overlay", 0, options)
}

// MountBind performs a non-recursive bind mount.
func MountBind(source, target string) error {
	return syscall.Mount(source, target, "", syscall.MS_BIND, "")
}

// MountRBind performs a recursive bind mount.
func MountRBind(source, target string) error {
	return syscall.Mount(source, target, "", syscall.MS_BIND|syscall.MS_REC, "")
}

// MakeRSlave makes a mount point a recursive slave.
func MakeRSlave(target string) error {
	return syscall.Mount("", target, "", syscall.MS_SLAVE|syscall.MS_REC, "")
}

// Unmount performs a normal unmount.
func Unmount(target string) error {
	return syscall.Unmount(target, 0)
}

// BindSystemDirs binds system directories into the target.
func BindSystemDirs(targetRoot string) error {
	for _, dir := range systemBinds {
		source := "/" + dir
		target := targetRoot + "/" + dir

		if _, err := os.Stat(source); os.IsNotExist(err) {
			continue
		}

		if err := os.MkdirAll(target, 0755); err != nil {
			return fmt.Errorf("failed to create %s: %w", target, err)
		}

		if IsMountPoint(target) {
			continue
		}

		if err := MountRBind(source, target); err != nil {
			return fmt.Errorf("failed to bind mount %s: %w", dir, err)
		}

		// Best effort to make it a slave
		_ = MakeRSlave(target)
	}
	return nil
}

// UnbindSystemDirs unmounts system directories from the target.
// Uses MNT_DETACH (lazy unmount) because recursive bind mounts of /proc, /sys, /dev
// contain many sub-mounts that prevent a normal unmount.
func UnbindSystemDirs(targetRoot string) {
	for _, dir := range systemBinds {
		target := targetRoot + "/" + dir
		if IsMountPoint(target) {
			_ = syscall.Unmount(target, syscall.MNT_DETACH)
		}
	}
}

// GetMountInfo returns mount information for debugging.
func GetMountInfo() ([]string, error) {
	file, err := os.Open("/proc/mounts")
	if err != nil {
		return nil, err
	}
	defer file.Close()

	var mounts []string
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := scanner.Text()
		if strings.Contains(line, "overlay") || strings.Contains(line, "workspace") {
			mounts = append(mounts, line)
		}
	}
	return mounts, scanner.Err()
}
