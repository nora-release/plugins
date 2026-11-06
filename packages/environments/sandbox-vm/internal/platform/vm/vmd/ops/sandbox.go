//go:build linux

package ops

import (
	"archive/tar"
	"bytes"
	"context"
	"fmt"
	"io"
	"io/fs"
	"log"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/nora/sandbox-vm/internal/executil"
	"github.com/nora/sandbox-vm/internal/platform/vm/vmd/overlay"
)

// SandboxManager manages sandbox workspace overlays.
type SandboxManager struct {
	config    *PathConfig
	sandboxes map[string]*Sandbox
}

// OverlayMount represents a single overlay mount for a sandbox.
type OverlayMount struct {
	MountConfig MountConfig // The original mount config
	LowerDir    string      // VirtioFS lower dir (e.g., /tmp/workspace-lower/mount-0)
	UpperDir    string      // Sandbox upper dir for this mount
	WorkDir     string      // Overlay work dir for this mount
	MergedDir   string      // Where overlay is mounted (intermediate)
	analyzer    *overlay.OverlayDiffAnalyzer
}

// Sandbox represents an active sandbox with its paths and operations.
type Sandbox struct {
	ID       string
	BasePath string // e.g., /mnt/storage/nora/sandboxes/<id>
	RootDir  string // sandbox root (chroot target)

	// Multiple overlays, one per mount
	Overlays []*OverlayMount

	// workspaceMounted tracks whether workspace is currently mounted
	workspaceMounted bool
}

func remoteFilesystemVirtualizationDisabled() bool {
	return os.Getenv("VMD_REMOTE") == "1"
}

// NewSandboxManager creates a new SandboxManager.
func NewSandboxManager(config *PathConfig) *SandboxManager {
	return &SandboxManager{
		config:    config,
		sandboxes: make(map[string]*Sandbox),
	}
}

// Get returns a cached sandbox by ID, or error if not found.
func (m *SandboxManager) Get(sandboxID string) (*Sandbox, error) {
	if s, ok := m.sandboxes[sandboxID]; ok {
		return s, nil
	}
	return nil, fmt.Errorf("sandbox %s not found", sandboxID)
}

// Create creates a new sandbox and prepares it.
func (m *SandboxManager) Create(sandboxID string) (*Sandbox, error) {
	base := m.config.GetSandboxPath(sandboxID)
	s := &Sandbox{
		ID:       sandboxID,
		BasePath: base,
		RootDir:  m.config.GetSandboxRoot(sandboxID),
	}

	// Get global workspace mounts
	mounts := GetGlobalWorkspaceMounts()
	if len(mounts) == 0 {
		return nil, fmt.Errorf("no workspace mounts configured, call SetupWorkspaces first")
	}

	// Create an overlay struct for each mount
	for i, mount := range mounts {
		overlayDir := filepath.Join(base, fmt.Sprintf("overlay-%d", i))
		ov := &OverlayMount{
			MountConfig: mount,
			LowerDir:    resolveSandboxLowerDir(base, mount, i),
			UpperDir:    filepath.Join(overlayDir, "upper"),
			WorkDir:     filepath.Join(overlayDir, "work"),
			MergedDir:   filepath.Join(overlayDir, "merged"),
		}
		s.Overlays = append(s.Overlays, ov)
	}

	if err := s.Prepare(); err != nil {
		return nil, err
	}

	// Initialize analyzers for each overlay
	for _, ov := range s.Overlays {
		if ov.MountConfig.ReadOnly || ov.MountConfig.Passthrough {
			continue // No analyzer needed for read-only or passthrough mounts
		}
		analyzer, err := overlay.NewOverlayDiffAnalyzer(ov.UpperDir, ov.LowerDir)
		if err != nil {
			// Non-fatal: continue without analyzer
			analyzer = nil
		}
		ov.analyzer = analyzer
	}

	m.sandboxes[sandboxID] = s
	return s, nil
}

// Delete removes a sandbox from cache (caller should call sandbox.Cleanup first).
func (m *SandboxManager) Delete(sandboxID string) {
	delete(m.sandboxes, sandboxID)
}

// Exists checks if a sandbox exists on disk (can be restored).
// This checks if the sandbox directory exists with at least one overlay subdirectory.
func (m *SandboxManager) Exists(sandboxID string) bool {
	base := m.config.GetSandboxPath(sandboxID)

	// Check if base directory exists
	info, err := os.Stat(base)
	if err != nil || !info.IsDir() {
		return false
	}

	// Check if at least one overlay directory exists
	overlay0 := filepath.Join(base, "overlay-0")
	info, err = os.Stat(overlay0)
	return err == nil && info.IsDir()
}

