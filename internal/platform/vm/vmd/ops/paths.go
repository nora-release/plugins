//go:build linux

// Package ops implements sandbox filesystem operations.
// This package is only meant to run inside the VM (Linux).
package ops

import (
	"os"
	"path/filepath"
)

const (
	// WorkspaceLowerBase is the base directory for VirtioFS mount points (lower layers from host)
	// Each mount-N tag is mounted at WorkspaceLowerBase/mount-N
	WorkspaceLowerBase = "/tmp/workspace-lower"
)

// MountConfig describes a single mount configuration.
type MountConfig struct {
	VirtioTag   string // VirtioFS tag (e.g., "mount-0", "mount-1")
	MountPath   string // Final merged mount path (e.g., "/Users/john")
	ReadOnly    bool   // If true, skip overlay (read-only mount)
	Passthrough bool   // If true, direct write-through to host, no overlay
}

// GetVirtioFSMountPoint returns the VirtioFS mount point for a given tag.
func GetVirtioFSMountPoint(virtioTag string) string {
	return filepath.Join(WorkspaceLowerBase, virtioTag)
}

// PathConfig holds the resolved paths for sandbox operations.
type PathConfig struct {
	SandboxesRoot string
}

// DefaultPathConfig returns the path config by probing available directories.
func DefaultPathConfig() (*PathConfig, error) {
	sandboxRoot := "/mnt/temp-overlay/nora/sandboxes"
	if os.Getenv("VMD_REMOTE") == "1" {
		sandboxRoot = "/root/.local/state/bridge/sandboxes"
	}

	return &PathConfig{
		SandboxesRoot: sandboxRoot,
	}, nil
}

// GetSandboxPath returns the base path for a sandbox.
func (c *PathConfig) GetSandboxPath(sandboxID string) string {
	return filepath.Join(c.SandboxesRoot, sandboxID)
}

// GetSandboxMerged returns the merged path for a sandbox.
func (c *PathConfig) GetSandboxMerged(sandboxID string) string {
	return filepath.Join(c.SandboxesRoot, sandboxID, "workspace-merged")
}

// GetSandboxRoot returns the sandbox root path (complete view: shared env + workspace).
func (c *PathConfig) GetSandboxRoot(sandboxID string) string {
	return filepath.Join(c.SandboxesRoot, sandboxID, "root")
}

// globalWorkspaceMounts stores the configured workspace mounts set during SetupWorkspaces.
var globalWorkspaceMounts []MountConfig

// SetGlobalWorkspaceMounts sets the global workspace mounts configuration.
// Called by WorkspaceManager.SetupAll() when workspaces are mounted.
func SetGlobalWorkspaceMounts(mounts []MountConfig) {
	globalWorkspaceMounts = mounts
}

// GetGlobalWorkspaceMounts returns the configured workspace mounts.
// Returns empty slice if workspaces are not set up yet.
func GetGlobalWorkspaceMounts() []MountConfig {
	return globalWorkspaceMounts
}

// GetGlobalWorkspaceMountPath returns the first workspace mount path for backward compatibility.
// Returns empty string if no workspaces are set up.
func GetGlobalWorkspaceMountPath() string {
	if len(globalWorkspaceMounts) > 0 {
		return globalWorkspaceMounts[0].MountPath
	}
	return ""
}

// resolveLowerDir returns the lower directory for a mount.
// In remote mode (VirtioTag is empty), MountPath is used directly as the lower dir.
// In local VM mode, the VirtioFS mount point is used.
func resolveLowerDir(mount MountConfig) string {
	if mount.VirtioTag == "" {
		return mount.MountPath
	}
	return GetVirtioFSMountPoint(mount.VirtioTag)
}
