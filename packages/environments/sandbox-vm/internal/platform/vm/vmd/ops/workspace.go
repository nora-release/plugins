//go:build linux

package ops

import (
	"fmt"
	"log"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

// WorkspaceManager handles workspace VirtioFS mounting.
type WorkspaceManager struct {
	config    *PathConfig
	setupFunc func(MountConfig) (*SetupResult, error)
}

// NewWorkspaceManager creates a new WorkspaceManager.
func NewWorkspaceManager(config *PathConfig) *WorkspaceManager {
	manager := &WorkspaceManager{config: config}
	manager.setupFunc = manager.setupSingle
	return manager
}

// SetupResult contains the result of a single workspace setup.
type SetupResult struct {
	MountedPath string
}

// SetupAllResult contains the results of setting up all workspaces.
type SetupAllResult struct {
	Results []SetupResult
}

// SetupAll mounts multiple workspace VirtioFS shares.
// Each mount is set up with:
// - VirtioFS mounted read-only at WorkspaceLowerBase/tag (for session overlays)
// - VirtioFS mounted read-write at mountPath (for direct access, if not read-only)
func (m *WorkspaceManager) SetupAll(mounts []MountConfig) (*SetupAllResult, error) {
	mounts = filterRemoteWorkspaceMounts(mounts)
	if len(mounts) == 0 {
		return nil, fmt.Errorf("no mounts specified")
	}
	SetGlobalWorkspaceMounts(nil)

	// Ensure base directory exists
	if err := os.MkdirAll(WorkspaceLowerBase, 0755); err != nil {
		return nil, fmt.Errorf("create workspace lower base %s: %w", WorkspaceLowerBase, err)
	}

	results := make([]SetupResult, 0, len(mounts))
	successfulMounts := make([]MountConfig, 0, len(mounts))
	skipped := 0

	for i, mount := range mounts {
		setupFunc := m.setupFunc
		if setupFunc == nil {
			setupFunc = m.setupSingle
		}
		result, err := setupFunc(mount)
		if err != nil {
			skipped++
			log.Printf("Skipping workspace mount[%d] tag=%q path=%q: %v", i, mount.VirtioTag, mount.MountPath, err)
			continue
		}
		results = append(results, *result)
		successfulMounts = append(successfulMounts, mount)
	}

	if len(successfulMounts) == 0 {
		return nil, fmt.Errorf("no workspace mounts could be set up")
	}

	// Store only the mounts that were actually configured for session use.
	SetGlobalWorkspaceMounts(successfulMounts)

	if skipped > 0 {
		log.Printf("Workspace setup completed with %d/%d mounts skipped", skipped, len(mounts))
	}

	return &SetupAllResult{Results: results}, nil
}

func filterRemoteWorkspaceMounts(mounts []MountConfig) []MountConfig {
	if os.Getenv("VMD_REMOTE") != "1" {
		return mounts
	}

	filtered := make([]MountConfig, 0, len(mounts))
	for _, mount := range mounts {
		if filepath.Clean(mount.MountPath) != "/mnt/workspace" {
			log.Printf("Ignoring remote workspace mount %q; only /mnt/workspace remains sandbox-scoped in remote mode", mount.MountPath)
			continue
		}
		filtered = append(filtered, mount)
	}
	return filtered
}

// setupSingle mounts a single workspace VirtioFS share.
func (m *WorkspaceManager) setupSingle(mount MountConfig) (*SetupResult, error) {
	if mount.MountPath == "" {
		return nil, fmt.Errorf("mount path is required")
	}

	// Remote mode: VirtioTag is empty, use MountPath as a local directory.
	if mount.VirtioTag == "" {
		return m.setupLocalDir(mount)
	}

	// Create the lower mount point for this tag
	lowerPath := GetVirtioFSMountPoint(mount.VirtioTag)
	if err := os.MkdirAll(lowerPath, 0755); err != nil {
		return nil, fmt.Errorf("create lower mount point %s: %w", lowerPath, err)
	}

	if mount.Passthrough {
		// Passthrough: mount read-write for direct host access
		if err := m.mountVirtioFS(mount.VirtioTag, lowerPath, false); err != nil {
			return nil, fmt.Errorf("mount VirtioFS at lower (passthrough): %w", err)
		}
		log.Printf("Mounted VirtioFS %q at %s (read-write, passthrough)", mount.VirtioTag, lowerPath)
	} else {
		// Normal: mount read-only for overlay use
		if err := m.mountVirtioFS(mount.VirtioTag, lowerPath, true); err != nil {
			return nil, fmt.Errorf("mount VirtioFS at lower: %w", err)
		}
		log.Printf("Mounted VirtioFS %q at %s (read-only, for session overlays)", mount.VirtioTag, lowerPath)
	}

	return &SetupResult{
		MountedPath: mount.MountPath,
	}, nil
}

// setupLocalDir handles remote mode where a local directory is used instead of VirtioFS.
func (m *WorkspaceManager) setupLocalDir(mount MountConfig) (*SetupResult, error) {
	if err := os.MkdirAll(mount.MountPath, 0755); err != nil {
		return nil, fmt.Errorf("create local dir %s: %w", mount.MountPath, err)
	}
	log.Printf("Using local directory %s (remote mode, no VirtioFS)", mount.MountPath)
	return &SetupResult{
		MountedPath: mount.MountPath,
	}, nil
}

// mountVirtioFS mounts a VirtioFS share.
func (m *WorkspaceManager) mountVirtioFS(tag, mountPoint string, readOnly bool) error {
	var cmd *exec.Cmd
	if readOnly {
		cmd = exec.Command("mount", "-t", "virtiofs", "-o", "ro", tag, mountPoint)
	} else {
		cmd = exec.Command("mount", "-t", "virtiofs", tag, mountPoint)
	}
	if out, err := cmd.CombinedOutput(); err != nil {
		return fmt.Errorf("mount failed: %w, output: %s", err, strings.TrimSpace(string(out)))
	}
	return nil
}