// Restore restores an existing sandbox from disk.
// The sandbox must have been previously created and its directory must exist.
// This re-mounts the sandbox root (bind of /) and initializes analyzers.
func (m *SandboxManager) Restore(sandboxID string) (*Sandbox, error) {
	// Check if already in cache
	if s, ok := m.sandboxes[sandboxID]; ok {
		return s, nil
	}

	base := m.config.GetSandboxPath(sandboxID)
	if !m.Exists(sandboxID) {
		return nil, fmt.Errorf("sandbox %s does not exist on disk", sandboxID)
	}

	s := &Sandbox{
		ID:       sandboxID,
		BasePath: base,
		RootDir:  m.config.GetSandboxRoot(sandboxID),
	}

	// Get global workspace mounts
	mounts := GetGlobalWorkspaceMounts()
	if len(mounts) == 0 {
		return nil, fmt.Errorf("no workspace mounts configured, call SetupWorkspaces first")
	}

	// Rebuild overlay structs for each mount
	for i, mount := range mounts {
		overlayDir := filepath.Join(base, fmt.Sprintf("overlay-%d", i))
		ov := &OverlayMount{
			MountConfig: mount,
			LowerDir:    resolveSandboxLowerDir(base, mount, i),
			UpperDir:    filepath.Join(overlayDir, "upper"),
			WorkDir:     filepath.Join(overlayDir, "work"),
			MergedDir:   filepath.Join(overlayDir, "merged"),
		}
		s.Overlays = append(s.Overlays, ov)
	}

	// Re-prepare sandbox (re-mount root bind and system dirs)
	if err := s.Prepare(); err != nil {
		return nil, fmt.Errorf("failed to prepare restored sandbox: %w", err)
	}

	// Initialize analyzers for each overlay
	for _, ov := range s.Overlays {
		if ov.MountConfig.ReadOnly || ov.MountConfig.Passthrough {
			continue
		}
		analyzer, err := overlay.NewOverlayDiffAnalyzer(ov.UpperDir, ov.LowerDir)
		if err != nil {
			analyzer = nil
		}
		ov.analyzer = analyzer
	}

	m.sandboxes[sandboxID] = s
	log.Printf("Restored sandbox %s from disk", sandboxID)
	return s, nil
}

// DeleteAll cleans up and removes all sandboxes.
// Returns the number of sandboxes deleted and any error encountered.
func (m *SandboxManager) DeleteAll() (int, error) {
	count := 0
	for sandboxID, sandbox := range m.sandboxes {
		if err := sandbox.Cleanup(); err != nil {
			return count, fmt.Errorf("failed to cleanup sandbox %s: %w", sandboxID, err)
		}
		delete(m.sandboxes, sandboxID)
		count++
	}

	// Remove entire sandboxes directory
	if err := os.RemoveAll(m.config.SandboxesRoot); err != nil {
		return count, fmt.Errorf("failed to remove sandboxes directory: %w", err)
	}

	return count, nil
}

// Prepare sets up the sandbox's directory structure and mounts static filesystems.
func (s *Sandbox) Prepare() error {
	if !remoteFilesystemVirtualizationDisabled() {
		if err := EnsureOverlaySupport(); err != nil {
			return err
		}
	}

	// Create directories for each overlay
	for _, ov := range s.Overlays {
		if needsManagedPassthroughLowerDir(ov.MountConfig, ov.LowerDir) {
			if err := os.MkdirAll(ov.LowerDir, 0755); err != nil {
				return fmt.Errorf("failed to create managed workspace dir %s: %w", ov.LowerDir, err)
			}
		}
		for _, dir := range []string{ov.UpperDir, ov.WorkDir, ov.MergedDir} {
			if err := os.MkdirAll(dir, 0755); err != nil {
				return fmt.Errorf("failed to create %s: %w", dir, err)
			}
		}
	}

	// Create sandbox root
	if err := os.MkdirAll(s.RootDir, 0755); err != nil {
		return fmt.Errorf("failed to create sandbox root: %w", err)
	}

	if remoteFilesystemVirtualizationDisabled() {
		return nil
	}

	// Mount sandbox root by binding VM's root (/)
	if err := MountBind("/", s.RootDir); err != nil {
		return fmt.Errorf("failed to bind root to sandbox root: %w", err)
	}
	_ = MakeRSlave(s.RootDir)

	// Bind system directories (/dev, /proc, /sys, etc.)
	if err := BindSystemDirs(s.RootDir); err != nil {
		return fmt.Errorf("failed to bind system directories to sandbox root: %w", err)
	}

	return nil
}

func resolveSandboxLowerDir(base string, mount MountConfig, index int) string {
	if mount.VirtioTag == "" && mount.Passthrough {
		return filepath.Join(base, fmt.Sprintf("workspace-%d", index))
	}
	return resolveLowerDir(mount)
}

func needsManagedPassthroughLowerDir(mount MountConfig, lowerDir string) bool {
	return mount.VirtioTag == "" && mount.Passthrough && lowerDir != ""
}

func isRemoteWorkspaceMount(path string) bool {
	return filepath.Clean(path) == "/mnt/workspace"
}

// Mount mounts all overlay filesystems and binds them into sandbox root.
func (s *Sandbox) Mount() error {
	if !remoteFilesystemVirtualizationDisabled() && !IsMountPoint(s.RootDir) {
		return fmt.Errorf("sandbox root is not prepared, call Prepare() first")
	}

	for i, ov := range s.Overlays {
		if remoteFilesystemVirtualizationDisabled() && !isRemoteWorkspaceMount(ov.MountConfig.MountPath) {
			continue
		}

		// Skip read-only and passthrough mounts - bind directly from lower
		if ov.MountConfig.ReadOnly || ov.MountConfig.Passthrough {
			if remoteFilesystemVirtualizationDisabled() {
				continue
			}
			sandboxMountInRoot := filepath.Join(s.RootDir, ov.MountConfig.MountPath)
			if err := os.MkdirAll(sandboxMountInRoot, 0755); err != nil {
				return fmt.Errorf("overlay[%d]: failed to create mount point: %w", i, err)
			}
			if err := MountRBind(ov.LowerDir, sandboxMountInRoot); err != nil {
				return fmt.Errorf("overlay[%d]: failed to bind read-only mount: %w", i, err)
			}
			_ = MakeRSlave(sandboxMountInRoot)
			continue
		}

		if remoteFilesystemVirtualizationDisabled() {
			continue
		}

		// Mount overlay filesystem
		if err := MountOverlay(ov.LowerDir, ov.UpperDir, ov.WorkDir, ov.MergedDir); err != nil {
			return fmt.Errorf("overlay[%d]: failed to mount overlay: %w", i, err)
		}

		// Bind overlay into sandbox root at the original path
		sandboxMountInRoot := filepath.Join(s.RootDir, ov.MountConfig.MountPath)
		if err := os.MkdirAll(sandboxMountInRoot, 0755); err != nil {
			return fmt.Errorf("overlay[%d]: failed to create mount point in sandbox root: %w", i, err)
		}
		if err := MountRBind(ov.MergedDir, sandboxMountInRoot); err != nil {
			return fmt.Errorf("overlay[%d]: failed to bind overlay into sandbox root: %w", i, err)
		}
		_ = MakeRSlave(sandboxMountInRoot)
	}

	s.workspaceMounted = true
	return nil
}

// Unmount unmounts all overlays and their bindings in sandbox root.
func (s *Sandbox) Unmount() error {
	// Unmount in reverse order (last mounted first)
	for i := len(s.Overlays) - 1; i >= 0; i-- {
		ov := s.Overlays[i]

		// Unmount binding inside sandbox root
		sandboxMountInRoot := filepath.Join(s.RootDir, ov.MountConfig.MountPath)
		if IsMountPoint(sandboxMountInRoot) {
			if err := Unmount(sandboxMountInRoot); err != nil {
				return fmt.Errorf("overlay[%d]: failed to unmount binding: %w", i, err)
			}
		}

		// Unmount overlay (only for normal overlay mounts)
		if !ov.MountConfig.ReadOnly && !ov.MountConfig.Passthrough && IsMountPoint(ov.MergedDir) {
			if err := Unmount(ov.MergedDir); err != nil {
				return fmt.Errorf("overlay[%d]: failed to unmount overlay: %w", i, err)
			}
		}
	}

	s.workspaceMounted = false
	return nil
}

// Cleanup unmounts and removes the sandbox's filesystem.
func (s *Sandbox) Cleanup() error {
	if err := s.Unmount(); err != nil {
		return fmt.Errorf("failed to unmount workspace during cleanup: %w", err)
	}

	// Close analyzers before unmounting
	for _, ov := range s.Overlays {
		if ov.analyzer != nil {
			ov.analyzer.Close()
			ov.analyzer = nil
		}
	}

	// Unmount sandbox root (bind of /)
	if !remoteFilesystemVirtualizationDisabled() && IsMountPoint(s.RootDir) {
		UnbindSystemDirs(s.RootDir)
		if err := Unmount(s.RootDir); err != nil {
			return fmt.Errorf("failed to unmount sandbox root: %w", err)
		}
	}

	return nil
}

// IsPrepared checks if the sandbox directories are initialized.
func (s *Sandbox) IsPrepared() bool {
	_, err := os.Stat(s.RootDir)
	return err == nil
}

// IsMounted checks if the sandbox's overlay filesystems are currently mounted.
func (s *Sandbox) IsMounted() bool {
	return s.workspaceMounted
}

// getOverlayForPath finds the overlay that contains the given path based on MountPath.
// Returns the overlay and the relative path within that overlay.
func (s *Sandbox) getOverlayForPath(path string) (*OverlayMount, string) {
	// Normalize path to always start with /
	if !strings.HasPrefix(path, "/") {
		path = "/" + path
	}
	path = filepath.Clean(path)

	// Find the overlay with the longest matching MountPath prefix
	var bestMatch *OverlayMount
	var bestMatchLen int
	var relPath string

	for _, ov := range s.Overlays {
		mountPath := filepath.Clean(ov.MountConfig.MountPath)

		// Check if path is under this mount
		if path == mountPath || strings.HasPrefix(path, mountPath+"/") {
			if len(mountPath) > bestMatchLen {
				bestMatch = ov
				bestMatchLen = len(mountPath)
				if path == mountPath {
					relPath = "."
				} else {
					relPath = strings.TrimPrefix(path, mountPath+"/")
				}
			}
		}
	}

	return bestMatch, relPath
}

// Exec executes a command inside the sandbox's isolated namespace.
func (s *Sandbox) Exec(command []string, workingDir string, env map[string]string) (stdout, stderr string, exitCode int, err error) {
	return s.ExecWithContext(context.Background(), command, workingDir, env)
}

func (s *Sandbox) ExecWithContext(ctx context.Context, command []string, workingDir string, env map[string]string) (stdout, stderr string, exitCode int, err error) {
	if !s.IsMounted() {
		return "", "", -1, fmt.Errorf("sandbox %s is not mounted", s.ID)
	}
	if remoteFilesystemVirtualizationDisabled() {
		return s.execInRemoteNamespace(ctx, command, workingDir, env)
	}
	return execInNamespace(ctx, s.RootDir, command, workingDir, env)
}

// ReadFile reads a file from the sandbox.
// When mounted: reads directly from SandboxRoot.
// When unmounted: finds the overlay for the path and reads from upper (if writable) or lower.
func (s *Sandbox) ReadFile(path string) ([]byte, error) {
	if s.workspaceMounted {
		fullPath, err := s.resolveMountedHostPath(path)
		if err != nil {
			return nil, err
		}
		return os.ReadFile(fullPath)
	}

	// When unmounted, find the overlay and read from upper or lower
	ov, relPath := s.getOverlayForPath(path)
	if ov == nil {
		return nil, fmt.Errorf("no overlay found for path: %s", path)
	}

	if ov.MountConfig.ReadOnly {
		// Read-only overlay: read directly from lower
		fullPath := filepath.Join(ov.LowerDir, relPath)
		return os.ReadFile(fullPath)
	}

	// Writable overlay: try upper first, then lower
	upperPath := filepath.Join(ov.UpperDir, relPath)
	if data, err := os.ReadFile(upperPath); err == nil {
		return data, nil
	}

	// Fall back to lower
	lowerPath := filepath.Join(ov.LowerDir, relPath)
	return os.ReadFile(lowerPath)
}

// OpenFileForRead opens a sandbox file without reading it entirely into memory.
func (s *Sandbox) OpenFileForRead(path string) (*os.File, fs.FileInfo, error) {
	fullPath, err := s.resolveReadablePath(path)
	if err != nil {
		return nil, nil, err
	}
	file, err := os.Open(fullPath)
	if err != nil {
		return nil, nil, err
	}
	info, err := file.Stat()
	if err != nil {
		_ = file.Close()
		return nil, nil, err
	}
	return file, info, nil
}

// WriteFile writes content to a file in the sandbox.
// This operation requires the workspace to be mounted and writes directly to SandboxRoot.
func (s *Sandbox) WriteFile(path string, content []byte, appendMode bool) error {
	if !s.workspaceMounted {
		return fmt.Errorf("workspace must be mounted to write files")
	}

	fullPath, err := s.resolveMountedHostPath(path)
	if err != nil {
		return err
	}

	if err := os.MkdirAll(filepath.Dir(fullPath), 0755); err != nil {
		return fmt.Errorf("failed to create parent directory: %w", err)
	}

	flag := os.O_WRONLY | os.O_CREATE
	if appendMode {
		flag |= os.O_APPEND
	} else {
		flag |= os.O_TRUNC
	}

	f, err := os.OpenFile(fullPath, flag, 0644)
	if err != nil {
		return err
	}
	defer f.Close()

	_, err = f.Write(content)
	return err
}

// DeleteFile deletes a file from the sandbox.
// This operation requires the workspace to be mounted and deletes from SandboxRoot.
func (s *Sandbox) DeleteFile(path string) error {
	if !s.workspaceMounted {
		return fmt.Errorf("workspace must be mounted to delete files")
	}

	fullPath, err := s.resolveMountedHostPath(path)
	if err != nil {
		return err
	}
	return os.Remove(fullPath)
}

// FileExists checks if a file exists in the sandbox.
// When mounted: checks directly in SandboxRoot.
// When unmounted: finds the overlay and checks upper (if writable) or lower.
func (s *Sandbox) FileExists(path string) bool {
	if s.workspaceMounted {
		fullPath, err := s.resolveMountedHostPath(path)
		if err != nil {
			return false
		}
		_, err = os.Stat(fullPath)
		return err == nil
	}

	// When unmounted, find the overlay and check upper or lower
	ov, relPath := s.getOverlayForPath(path)
	if ov == nil {
		return false
	}

	if ov.MountConfig.ReadOnly {
		// Read-only overlay: check lower only
		fullPath := filepath.Join(ov.LowerDir, relPath)
		_, err := os.Stat(fullPath)
		return err == nil
	}

	// Writable overlay: check upper first, then lower
	upperPath := filepath.Join(ov.UpperDir, relPath)
	if _, err := os.Stat(upperPath); err == nil {
		return true
	}

	lowerPath := filepath.Join(ov.LowerDir, relPath)
	_, err := os.Stat(lowerPath)
	return err == nil
}

func (s *Sandbox) resolveReadablePath(path string) (string, error) {
	if s.workspaceMounted {
		return s.resolveMountedHostPath(path)
	}

	ov, relPath := s.getOverlayForPath(path)
	if ov == nil {
		return "", fmt.Errorf("no overlay found for path: %s", path)
	}

	if ov.MountConfig.ReadOnly {
		return filepath.Join(ov.LowerDir, relPath), nil
	}

	upperPath := filepath.Join(ov.UpperDir, relPath)
	if _, err := os.Stat(upperPath); err == nil {
		return upperPath, nil
	}
	return filepath.Join(ov.LowerDir, relPath), nil
}

func (s *Sandbox) resolveMountedHostPath(path string) (string, error) {
	if !strings.HasPrefix(path, "/") {
		path = "/" + path
	}
	path = filepath.Clean(path)

	if !remoteFilesystemVirtualizationDisabled() {
		return filepath.Join(s.RootDir, path), nil
	}

	if ov, relPath := s.getOverlayForPath(path); ov != nil {
		if ov.MountConfig.ReadOnly || ov.MountConfig.Passthrough {
			return filepath.Join(ov.LowerDir, relPath), nil
		}
		return "", fmt.Errorf("overlay mounts are disabled for remote sandboxes: %s", path)
	}

	return path, nil
}

// ResolveHostPath returns the concrete VM path that backs a sandbox path while mounted.
func (s *Sandbox) ResolveHostPath(path string) (string, error) {
	if !s.workspaceMounted {
		return "", fmt.Errorf("workspace must be mounted to resolve paths")
	}
	return s.resolveMountedHostPath(path)
}

// ExecStream executes a command and streams stdout/stderr via callbacks.
func (s *Sandbox) ExecStream(ctx context.Context, command []string, workingDir string, env map[string]string, onStdout, onStderr func([]byte), onExit func(int)) error {
	if !s.IsMounted() {
		return fmt.Errorf("sandbox %s is not mounted", s.ID)
	}
	if remoteFilesystemVirtualizationDisabled() {
		return s.execStreamInRemoteNamespace(ctx, command, workingDir, env, onStdout, onStderr, onExit)
	}

	args := []string{
		"--mount",
		"--pid",
		"--fork",
		"--kill-child",
		"--root=" + s.RootDir,
		"--mount-proc",
		"--",
	}
	args = append(args, command...)

	cmd := exec.CommandContext(ctx, "unshare", args...)
	executil.ConfigureCommandCancellation(cmd)
	cmd.Env = mergeEnv(env)
	if workingDir != "" {
		cmd.Dir = filepath.Join(s.RootDir, workingDir)
	}

	stdoutPipe, err := cmd.StdoutPipe()
	if err != nil {
		return fmt.Errorf("failed to create stdout pipe: %w", err)
	}
	stderrPipe, err := cmd.StderrPipe()
	if err != nil {
		return fmt.Errorf("failed to create stderr pipe: %w", err)
	}

	if err := cmd.Start(); err != nil {
		return fmt.Errorf("failed to start command: %w", err)
	}

	var readers sync.WaitGroup
	readers.Add(2)

	go func() {
		defer readers.Done()
		buf := make([]byte, 4096)
		for {
			n, err := stdoutPipe.Read(buf)
			if n > 0 && onStdout != nil {
				onStdout(buf[:n])
			}
			if err != nil {
				break
			}
		}
	}()

	go func() {
		defer readers.Done()
		buf := make([]byte, 4096)
		for {
			n, err := stderrPipe.Read(buf)
			if n > 0 && onStderr != nil {
				onStderr(buf[:n])
			}
			if err != nil {
				break
			}
		}
	}()

	err = cmd.Wait()
	exitCode := 0
	if err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok {
			if status, ok := exitErr.Sys().(syscall.WaitStatus); ok {
				exitCode = status.ExitStatus()
			}
		}
	}
	readers.Wait()

	if onExit != nil {
		onExit(exitCode)
	}
	return nil
}

func (s *Sandbox) remoteWorkspaceSource() string {
	for _, ov := range s.Overlays {
		if isRemoteWorkspaceMount(ov.MountConfig.MountPath) {
			return ov.LowerDir
		}
	}
	return ""
}

func (s *Sandbox) execInRemoteNamespace(ctx context.Context, command []string, workingDir string, env map[string]string) (stdout, stderr string, exitCode int, err error) {
	if len(command) == 0 {
		return "", "", -1, fmt.Errorf("no command provided")
	}

	args := []string{
		"--mount",
		"--pid",
		"--fork",
		"--kill-child",
		"--mount-proc",
		"--",
		"sh",
		"-ceu",
		remoteNamespaceExecScript,
		"sh",
	}
	args = append(args, command...)

	cmd := exec.CommandContext(ctx, "unshare", args...)
	executil.ConfigureCommandCancellation(cmd)
	cmd.Env = mergeEnvWithRemoteNamespace(env, s.remoteWorkspaceSource(), workingDir)

	var stdoutBuf, stderrBuf bytes.Buffer
	cmd.Stdout = &stdoutBuf
	cmd.Stderr = &stderrBuf

	if err := cmd.Start(); err != nil {
		return "", "", -1, err
	}

	err = cmd.Wait()
	stdout = stdoutBuf.String()
	stderr = stderrBuf.String()

	if err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok {
			if status, ok := exitErr.Sys().(syscall.WaitStatus); ok {
				exitCode = status.ExitStatus()
				return stdout, stderr, exitCode, nil
			}
		}
		return stdout, stderr, -1, err
	}

	return stdout, stderr, 0, nil
}

func (s *Sandbox) execStreamInRemoteNamespace(ctx context.Context, command []string, workingDir string, env map[string]string, onStdout, onStderr func([]byte), onExit func(int)) error {
	args := []string{
		"--mount",
		"--pid",
		"--fork",
		"--kill-child",
		"--mount-proc",
		"--",
		"sh",
		"-ceu",
		remoteNamespaceExecScript,
		"sh",
	}
	args = append(args, command...)

	cmd := exec.CommandContext(ctx, "unshare", args...)
	executil.ConfigureCommandCancellation(cmd)
	cmd.Env = mergeEnvWithRemoteNamespace(env, s.remoteWorkspaceSource(), workingDir)

	stdoutPipe, err := cmd.StdoutPipe()
	if err != nil {
		return fmt.Errorf("failed to create stdout pipe: %w", err)
	}
	stderrPipe, err := cmd.StderrPipe()
	if err != nil {
		return fmt.Errorf("failed to create stderr pipe: %w", err)
	}

	if err := cmd.Start(); err != nil {
		return fmt.Errorf("failed to start command: %w", err)
	}

	var readers sync.WaitGroup
	readers.Add(2)

	go func() {
		defer readers.Done()
		buf := make([]byte, 4096)
		for {
			n, err := stdoutPipe.Read(buf)
			if n > 0 && onStdout != nil {
				onStdout(buf[:n])
			}
			if err != nil {
				break
			}
		}
	}()

	go func() {
		defer readers.Done()
		buf := make([]byte, 4096)
		for {
			n, err := stderrPipe.Read(buf)
			if n > 0 && onStderr != nil {
				onStderr(buf[:n])
			}
			if err != nil {
				break
			}
		}
	}()

	err = cmd.Wait()
	exitCode := 0
	if err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok {
			if status, ok := exitErr.Sys().(syscall.WaitStatus); ok {
				exitCode = status.ExitStatus()
			}
		}
	}
	readers.Wait()

	if onExit != nil {
		onExit(exitCode)
	}
	return nil
}

// execInNamespace executes a command in a new PID/mount namespace with chroot.
func execInNamespace(ctx context.Context, rootDir string, command []string, workingDir string, env map[string]string) (stdout, stderr string, exitCode int, err error) {
	if len(command) == 0 {
		return "", "", -1, fmt.Errorf("no command provided")
	}

	args := []string{
		"--mount",
		"--pid",
		"--fork",
		"--kill-child",
		"--root=" + rootDir,
		"--mount-proc",
		"--",
	}
	args = append(args, command...)

	cmd := exec.CommandContext(ctx, "unshare", args...)
	executil.ConfigureCommandCancellation(cmd)
	cmd.Env = mergeEnv(env)

	var stdoutBuf, stderrBuf bytes.Buffer
	cmd.Stdout = &stdoutBuf
	cmd.Stderr = &stderrBuf

	if workingDir != "" {
		// Working directory must be resolved relative to the sandbox root on the VM
		// filesystem, since cmd.Dir is evaluated BEFORE unshare --root pivots the root.
		// After pivot_root, the process's working directory inode stays the same and
		// becomes accessible at its original path inside the new root.
		cmd.Dir = filepath.Join(rootDir, workingDir)
	}

	if err := cmd.Start(); err != nil {
		return "", "", -1, err
	}

	err = cmd.Wait()
	stdout = stdoutBuf.String()
	stderr = stderrBuf.String()

	if err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok {
			if status, ok := exitErr.Sys().(syscall.WaitStatus); ok {
				exitCode = status.ExitStatus()
				return stdout, stderr, exitCode, nil
			}
		}
		return stdout, stderr, -1, err
	}

	return stdout, stderr, 0, nil
}

func mergeEnv(overrides map[string]string) []string {
	base := os.Environ()
	if len(overrides) == 0 {
		return base
	}

	index := make(map[string]int, len(base))
	for i, kv := range base {
		if eq := strings.IndexByte(kv, '='); eq > 0 {
			index[kv[:eq]] = i
		}
	}

	for key, value := range overrides {
		entry := key + "=" + value
		if i, ok := index[key]; ok {
			base[i] = entry
			continue
		}
		base = append(base, entry)
	}

	return base
}

func mergeEnvWithRemoteNamespace(overrides map[string]string, workspaceSource string, workingDir string) []string {
	base := mergeEnv(overrides)
	base = append(base, "CUEBOARD_REMOTE_WORKSPACE_SOURCE="+workspaceSource)
	base = append(base, "CUEBOARD_REMOTE_WORKDIR="+workingDir)
	return base
}

const remoteNamespaceExecScript = `
workspace_source="${CUEBOARD_REMOTE_WORKSPACE_SOURCE:-}"
if [ -n "$workspace_source" ]; then
    mkdir -p /mnt/workspace
    mount --rbind "$workspace_source" /mnt/workspace
    mount --make-rslave /mnt/workspace || true
fi

workdir="${CUEBOARD_REMOTE_WORKDIR:-}"
if [ -n "$workdir" ]; then
    cd "$workdir"
fi

exec "$@"
`

// GetFileDiff returns detailed file diff information for all writable overlays.
// Paths in the returned diffs are prefixed with their respective MountPath.
func (s *Sandbox) GetFileDiff() ([]overlay.FileDiff, error) {
	overallStart := time.Now()
	defer func() {
		log.Printf("Sandbox.GetFileDiff took %v", time.Since(overallStart))
	}()

	var allDiffs []overlay.FileDiff

	for _, ov := range s.Overlays {
		if ov.MountConfig.ReadOnly || ov.MountConfig.Passthrough {
			continue
		}

		if _, err := os.Stat(ov.UpperDir); os.IsNotExist(err) {
			continue
		}
		if _, err := os.Stat(ov.LowerDir); os.IsNotExist(err) {
			return nil, fmt.Errorf("lower directory does not exist: %s", ov.LowerDir)
		}

		if ov.analyzer == nil {
			continue
		}

		analyzeStart := time.Now()
		result, err := ov.analyzer.Analyze()
		if err != nil {
			return nil, err
		}
		log.Printf("Sandbox.GetFileDiff: analyzer.Analyze for %s took %v", ov.MountConfig.MountPath, time.Since(analyzeStart))

		// Prefix paths with MountPath
		mountPath := ov.MountConfig.MountPath
		for _, diff := range result.FileDiff {
			if mountPath != "" {
				diff.Path = filepath.Join(mountPath, diff.Path)
				if diff.MovedFrom != "" {
					diff.MovedFrom = filepath.Join(mountPath, diff.MovedFrom)
				}
			}
			allDiffs = append(allDiffs, diff)
		}
	}

	return allDiffs, nil
}

// RunHousekeeper performs housekeeping on all writable overlay filesystems.
func (s *Sandbox) RunHousekeeper() error {
	overallStart := time.Now()
	defer func() {
		log.Printf("Sandbox.RunHousekeeper took %v", time.Since(overallStart))
	}()

	if s.IsMounted() {
		return fmt.Errorf("sandbox must be unmounted before running housekeeper")
	}

	for _, ov := range s.Overlays {
		if ov.MountConfig.ReadOnly || ov.MountConfig.Passthrough {
			continue
		}

		if ov.analyzer == nil {
			continue
		}

		housekeeperStart := time.Now()
		housekeeper := overlay.NewHousekeeperWithAnalyzer(ov.UpperDir, ov.LowerDir, ov.analyzer)
		if err := housekeeper.Run(); err != nil {
			return fmt.Errorf("housekeeper failed for mount %s: %w", ov.MountConfig.MountPath, err)
		}
		log.Printf("Sandbox.RunHousekeeper: housekeeper.Run for %s took %v", ov.MountConfig.MountPath, time.Since(housekeeperStart))
	}

	return nil
}

// ExportDiffResult contains the diff metadata for ExportDiff.
type ExportDiffResult struct {
	Changes []overlay.FileDiff
	Paths   []string
}

// ExportDiff generates filtered diff metadata and streams tar data.
func (s *Sandbox) ExportDiff(paths []string, onMetadata func(*ExportDiffResult), onTarData func([]byte)) error {
	overallStart := time.Now()
	defer func() {
		log.Printf("Sandbox.ExportDiff took %v", time.Since(overallStart))
	}()

	if s.IsMounted() {
		return fmt.Errorf("sandbox must be unmounted before exporting diff")
	}

	normalizedPaths := normalizePaths(paths)

	diffStart := time.Now()
	globalDiffs, err := s.GetFileDiff()
	if err != nil {
		return fmt.Errorf("getting file diff: %w", err)
	}
	log.Printf("Sandbox.ExportDiff: GetFileDiff took %v", time.Since(diffStart))

	filteredDiffs := filterDiffsByPaths(globalDiffs, normalizedPaths)

	onMetadata(&ExportDiffResult{
		Changes: filteredDiffs,
		Paths:   normalizedPaths,
	})

	var tarPaths []string
	for _, diff := range filteredDiffs {
		if diff.IsDeleted {
			continue
		}
		if diff.MovedFrom != "" && !diff.IsUpdated {
			continue
		}
		tarPaths = append(tarPaths, diff.Path)
	}

	if len(tarPaths) > 0 {
		streamStart := time.Now()
		if err := s.streamSelectedFiles(tarPaths, onTarData); err != nil {
			return err
		}
		log.Printf("Sandbox.ExportDiff: streamSelectedFiles took %v", time.Since(streamStart))
	}

	return nil
}

// DiscardDiff discards changes in the upper layers matching the specified paths.
// Paths should be absolute paths (with mount path prefix).
// DiscardAllChanges clears the entire upper directory of all writable overlays,
// effectively reverting all workspace modifications.
func (s *Sandbox) DiscardAllChanges() error {
	if s.IsMounted() {
		return fmt.Errorf("sandbox must be unmounted before discarding changes")
	}

	for _, ov := range s.Overlays {
		if ov.MountConfig.ReadOnly || ov.MountConfig.Passthrough {
			continue
		}
		entries, err := os.ReadDir(ov.UpperDir)
		if err != nil {
			return fmt.Errorf("reading upper dir: %w", err)
		}
		for _, entry := range entries {
			entryPath := filepath.Join(ov.UpperDir, entry.Name())
			if err := os.RemoveAll(entryPath); err != nil {
				return fmt.Errorf("removing %s: %w", entryPath, err)
			}
		}
	}

	return nil
}

func normalizePaths(paths []string) []string {
	if len(paths) == 0 {
		return nil
	}
	normalized := make([]string, 0, len(paths))
	for _, p := range paths {
		p = strings.TrimSpace(p)
		p = strings.TrimSuffix(p, "/")
		// Ensure path starts with /
		if p != "" && !strings.HasPrefix(p, "/") {
			p = "/" + p
		}
		if p != "" && p != "/" {
			normalized = append(normalized, p)
		}
	}
	return normalized
}

func filterDiffsByPaths(diffs []overlay.FileDiff, paths []string) []overlay.FileDiff {
	if len(paths) == 0 {
		return diffs
	}

	pathSet := make(map[string]bool, len(paths))
	for _, p := range paths {
		pathSet[p] = true
	}

	var filtered []overlay.FileDiff
	for _, diff := range diffs {
		if pathSet[diff.Path] {
			filtered = append(filtered, diff)
		}
	}
	return filtered
}

func (s *Sandbox) streamSelectedFiles(paths []string, onData func([]byte)) error {
	if len(paths) == 0 {
		return nil
	}

	// Group paths by overlay
	type overlayFile struct {
		ov      *OverlayMount
		relPath string
	}
	var files []overlayFile

	for _, path := range paths {
		ov, relPath := s.getOverlayForPath(path)
		if ov == nil || ov.MountConfig.ReadOnly {
			continue
		}
		files = append(files, overlayFile{ov: ov, relPath: relPath})
	}

	if len(files) == 0 {
		return fmt.Errorf("no writable overlay found for any of the paths")
	}

	// Create a pipe to stream tar data
	pr, pw := io.Pipe()

	// Write tar in goroutine
	errCh := make(chan error, 1)
	go func() {
		tw := tar.NewWriter(pw)
		var writeErr error

		for _, f := range files {
			srcPath := filepath.Join(f.ov.UpperDir, f.relPath)
			// Build tar entry path with mount prefix (without leading /)
			mountPath := strings.TrimPrefix(f.ov.MountConfig.MountPath, "/")
			tarPath := filepath.Join(mountPath, f.relPath)

			if err := s.addToTar(tw, srcPath, tarPath); err != nil {
				writeErr = err
				break
			}
		}

		tw.Close()
		pw.CloseWithError(writeErr)
		errCh <- writeErr
	}()

	// Stream tar data to callback
	// Use 1MB chunks for better throughput (gRPC max msg size is 64MB)
	buf := make([]byte, 1*1024*1024)
	for {
		n, err := pr.Read(buf)
		if n > 0 {
			onData(buf[:n])
		}
		if err != nil {
			break
		}
	}

	return <-errCh
}

// addToTar adds a single file or directory entry to the tar writer.
// This does NOT recurse into directories - each path that needs to be
// included must be explicitly listed in the paths passed to streamSelectedFiles.
// This prevents accidentally packing entire directory trees when only the
// directory itself is new (e.g., created as part of a move operation).
func (s *Sandbox) addToTar(tw *tar.Writer, srcPath, tarPath string) error {
	info, err := os.Lstat(srcPath)
	if err != nil {
		return err
	}

	// Handle symlinks
	var link string
	if info.Mode()&os.ModeSymlink != 0 {
		link, err = os.Readlink(srcPath)
		if err != nil {
			return err
		}
	}

	header, err := tar.FileInfoHeader(info, link)
	if err != nil {
		return err
	}
	header.Name = tarPath

	if err := tw.WriteHeader(header); err != nil {
		return err
	}

	// Write file content for regular files
	if info.Mode().IsRegular() {
		file, err := os.Open(srcPath)
		if err != nil {
			return err
		}
		defer file.Close()

		if _, err := io.Copy(tw, file); err != nil {
			return err
		}
	}

	// Note: We intentionally do NOT recurse into directories.
	// Each file that needs to be transferred should be explicitly
	// listed in tarPaths. This avoids packing entire directory contents
	// when only the directory entry is new (e.g., a target folder for moved files).

	return nil
}
