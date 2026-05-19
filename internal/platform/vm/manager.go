package vm

import (
	"archive/tar"
	"context"
	"fmt"
	"io"
	"log"
	"net"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"time"

	"github.com/Code-Hex/vz/v3"
	"github.com/nora/sandbox-vm/internal/envhost"
	"github.com/nora/sandbox-vm/internal/platform/telemetry"
	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
	"github.com/nora/sandbox-vm/pkg/types"
)

// Manager manages VM lifecycle
type Manager struct {
	vm                atomic.Pointer[vz.VirtualMachine]
	config            *Config
	peer              *vmrpc.Peer // Bidirectional peer for Host-VM communication
	mu                sync.Mutex
	isRunning         bool
	activeMounts      []Mount
	rootfsOverlayPath string
	overlayLockFile   *os.File // File handle for overlay lock (prevents concurrent use)

	// Track mounted sandboxes to prevent operations that require unmounted overlay
	mountedSandboxesMu sync.RWMutex
	mountedSandboxes   map[string]bool

	netstackListener *vz.VirtioSocketListener
	netstackCancel   context.CancelFunc

	serialBlackholeRead *os.File
	serialLogRead       *os.File
	serialLogWrite      *os.File
}

const (
	DefaultRuntimeBridgeGuestPort uint32 = 50080

	runtimeBridgeGuestPortKernelArg = "cue_rt_guest_port"
)

// Mount represents a directory mount configuration for the VM.
type Mount struct {
	HostPath    string // Host path to mount
	VMPath      string // Path inside VM (usually same as HostPath)
	ReadOnly    bool   // Whether to skip overlay (read-only mount)
	Passthrough bool   // Direct write-through to host, no overlay
}

// Config represents VM configuration
type Config struct {
	// VM Resources
	CPUCount   uint
	MemorySize uint64 // in bytes

	// VM Images
	KernelPath  string
	RootfsPath  string
	BootCommand string

	// Proxy Configuration (inherited from host)
	HTTPProxy  string // HTTP proxy URL (e.g., http://host.docker.internal:7890)
	HTTPSProxy string // HTTPS proxy URL
	NoProxy    string // Comma-separated list of hosts to exclude from proxy

	// Guest-local runtime bridge port exposed inside the VM.
	RuntimeBridgeGuestPort uint32

	// Directory mounts (replaces HostWorkDir/VMWorkDir)
	Mounts []Mount

	// Timeout for VM startup
	StartupTimeout time.Duration

	// Rootfs overlay configuration
	RootfsOverlaySize int64  // Size of the rootfs writable overlay (bytes)
	RootfsOverlayDir  string // Directory to store rootfs overlay files (optional)
}

// DefaultConfig returns a default VM configuration
func DefaultConfig(resourcesDir string) *Config {
	return &Config{
		CPUCount:               2,
		MemorySize:             2 * 1024 * 1024 * 1024, // 2GB
		KernelPath:             filepath.Join(resourcesDir, "kernel.bin"),
		RootfsPath:             filepath.Join(resourcesDir, "rootfs.img"),
		BootCommand:            "console=hvc0 root=/dev/vda quiet",
		RuntimeBridgeGuestPort: DefaultRuntimeBridgeGuestPort,
		Mounts:                 nil, // Will be set by caller
		StartupTimeout:         60 * time.Second,
		RootfsOverlaySize:      32 * 1024 * 1024 * 1024, // 32GB rootfs overlay
		RootfsOverlayDir:       "",
	}
}

// NewManager creates a new VM manager
func NewManager(config *Config) (*Manager, error) {
	if config == nil {
		return nil, fmt.Errorf("config cannot be nil")
	}
	normalizeConfig(config)

	// Validate configuration
	if err := validateConfig(config); err != nil {
		return nil, fmt.Errorf("invalid config: %w", err)
	}

	return &Manager{
		config:           config,
		mountedSandboxes: make(map[string]bool),
	}, nil
}

func validateConfig(config *Config) error {
	if config.CPUCount == 0 {
		return fmt.Errorf("CPUCount must be greater than 0")
	}
	if config.MemorySize == 0 {
		return fmt.Errorf("MemorySize must be greater than 0")
	}
	if config.KernelPath == "" {
		return fmt.Errorf("KernelPath is required")
	}
	if _, err := os.Stat(config.KernelPath); os.IsNotExist(err) {
		return fmt.Errorf("kernel file not found: %s", config.KernelPath)
	}
	if config.RootfsPath != "" {
		if _, err := os.Stat(config.RootfsPath); os.IsNotExist(err) {
			return fmt.Errorf("rootfs file not found: %s", config.RootfsPath)
		}
	}
	return nil
}

func (m *Manager) VsockConnect(port uint32) (*vz.VirtioSocketConnection, error) {
	vm := m.vm.Load()
	if vm == nil {
		return nil, fmt.Errorf("VM is not running")
	}

	sock := vm.SocketDevices()[0]
	conn, err := sock.Connect(port)
	if err != nil {
		return nil, fmt.Errorf("vsock connect failed: %w", err)
	}
	return conn, nil
}

// VsockListen creates a listener on the specified vsock port.
// This can be used by external components (like APIProxy) to accept connections from the VM.
func (m *Manager) VsockListen(port uint32) (*vz.VirtioSocketListener, error) {
	vm := m.vm.Load()
	if vm == nil {
		return nil, fmt.Errorf("VM is not running")
	}

	sock := vm.SocketDevices()[0]
	listener, err := sock.Listen(port)
	if err != nil {
		return nil, fmt.Errorf("vsock listen failed: %w", err)
	}
	return listener, nil
}

// Start starts the VM
func (m *Manager) Start(ctx context.Context) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	if m.isRunning {
		return fmt.Errorf("VM is already running")
	}

	// Ensure each mount has VMPath set (defaults to same as HostPath)
	for i := range m.config.Mounts {
		if m.config.Mounts[i].VMPath == "" {
			m.config.Mounts[i].VMPath = m.config.Mounts[i].HostPath
		}
	}
	m.activeMounts = nil

	if err := m.prepareRootfsOverlay(); err != nil {
		return fmt.Errorf("failed to prepare rootfs overlay: %w", err)
	}

	// Create VM configuration
	vmConfig, err := m.createVMConfig()
	if err != nil {
		return fmt.Errorf("failed to create VM config: %w", err)
	}

	// Validate VM configuration
	validated, err := vmConfig.Validate()
	if err != nil {
		return fmt.Errorf("invalid VM configuration: %w", err)
	}
	if !validated {
		return fmt.Errorf("VM configuration validation failed")
	}

	// Create virtual machine
	vm, err := vz.NewVirtualMachine(vmConfig)
	if err != nil {
		return fmt.Errorf("failed to create virtual machine: %w", err)
	}

	m.vm.Store(vm)

	// Start VM
	log.Println("Starting VM...")
	if err := vm.Start(); err != nil {
		return fmt.Errorf("failed to start VM: %w", err)
	}

	// Wait for VM to be running
	if err := m.waitForVMRunning(ctx); err != nil {
		return fmt.Errorf("VM failed to start: %w", err)
	}

	m.isRunning = true
	log.Println("VM started successfully")

	if err := m.startNetworkDataPlane(); err != nil {
		return m.failStartLocked(fmt.Errorf("failed to start VM network data plane: %w", err))
	}

	// Initialize peer and connect to vmd via gRPC
	// This is the primary communication channel with the VM
	if err := m.initPeer(ctx); err != nil {
		return m.failStartLocked(fmt.Errorf("failed to connect to vmd: %w", err))
	}
	log.Println("Connected to VM via gRPC")

	// Setup working directory via gRPC
	if err := m.setupWorkspace(ctx); err != nil {
		return m.failStartLocked(fmt.Errorf("failed to setup workspace: %w", err))
	}

	// Setup proxy environment from host via gRPC
	if err := m.setupProxyEnv(ctx); err != nil {
		log.Printf("Warning: failed to setup proxy environment: %v", err)
	}

	return nil
}

func (m *Manager) failStartLocked(err error) error {
	if m.peer != nil {
		m.peer.Close()
		m.peer = nil
	}

	m.stopNetworkDataPlaneLocked()

	vm := m.vm.Load()
	if vm != nil {
		if stopErr := vm.Stop(); stopErr != nil {
			err = fmt.Errorf("%w; stop VM after start failure: %v", err, stopErr)
		}
		m.vm.Store(nil)
	}
	m.closeSerialConsolePipesLocked()

	if m.overlayLockFile != nil {
		unlockOverlay(m.overlayLockFile)
		m.overlayLockFile = nil
	}

	m.activeMounts = nil
	m.isRunning = false
	return err
}

func (m *Manager) createVMConfig() (*vz.VirtualMachineConfiguration, error) {
	// Create bootloader
	bootloader, err := vz.NewLinuxBootLoader(
		m.config.KernelPath,
		vz.WithCommandLine(buildBootCommand(m.config.BootCommand, m.config.RuntimeBridgeGuestPort)),
	)
	if err != nil {
		return nil, fmt.Errorf("failed to create bootloader: %w", err)
	}

	// Create VM config
	vmConfig, err := vz.NewVirtualMachineConfiguration(
		bootloader,
		m.config.CPUCount,
		m.config.MemorySize,
	)
	if err != nil {
		return nil, fmt.Errorf("failed to create VM configuration: %w", err)
	}

	// Add serial console
	// Isolate hvc from the real stdio to prevent the VM from tainting our PTY
	blackholeR, blackholeW, err := os.Pipe()
	if err != nil {
		return nil, fmt.Errorf("failed to create blackhole pipe: %w", err)
	}
	copyR, copyW, err := os.Pipe()
	if err != nil {
		_ = blackholeR.Close()
		_ = blackholeW.Close()
		return nil, fmt.Errorf("failed to create copy pipe: %w", err)
	}
	cleanupSerialPipes := true
	defer func() {
		if !cleanupSerialPipes {
			return
		}
		_ = blackholeR.Close()
		_ = blackholeW.Close()
		_ = copyR.Close()
		_ = copyW.Close()
	}()
	if err := blackholeW.Close(); err != nil {
		return nil, fmt.Errorf("failed to close blackhole writer: %w", err)
	}
	blackholeW = nil
	go func() {
		_, _ = io.Copy(log.Writer(), copyR)
	}()

	serialAttachment, err := vz.NewFileHandleSerialPortAttachment(blackholeR, copyW)
	if err != nil {
		return nil, fmt.Errorf("failed to create serial attachment: %w", err)
	}

	serialConfig, err := vz.NewVirtioConsoleDeviceSerialPortConfiguration(serialAttachment)
	if err != nil {
		return nil, fmt.Errorf("failed to create serial config: %w", err)
	}
	vmConfig.SetSerialPortsVirtualMachineConfiguration([]*vz.VirtioConsoleDeviceSerialPortConfiguration{
		serialConfig,
	})
	m.closeSerialConsolePipesLocked()
	m.serialBlackholeRead = blackholeR
	m.serialLogRead = copyR
	m.serialLogWrite = copyW
	cleanupSerialPipes = false

	vsock, err := vz.NewVirtioSocketDeviceConfiguration()
	if err != nil {
		return nil, fmt.Errorf("failed to create vsock config: %w", err)
	}
	vmConfig.SetSocketDevicesVirtualMachineConfiguration([]vz.SocketDeviceConfiguration{vsock})

	// Add entropy device
	entropyConfig, err := vz.NewVirtioEntropyDeviceConfiguration()
	if err != nil {
		return nil, fmt.Errorf("failed to create entropy config: %w", err)
	}
	vmConfig.SetEntropyDevicesVirtualMachineConfiguration([]*vz.VirtioEntropyDeviceConfiguration{
		entropyConfig,
	})

	// Add storage device (rootfs)
	storageAttachment, err := vz.NewDiskImageStorageDeviceAttachment(
		m.config.RootfsPath,
		true, // readOnly - Force read-only for rootfs as we use overlay
	)
	if err != nil {
		return nil, fmt.Errorf("failed to create storage attachment: %w", err)
	}

	storageConfig, err := vz.NewVirtioBlockDeviceConfiguration(storageAttachment)
	if err != nil {
		return nil, fmt.Errorf("failed to create storage config: %w", err)
	}

	storageDevices := []vz.StorageDeviceConfiguration{
		storageConfig,
	}

	if m.rootfsOverlayPath != "" {
		tempAttachment, err := vz.NewDiskImageStorageDeviceAttachment(
			m.rootfsOverlayPath,
			false,
		)
		if err != nil {
			return nil, fmt.Errorf("failed to create temp overlay attachment: %w", err)
		}

		tempConfig, err := vz.NewVirtioBlockDeviceConfiguration(tempAttachment)
		if err != nil {
			return nil, fmt.Errorf("failed to create temp overlay config: %w", err)
		}
		storageDevices = append(storageDevices, tempConfig)
	}

	vmConfig.SetStorageDevicesVirtualMachineConfiguration(storageDevices)

	// Add shared directories (multiple mounts)
	if len(m.config.Mounts) > 0 {
		var fsConfigs []vz.DirectorySharingDeviceConfiguration
		for i, mount := range m.config.Mounts {

			// Check if host directory exists
			if _, err := os.Stat(mount.HostPath); os.IsNotExist(err) {
				return nil, fmt.Errorf("mount[%d] host directory not found: %s", i, mount.HostPath)
			}

			// Passthrough mounts use read-write VirtioFS (no overlay)
			// Normal mounts use read-only VirtioFS (writes go through OverlayFS per-session)
			readOnly := !mount.Passthrough
			sharedDir, err := vz.NewSharedDirectory(mount.HostPath, readOnly)
			if err != nil {
				return nil, fmt.Errorf("failed to create shared directory for mount[%d]: %w", i, err)
			}

			dirShare, err := vz.NewSingleDirectoryShare(sharedDir)
			if err != nil {
				return nil, fmt.Errorf("failed to create directory share for mount[%d]: %w", i, err)
			}

			tag := fmt.Sprintf("mount-%d", i)
			fsConfig, err := vz.NewVirtioFileSystemDeviceConfiguration(tag)
			if err != nil {
				return nil, fmt.Errorf("failed to create filesystem device config for mount[%d]: %w", i, err)
			}

			fsConfig.SetDirectoryShare(dirShare)
			fsConfigs = append(fsConfigs, fsConfig)

			log.Printf("Configured mount[%d]: %s -> %s (tag=%s, readOnly=%v)", i, mount.HostPath, mount.VMPath, tag, mount.ReadOnly)
		}

		vmConfig.SetDirectorySharingDevicesVirtualMachineConfiguration(fsConfigs)
	}

	return vmConfig, nil
}

func normalizeConfig(config *Config) {
	if config == nil {
		return
	}
	if config.RuntimeBridgeGuestPort == 0 {
		config.RuntimeBridgeGuestPort = DefaultRuntimeBridgeGuestPort
	}
}

func buildBootCommand(base string, guestPort uint32) string {
	parts := make([]string, 0, 3)
	base = strings.TrimSpace(base)
	if base != "" {
		parts = append(parts, base)
	}
	if guestPort != 0 {
		parts = append(parts, fmt.Sprintf("%s=%d", runtimeBridgeGuestPortKernelArg, guestPort))
	}
	return strings.Join(parts, " ")
}

func (m *Manager) RuntimeBridgeGuestPort() uint32 {
	if m == nil || m.config == nil || m.config.RuntimeBridgeGuestPort == 0 {
		return DefaultRuntimeBridgeGuestPort
	}
	return m.config.RuntimeBridgeGuestPort
}

func (m *Manager) RuntimeBridgeGuestBaseURL() string {
	return fmt.Sprintf("http://127.0.0.1:%d", m.RuntimeBridgeGuestPort())
}

func (m *Manager) vmClient() vmrpc.VMServiceClient {
	if m == nil || m.peer == nil {
		return nil
	}
	return m.peer.VMClient()
}

func (m *Manager) SetRuntimeConfig(ctx context.Context, runtime *envhost.RuntimeConfig) error {
	if m == nil || runtime == nil || strings.TrimSpace(runtime.CapabilityToken) == "" {
		return nil
	}
	client := m.vmClient()
	if client == nil {
		return fmt.Errorf("vm client not connected")
	}
	_, err := client.SetRuntimeBridgeConfig(ctx, &vmrpc.SetRuntimeBridgeConfigRequest{
		CapabilityToken: strings.TrimSpace(runtime.CapabilityToken),
		SessionId:       strings.TrimSpace(runtime.SessionID),
		CallerAgentId:   strings.TrimSpace(runtime.CallerAgentID),
		BackendUrl:      guestReachableURL(runtime.BackendURL),
		BackendApiKey:   strings.TrimSpace(runtime.BackendAPIKey),
	})
	if err != nil {
		return fmt.Errorf("set runtime bridge config RPC: %w", err)
	}
	return nil
}

func (m *Manager) waitForVMRunning(ctx context.Context) error {
	timeout := m.config.StartupTimeout
	if timeout == 0 {
		timeout = 60 * time.Second
	}

	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			return fmt.Errorf("timeout waiting for VM to start")
		case <-ticker.C:
			if m.vm.Load().State() == vz.VirtualMachineStateRunning {
				return nil
			}
		}
	}
}

// tryLockOverlay attempts to acquire an exclusive lock on the overlay file.
// Returns the lock file handle if successful, nil if the file is already locked.
func tryLockOverlay(overlayPath string) *os.File {
	lockPath := overlayPath + ".lock"
	f, err := os.OpenFile(lockPath, os.O_CREATE|os.O_RDWR, 0644)
	if err != nil {
		return nil
	}

	// Try non-blocking exclusive lock
	err = syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB)
	if err != nil {
		f.Close()
		return nil // File is locked by another process
	}

	return f
}

// unlockOverlay releases the lock and closes the lock file.
func unlockOverlay(lockFile *os.File) {
	if lockFile == nil {
		return
	}
	syscall.Flock(int(lockFile.Fd()), syscall.LOCK_UN)
	lockFile.Close()
}

func (m *Manager) prepareRootfsOverlay() error {
	if m.config.RootfsOverlaySize <= 0 {
		return fmt.Errorf("RootfsOverlaySize must be greater than 0")
	}

	baseDir := m.config.RootfsOverlayDir
	if baseDir == "" {
		baseDir = os.TempDir()
	}

	if err := os.MkdirAll(baseDir, 0o755); err != nil {
		return fmt.Errorf("failed to create overlay dir %s: %w", baseDir, err)
	}

	// Use fixed overlay file path
	path := filepath.Join(baseDir, "rootfs-overlay.img")

	// Try to lock the overlay file
	lockFile := tryLockOverlay(path)
	if lockFile == nil {
		return fmt.Errorf("rootfs overlay %s is locked by another process", path)
	}
	m.overlayLockFile = lockFile

	// Create overlay file if it doesn't exist
	if _, err := os.Stat(path); os.IsNotExist(err) {
		f, err := os.Create(path)
		if err != nil {
			unlockOverlay(m.overlayLockFile)
			m.overlayLockFile = nil
			return fmt.Errorf("failed to create overlay file: %w", err)
		}
		if err := f.Truncate(m.config.RootfsOverlaySize); err != nil {
			f.Close()
			os.Remove(path)
			unlockOverlay(m.overlayLockFile)
			m.overlayLockFile = nil
			return fmt.Errorf("failed to resize overlay file: %w", err)
		}
		if err := f.Close(); err != nil {
			os.Remove(path)
			unlockOverlay(m.overlayLockFile)
			m.overlayLockFile = nil
			return fmt.Errorf("failed to close overlay file: %w", err)
		}
		log.Printf("Created new rootfs overlay at %s (%d bytes)", path, m.config.RootfsOverlaySize)
	} else {
		log.Printf("Reusing existing rootfs overlay at %s", path)
	}

	m.rootfsOverlayPath = path
	return nil
}

// setupProxyEnv configures proxy environment variables in the VM via gRPC.
func (m *Manager) setupProxyEnv(ctx context.Context) error {
	if m.peer == nil || !m.peer.IsConnected() {
		return fmt.Errorf("sandbox daemon not connected")
	}

	// Check if any proxy is configured from host
	httpProxy := m.config.HTTPProxy
	httpsProxy := m.config.HTTPSProxy
	noProxy := m.config.NoProxy

	if httpProxy == "" && httpsProxy == "" {
		log.Printf("No proxy configured, skipping proxy environment configuration")
		return nil
	}

	log.Printf("Configuring VM proxy from host: http_proxy=%s, https_proxy=%s", httpProxy, httpsProxy)

	// Configure proxy via gRPC
	_, err := m.peer.VMClient().SetProxyEnv(ctx, &vmrpc.SetProxyEnvRequest{
		HttpProxy:  httpProxy,
		HttpsProxy: httpsProxy,
		NoProxy:    noProxy,
	})
	if err != nil {
		return fmt.Errorf("failed to set proxy env via gRPC: %w", err)
	}

	log.Println("Proxy environment configured in VM from host settings")
	return nil
}

func (m *Manager) setupWorkspace(ctx context.Context) error {
	if m.peer == nil || !m.peer.IsConnected() {
		return fmt.Errorf("sandbox daemon not connected")
	}

	// Mount each shared directory with OverlayFS
	if len(m.config.Mounts) > 0 {
		resp, err := m.peer.VMClient().SetupWorkspaces(ctx, &vmrpc.SetupWorkspacesRequest{
			Mounts: buildWorkspaceSetupMounts(m.config.Mounts, true),
		})
		if err != nil {
			return fmt.Errorf("failed to setup workspaces via gRPC: %w", err)
		}
		m.activeMounts = selectMountedMounts(m.config.Mounts, resp.Results)
		if len(m.activeMounts) == 0 {
			return fmt.Errorf("no workspace mounts were successfully configured")
		}
		for i, result := range resp.Results {
			log.Printf("Workspace[%d] mounted at %s", i, result.MountedPath)
		}
	} else {
		log.Printf("No mounts configured, skipping workspace overlay")
		m.activeMounts = nil
	}

	return nil
}

func selectMountedMounts(mounts []Mount, results []*vmrpc.WorkspaceResult) []Mount {
	if len(mounts) == 0 || len(results) == 0 {
		return nil
	}

	remaining := make(map[string]int, len(mounts))
	for _, result := range results {
		if result == nil {
			continue
		}
		remaining[result.MountedPath]++
	}

	selected := make([]Mount, 0, len(results))
	for _, mount := range mounts {
		if remaining[mount.VMPath] <= 0 {
			continue
		}
		selected = append(selected, mount)
		remaining[mount.VMPath]--
	}
	return selected
}

func buildWorkspaceSetupMounts(mounts []Mount, useVirtioTags bool) []*vmrpc.MountInfo {
	if len(mounts) == 0 {
		return nil
	}

	result := make([]*vmrpc.MountInfo, len(mounts))
	for i, mount := range mounts {
		virtioTag := ""
		if useVirtioTags {
			virtioTag = fmt.Sprintf("mount-%d", i)
		}
		result[i] = &vmrpc.MountInfo{
			VirtioTag:   virtioTag,
			MountPath:   mount.VMPath,
			ReadOnly:    mount.ReadOnly,
			Passthrough: mount.Passthrough,
		}
	}

	return result
}

// GetSandboxState returns the current state for a sandbox.
// This uses gRPC to get the file tree and file diffs from the VM daemon.
func (m *Manager) GetSandboxState(ctx context.Context, sandboxID string) (*types.SandboxState, error) {
	if !m.isRunning {
		return nil, fmt.Errorf("VM is not running")
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return nil, fmt.Errorf("sandbox daemon not connected")
	}

	var fileDiffs []types.FileDiff

	// Get sandbox state
	stateResp, err := m.peer.VMClient().GetSandboxState(ctx, &vmrpc.GetSandboxStateRequest{SandboxId: sandboxID})
	if err == nil {
		// Convert vmrpc.FileDiff to types.FileDiff
		fileDiffs = make([]types.FileDiff, len(stateResp.Changes))
		for i, diff := range stateResp.Changes {
			fileDiffs[i] = types.FileDiff{
				Path:      diff.Path,
				Mode:      diff.Mode,
				IsDir:     diff.IsDir,
				IsUpdated: diff.IsUpdated,
				IsDeleted: diff.IsDeleted,
				MovedFrom: diff.MovedFrom,
				Timestamp: time.Unix(diff.Timestamp, 0),
				Size:      diff.Size,
			}
		}
	} else {
		log.Printf("Get sandbox state failed: %v", err)
	}

	log.Printf("Sandbox %s state: %d file diffs", sandboxID, len(fileDiffs))
	return &types.SandboxState{
		SandboxID: sandboxID,
		FileDiff:  fileDiffs,
	}, nil
}

// EnsureReady verifies the VM is running and the gRPC connection is healthy.
func (m *Manager) EnsureReady(ctx context.Context) error {
	if ctx == nil {
		ctx = context.Background()
	}

	if !m.isRunning {
		return fmt.Errorf("VM is not running")
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return fmt.Errorf("sandbox daemon not connected")
	}

	if _, err := m.peer.VMClient().Health(ctx, &vmrpc.HealthRequest{}); err != nil {
		return fmt.Errorf("VM daemon health check failed: %w", err)
	}

	return nil
}

// CreateSandbox creates a sandbox workspace overlay in the VM.
// All tasks within the same sandbox share this overlay, so file changes accumulate
// across multiple tasks. The sandbox upper layer contains all modifications.
// Returns the generated sandbox ID.
func (m *Manager) CreateSandbox(ctx context.Context) (string, error) {
	if ctx == nil {
		ctx = context.Background()
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return "", fmt.Errorf("sandbox daemon not connected")
	}

	resp, err := m.peer.VMClient().CreateSandbox(ctx, &vmrpc.CreateSandboxRequest{})
	if err != nil {
		return "", fmt.Errorf("create sandbox failed: %w", err)
	}
	log.Printf("Created sandbox %s", resp.SandboxId)
	return resp.SandboxId, nil
}

// SandboxExists checks if a sandbox exists in the VM (can be restored).
func (m *Manager) SandboxExists(ctx context.Context, sandboxID string) (bool, error) {
	if ctx == nil {
		ctx = context.Background()
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return false, fmt.Errorf("sandbox daemon not connected")
	}

	resp, err := m.peer.VMClient().SandboxExists(ctx, &vmrpc.SandboxExistsRequest{SandboxId: sandboxID})
	if err != nil {
		return false, fmt.Errorf("check sandbox exists %s failed: %w", sandboxID, err)
	}
	return resp.Exists, nil
}

// DeleteSandbox removes the sandbox-specific workspace overlay.
func (m *Manager) DeleteSandbox(ctx context.Context, sandboxID string) error {
	if ctx == nil {
		ctx = context.Background()
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return fmt.Errorf("sandbox daemon not connected")
	}

	if _, err := m.peer.VMClient().DeleteSandbox(ctx, &vmrpc.DeleteSandboxRequest{SandboxId: sandboxID}); err != nil {
		return fmt.Errorf("delete sandbox %s failed: %w", sandboxID, err)
	}
	log.Printf("Deleted sandbox %s", sandboxID)
	return nil
}

// MountSandbox mounts the sandbox's overlay filesystem.
// This must be called before executing commands in the sandbox.
// The function is idempotent - calling it multiple times is safe.
// Prerequisites: CreateSandbox must have been called first to set up the sandbox root.
func (m *Manager) MountSandbox(ctx context.Context, sandboxID string) error {
	if ctx == nil {
		ctx = context.Background()
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return fmt.Errorf("sandbox daemon not connected")
	}

	if _, err := m.peer.VMClient().MountSandbox(ctx, &vmrpc.MountSandboxRequest{SandboxId: sandboxID}); err != nil {
		return fmt.Errorf("mount sandbox %s failed: %w", sandboxID, err)
	}

	// Track mounted state
	m.mountedSandboxesMu.Lock()
	m.mountedSandboxes[sandboxID] = true
	m.mountedSandboxesMu.Unlock()

	log.Printf("Mounted sandbox %s", sandboxID)
	return nil
}

// UnmountSandbox unmounts the sandbox's overlay filesystem.
// This should be called after task execution to allow modifications
// to upper/lower directories (e.g., running Housekeeper).
// The function is idempotent - calling it multiple times is safe.
// Note: Sandbox root and system directories remain mounted (cleaned up in DeleteSandbox).
func (m *Manager) UnmountSandbox(ctx context.Context, sandboxID string) error {
	if ctx == nil {
		ctx = context.Background()
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return fmt.Errorf("sandbox daemon not connected")
	}

	if _, err := m.peer.VMClient().UnmountSandbox(ctx, &vmrpc.UnmountSandboxRequest{SandboxId: sandboxID}); err != nil {
		return fmt.Errorf("unmount sandbox %s failed: %w", sandboxID, err)
	}

	// Clear mounted state
	m.mountedSandboxesMu.Lock()
	delete(m.mountedSandboxes, sandboxID)
	m.mountedSandboxesMu.Unlock()

	log.Printf("Unmounted sandbox %s", sandboxID)
	return nil
}

// IsSandboxMounted returns true if the sandbox is currently mounted.
func (m *Manager) IsSandboxMounted(sandboxID string) bool {
	m.mountedSandboxesMu.RLock()
	defer m.mountedSandboxesMu.RUnlock()
	return m.mountedSandboxes[sandboxID]
}

// RunSandboxHousekeeper runs housekeeping on the sandbox's overlay filesystem.
// This cleans up identical files, flattens opaque directories, and removes empty directories.
// The sandbox must be unmounted before calling this.
func (m *Manager) RunSandboxHousekeeper(ctx context.Context, sandboxID string) error {
	if ctx == nil {
		ctx = context.Background()
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return fmt.Errorf("sandbox daemon not connected")
	}

	_, err := m.peer.VMClient().RunSandboxHousekeeper(ctx, &vmrpc.RunSandboxHousekeeperRequest{SandboxId: sandboxID})
	if err != nil {
		return fmt.Errorf("run housekeeper for sandbox %s failed: %w", sandboxID, err)
	}
	log.Printf("Housekeeper completed for sandbox %s", sandboxID)
	return nil
}

// ExecutePython executes Python code in a session with automatic dependency management.
func (m *Manager) ExecutePython(ctx context.Context, req *vmrpc.ExecutePythonRequest) (*vmrpc.ExecutePythonResponse, error) {
	if ctx == nil {
		ctx = context.Background()
	}

	if req != nil {
		req.Env = telemetry.TraceContextFromContext(ctx).InjectEnv(req.Env)
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return nil, fmt.Errorf("sandbox daemon not connected")
	}

	return m.peer.VMClient().ExecutePython(ctx, req)
}

func (m *Manager) ExecutePythonStream(
	ctx context.Context,
	req *vmrpc.ExecutePythonRequest,
	onStdout func([]byte),
	onStderr func([]byte),
	onExit func(int),
) error {
	if ctx == nil {
		ctx = context.Background()
	}

	if req != nil {
		req.Env = telemetry.TraceContextFromContext(ctx).InjectEnv(req.Env)
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return fmt.Errorf("sandbox daemon not connected")
	}

	stream, err := m.peer.VMClient().ExecutePythonStream(ctx, req)
	if err != nil {
		return err
	}

	for {
		resp, recvErr := stream.Recv()
		if recvErr == io.EOF {
			return nil
		}
		if recvErr != nil {
			return recvErr
		}

		switch resp.Type {
		case vmrpc.ExecOutput_STDOUT:
			if onStdout != nil && len(resp.Data) > 0 {
				chunk := make([]byte, len(resp.Data))
				copy(chunk, resp.Data)
				onStdout(chunk)
			}
		case vmrpc.ExecOutput_STDERR:
			if onStderr != nil && len(resp.Data) > 0 {
				chunk := make([]byte, len(resp.Data))
				copy(chunk, resp.Data)
				onStderr(chunk)
			}
		case vmrpc.ExecOutput_EXIT:
			if onExit != nil {
				onExit(int(resp.ExitCode))
			}
		}
	}
}

// initPeer initializes the bidirectional Peer for Host-VM communication.
func (m *Manager) initPeer(ctx context.Context) error {
	m.peer = vmrpc.NewHostPeer(
		nil,
		func(ctx context.Context) (net.Conn, error) {
			return m.VsockConnect(vmrpc.VMServicePort)
		},
	)

	// Connect to vmd with retries
	var connectErr error
	for i := 0; i < 30; i++ {
		if err := m.peer.Connect(ctx); err != nil {
			connectErr = err
			time.Sleep(100 * time.Millisecond)
			continue
		}
		if _, err := m.peer.VMClient().Health(ctx, &vmrpc.HealthRequest{}); err != nil {
			connectErr = err
			m.peer.ResetClient()
			time.Sleep(100 * time.Millisecond)
			continue
		}
		log.Printf("Connected to vmd on port %d", vmrpc.VMServicePort)
		return nil
	}
	m.peer.Close()
	m.peer = nil
	return fmt.Errorf("connect vmd: %w", connectErr)
}

// Stop stops the VM
func (m *Manager) Stop(ctx context.Context) error {
	m.mu.Lock()
	defer m.mu.Unlock()

	vmInstance := m.vm.Load()
	if !m.isRunning &&
		m.peer == nil &&
		vmInstance == nil &&
		m.overlayLockFile == nil &&
		len(m.activeMounts) == 0 &&
		m.netstackListener == nil &&
		m.netstackCancel == nil {
		return nil
	}

	if m.peer != nil {
		m.peer.Close()
		m.peer = nil
	}

	m.stopNetworkDataPlaneLocked()

	// Stop VM
	var stopErr error
	if vmInstance != nil {
		if m.isRunning {
			err := vmInstance.Stop()
			if err != nil {
				stopErr = fmt.Errorf("failed to stop VM: %w", err)
			} else {
				log.Println("VM stopped")
			}
		}
		m.vm.Store(nil)
	}
	m.closeSerialConsolePipesLocked()

	// Release overlay lock
	if m.overlayLockFile != nil {
		unlockOverlay(m.overlayLockFile)
		m.overlayLockFile = nil
	}

	m.activeMounts = nil
	m.isRunning = false
	return stopErr
}

func (m *Manager) closeSerialConsolePipesLocked() {
	files := []*os.File{
		m.serialLogWrite,
		m.serialLogRead,
		m.serialBlackholeRead,
	}
	m.serialLogWrite = nil
	m.serialLogRead = nil
	m.serialBlackholeRead = nil

	for _, file := range files {
		if file != nil {
			_ = file.Close()
		}
	}
}

// ExecInSandbox executes a command inside the specified sandbox.
func (m *Manager) ExecInSandbox(ctx context.Context, sandboxID string, args []string, workDir string) (stdout, stderr string, exitCode int, err error) {
	return m.ExecInSandboxWithEnv(ctx, sandboxID, args, workDir, nil)
}

// ExecInSandboxWithEnv executes a command inside a sandbox with additional env vars.
func (m *Manager) ExecInSandboxWithEnv(ctx context.Context, sandboxID string, args []string, workDir string, extraEnv map[string]string) (stdout, stderr string, exitCode int, err error) {
	if ctx == nil {
		ctx = context.Background()
	}

	if len(args) == 0 {
		return "", "", -1, fmt.Errorf("no command provided")
	}

	if sandboxID == "" {
		return "", "", -1, fmt.Errorf("sandbox ID is required")
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return "", "", -1, fmt.Errorf("sandbox daemon not connected")
	}

	env := telemetry.TraceContextFromContext(ctx).InjectEnv(nil)
	if env == nil {
		env = make(map[string]string)
	}
	for key, value := range extraEnv {
		env[key] = value
	}

	resp, err := m.peer.VMClient().Exec(ctx, &vmrpc.ExecRequest{
		SandboxId:  sandboxID,
		Command:    args,
		WorkingDir: workDir,
		Env:        env,
	})
	if err != nil {
		return "", "", -1, err
	}
	return string(resp.Stdout), string(resp.Stderr), int(resp.ExitCode), nil
}

// ExecInSandboxStreamWithEnv executes a command inside a sandbox and streams stdout/stderr.
func (m *Manager) ExecInSandboxStreamWithEnv(
	ctx context.Context,
	sandboxID string,
	args []string,
	workDir string,
	extraEnv map[string]string,
	onStdout func([]byte),
	onStderr func([]byte),
	onExit func(int),
) error {
	if ctx == nil {
		ctx = context.Background()
	}

	if len(args) == 0 {
		return fmt.Errorf("no command provided")
	}
	if sandboxID == "" {
		return fmt.Errorf("sandbox ID is required")
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return fmt.Errorf("sandbox daemon not connected")
	}

	env := telemetry.TraceContextFromContext(ctx).InjectEnv(nil)
	if env == nil {
		env = make(map[string]string)
	}
	for key, value := range extraEnv {
		env[key] = value
	}

	stream, err := m.peer.VMClient().ExecStream(ctx, &vmrpc.ExecRequest{
		SandboxId:  sandboxID,
		Command:    args,
		WorkingDir: workDir,
		Env:        env,
	})
	if err != nil {
		return err
	}

	for {
		resp, recvErr := stream.Recv()
		if recvErr == io.EOF {
			return nil
		}
		if recvErr != nil {
			return recvErr
		}

		switch resp.Type {
		case vmrpc.ExecOutput_STDOUT:
			if onStdout != nil && len(resp.Data) > 0 {
				chunk := make([]byte, len(resp.Data))
				copy(chunk, resp.Data)
				onStdout(chunk)
			}
		case vmrpc.ExecOutput_STDERR:
			if onStderr != nil && len(resp.Data) > 0 {
				chunk := make([]byte, len(resp.Data))
				copy(chunk, resp.Data)
				onStderr(chunk)
			}
		case vmrpc.ExecOutput_EXIT:
			if onExit != nil {
				onExit(int(resp.ExitCode))
			}
		}
	}
}

// WriteSandboxFile writes content to a file in the sandbox workspace via gRPC.
func (m *Manager) WriteSandboxFile(ctx context.Context, sandboxID, path string, content []byte, appendMode bool) error {
	if m.peer == nil || !m.peer.IsConnected() {
		return fmt.Errorf("sandbox daemon not connected")
	}

	_, err := m.peer.VMClient().WriteSandboxFile(ctx, &vmrpc.WriteSandboxFileRequest{
		SandboxId: sandboxID,
		Path:      path,
		Content:   content,
		Append:    appendMode,
	})
	if err != nil {
		return fmt.Errorf("failed to write file: %w", err)
	}
	return nil
}

func (m *Manager) StreamSandboxFile(ctx context.Context, sandboxID, path string) (vmrpc.VMService_StreamSandboxFileClient, error) {
	if m.peer == nil || !m.peer.IsConnected() {
		return nil, fmt.Errorf("sandbox daemon not connected")
	}

	stream, err := m.peer.VMClient().StreamSandboxFile(ctx, &vmrpc.StreamSandboxFileRequest{
		SandboxId: sandboxID,
		Path:      path,
	})
	if err != nil {
		return nil, fmt.Errorf("failed to stream file: %w", err)
	}
	return stream, nil
}

func (m *Manager) OpenSandboxFileReadStream(ctx context.Context, sandboxID, path string) (envhost.FileReadStream, error) {
	stream, err := m.StreamSandboxFile(ctx, sandboxID, path)
	if err != nil {
		return nil, err
	}
	return newVMReadStream(stream)
}

func (m *Manager) ExportSandboxFile(ctx context.Context, sandboxID, srcPath, dstPath string) (*types.ExportFileResult, error) {
	stream, err := m.StreamSandboxFile(ctx, sandboxID, srcPath)
	if err != nil {
		return nil, fmt.Errorf("stream sandbox file: %w", err)
	}
	return exportSandboxFileStream(ctx, stream, dstPath)
}

func (m *Manager) OpenSandboxFileWriteStream(ctx context.Context, sandboxID, path string, opts envhost.FileWriteOptions) (envhost.FileWriteStream, error) {
	if m.peer == nil || !m.peer.IsConnected() {
		return nil, fmt.Errorf("sandbox daemon not connected")
	}

	stream, err := m.peer.VMClient().UploadSandboxFile(ctx)
	if err != nil {
		return nil, fmt.Errorf("failed to open upload stream: %w", err)
	}
	return newVMWriteStream(stream, sandboxID, path, opts)
}

// DeleteSandboxFile deletes a file from the sandbox workspace via gRPC.
func (m *Manager) DeleteSandboxFile(ctx context.Context, sandboxID, path string) error {
	if m.peer == nil || !m.peer.IsConnected() {
		return fmt.Errorf("sandbox daemon not connected")
	}

	_, err := m.peer.VMClient().DeleteSandboxFile(ctx, &vmrpc.DeleteSandboxFileRequest{
		SandboxId: sandboxID,
		Path:      path,
	})
	if err != nil {
		return fmt.Errorf("failed to delete file: %w", err)
	}
	return nil
}

// SandboxFileExists checks if a file exists in the sandbox workspace via gRPC.
func (m *Manager) SandboxFileExists(ctx context.Context, sandboxID, path string) (bool, error) {
	if m.peer == nil || !m.peer.IsConnected() {
		return false, fmt.Errorf("sandbox daemon not connected")
	}

	resp, err := m.peer.VMClient().SandboxFileExists(ctx, &vmrpc.SandboxFileExistsRequest{
		SandboxId: sandboxID,
		Path:      path,
	})
	if err != nil {
		return false, fmt.Errorf("failed to check file exists: %w", err)
	}
	return resp.Exists, nil
}

// GetCurrentOverlayPath returns the path to the current rootfs overlay image.
func (m *Manager) GetCurrentOverlayPath() string {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.rootfsOverlayPath
}

// GetMounts returns a copy of the configured VM mounts.
func (m *Manager) GetMounts() []Mount {
	m.mu.Lock()
	defer m.mu.Unlock()

	source := m.config.Mounts
	if len(m.activeMounts) > 0 {
		source = m.activeMounts
	}
	mounts := make([]Mount, len(source))
	copy(mounts, source)
	return mounts
}

// IsRunning returns whether the VM is running
func (m *Manager) IsRunning() bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	return m.isRunning
}

// ReadSandboxFile reads a file from the sandbox.
func (m *Manager) ReadSandboxFile(ctx context.Context, sandboxID, path string) ([]byte, error) {
	if !m.isRunning {
		return nil, fmt.Errorf("VM is not running")
	}
	if sandboxID == "" {
		return nil, fmt.Errorf("sandboxID is required")
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return nil, fmt.Errorf("sandbox daemon not connected")
	}

	resp, err := m.peer.VMClient().GetSandboxFile(ctx, &vmrpc.GetSandboxFileRequest{
		SandboxId: sandboxID,
		Path:      path,
	})
	if err != nil {
		return nil, fmt.Errorf("read sandbox file failed: %w", err)
	}

	return resp.Content, nil
}

// ExportSandboxDiff exports workspace diff metadata and tar data for a sandbox.
// paths: optional list of specific paths to export (empty for all changes)
// Returns diff metadata and streams tar data to the output writer.
func (m *Manager) ExportSandboxDiff(ctx context.Context, sandboxID string, paths []string, output io.Writer) (*types.ExportDiffResult, error) {
	overallStart := time.Now()
	defer func() {
		log.Printf("ExportSandboxDiff total took %v", time.Since(overallStart))
	}()

	if !m.isRunning {
		return nil, fmt.Errorf("VM is not running")
	}
	if sandboxID == "" {
		return nil, fmt.Errorf("sandboxID is required")
	}
	if ctx == nil {
		ctx = context.Background()
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return nil, fmt.Errorf("sandbox daemon not connected")
	}

	rpcStart := time.Now()
	stream, err := m.peer.VMClient().ExportSandboxDiff(ctx, &vmrpc.ExportSandboxDiffRequest{
		SandboxId: sandboxID,
		Paths:     paths,
	})
	if err != nil {
		return nil, fmt.Errorf("export sandbox diff failed: %w", err)
	}

	var result *types.ExportDiffResult
	var metadataReceivedAt time.Time
	var totalDataBytes int64

	for {
		resp, err := stream.Recv()
		if err == io.EOF {
			break
		}
		if err != nil {
			return nil, fmt.Errorf("stream receive failed: %w", err)
		}

		switch payload := resp.Payload.(type) {
		case *vmrpc.ExportSandboxDiffResponse_Metadata:
			metadataReceivedAt = time.Now()
			log.Printf("ExportSandboxDiff: received metadata in %v", time.Since(rpcStart))
			// First message: extract metadata
			meta := payload.Metadata
			changes := make([]types.FileDiff, len(meta.Changes))
			for i, diff := range meta.Changes {
				changes[i] = types.FileDiff{
					Path:      diff.Path,
					Mode:      diff.Mode,
					IsDir:     diff.IsDir,
					IsUpdated: diff.IsUpdated,
					IsDeleted: diff.IsDeleted,
					MovedFrom: diff.MovedFrom,
					Timestamp: time.Unix(diff.Timestamp, 0),
					Size:      diff.Size,
				}
			}
			result = &types.ExportDiffResult{
				Changes: changes,
				Paths:   meta.Paths,
			}

		case *vmrpc.ExportSandboxDiffResponse_Data:
			// Subsequent messages: write tar data
			totalDataBytes += int64(len(payload.Data.Data))
			if _, err := output.Write(payload.Data.Data); err != nil {
				return nil, fmt.Errorf("write tar data failed: %w", err)
			}
		}
	}

	if result == nil {
		return nil, fmt.Errorf("no metadata received in stream")
	}

	if !metadataReceivedAt.IsZero() {
		log.Printf("ExportSandboxDiff: received %d bytes tar data in %v", totalDataBytes, time.Since(metadataReceivedAt))
	}
	log.Printf("ExportSandboxDiff completed: %d changes", len(result.Changes))
	return result, nil
}

// DiscardSandboxAllChanges clears all changes from the sandbox's overlay upper layer.
// This reverts all workspace modifications by removing the entire upper directory contents.
func (m *Manager) DiscardSandboxAllChanges(ctx context.Context, sandboxID string) error {
	if !m.isRunning {
		return fmt.Errorf("VM is not running")
	}
	if sandboxID == "" {
		return fmt.Errorf("sandboxID is required")
	}
	if ctx == nil {
		ctx = context.Background()
	}

	if m.peer == nil || !m.peer.IsConnected() {
		return fmt.Errorf("sandbox daemon not connected")
	}

	if _, err := m.peer.VMClient().DiscardSandboxAllChanges(ctx, &vmrpc.DiscardSandboxAllChangesRequest{
		SandboxId: sandboxID,
	}); err != nil {
		return fmt.Errorf("discard sandbox changes failed: %w", err)
	}

	log.Printf("DiscardSandboxAllChanges completed for sandbox %s", sandboxID)
	return nil
}

// ApplySandboxDiff applies workspace changes from a sandbox to the host filesystem.
// paths: optional list of specific paths to apply (empty for all changes)
// hostBaseDir: base directory on host where changes will be applied
//
// Application order (safe):
// 1. Process move operations (if source exists, rename; otherwise extract from tar)
// 2. Extract new/modified files from tar
// 3. Execute deletions (depth-first order)
//
// Note: Uses a temporary file to store tar data to avoid memory pressure
// when dealing with large workspace changes.
func (m *Manager) ApplySandboxDiff(ctx context.Context, sandboxID string, paths []string, hostBaseDir string) error {
	overallStart := time.Now()
	defer func() {
		log.Printf("ApplySandboxDiff total took %v", time.Since(overallStart))
	}()

	if !m.isRunning {
		return fmt.Errorf("VM is not running")
	}

	// Create temporary file for tar data to avoid memory pressure
	tarFile, err := os.CreateTemp("", "nora-diff-*.tar")
	if err != nil {
		return fmt.Errorf("failed to create temp tar file: %w", err)
	}
	defer os.Remove(tarFile.Name())
	defer tarFile.Close()

	// Export diff to temporary file
	diffResult, err := m.ExportSandboxDiff(ctx, sandboxID, paths, tarFile)
	if err != nil {
		return fmt.Errorf("export diff failed: %w", err)
	}

	// Discard accepted changes from the overlay upper layer
	if err := m.DiscardSandboxAllChanges(ctx, sandboxID); err != nil {
		return fmt.Errorf("discard changes failed: %w", err)
	}

	// Apply pure moves first (before extraction, while source files still exist)
	// Pure moves are not included in tar, so we handle them via os.Rename
	if err := m.applyMoves(hostBaseDir, diffResult.Changes); err != nil {
		return fmt.Errorf("apply moves failed: %w", err)
	}

	// Seek back to beginning for extraction
	if _, err := tarFile.Seek(0, 0); err != nil {
		return fmt.Errorf("failed to seek tar file: %w", err)
	}

	// Extract tar to host (creates new/modified files)
	if err := m.extractTar(ctx, tarFile, hostBaseDir); err != nil {
		return fmt.Errorf("extract workspace failed: %w", err)
	}

	// Apply deletions (after files are written - safer order)
	if err := m.applyDeletions(hostBaseDir, diffResult.Changes); err != nil {
		return fmt.Errorf("apply deletions failed: %w", err)
	}

	// Re-run housekeeper to clean up the overlay after discard
	if err := m.RunSandboxHousekeeper(ctx, sandboxID); err != nil {
		return fmt.Errorf("post-apply housekeeper failed: %w", err)
	}

	return nil
}

// applyMoves executes pure move operations (rename without content change).
// These are files with MovedFrom set but IsUpdated=false.
// Must be called before tar extraction while source files still exist on host.
func (m *Manager) applyMoves(hostBaseDir string, changes []types.FileDiff) error {
	var moveCount int
	for _, change := range changes {
		// Only handle pure moves (no content modification)
		if change.MovedFrom == "" || change.IsUpdated || change.IsDeleted {
			continue
		}

		srcPath := filepath.Join(hostBaseDir, change.MovedFrom)
		dstPath := filepath.Join(hostBaseDir, change.Path)

		// Check if source exists on host
		if _, err := os.Stat(srcPath); os.IsNotExist(err) {
			// Source doesn't exist on host - this can happen if:
			// 1. File was created and moved within the VM session
			// 2. Source was already moved/deleted
			// In these cases, the file should be in the tar (as new file)
			log.Printf("Move source %s not found on host, skipping rename", change.MovedFrom)
			continue
		}

		// Ensure destination directory exists
		dstDir := filepath.Dir(dstPath)
		if err := os.MkdirAll(dstDir, 0755); err != nil {
			log.Printf("Warning: failed to create directory %s: %v", dstDir, err)
			continue
		}

		// Perform the rename
		if err := os.Rename(srcPath, dstPath); err != nil {
			log.Printf("Warning: failed to move %s -> %s: %v", change.MovedFrom, change.Path, err)
		} else {
			moveCount++
		}
	}

	if moveCount > 0 {
		log.Printf("Applied %d moves", moveCount)
	}
	return nil
}

// applyDeletions executes deletion operations from the diff.
// Deletions are sorted by path depth descending (delete children before parents).
func (m *Manager) applyDeletions(hostBaseDir string, changes []types.FileDiff) error {
	// Collect deletions
	var deletions []string
	for _, change := range changes {
		if change.IsDeleted {
			deletions = append(deletions, change.Path)
		} else if change.MovedFrom != "" {
			deletions = append(deletions, change.MovedFrom)
		}
	}

	if len(deletions) == 0 {
		return nil
	}

	// Sort by path depth descending (deepest first)
	sort.Slice(deletions, func(i, j int) bool {
		depthI := strings.Count(deletions[i], "/")
		depthJ := strings.Count(deletions[j], "/")
		return depthI > depthJ
	})

	for _, del := range deletions {
		hostPath := filepath.Join(hostBaseDir, del)

		var err error
		// Remove directory and all contents
		err = os.RemoveAll(hostPath)

		if err != nil && !os.IsNotExist(err) {
			log.Printf("Warning: failed to delete %s: %v", del, err)
		}
	}

	log.Printf("Applied %d deletions", len(deletions))
	return nil
}

// extractTar extracts the exported workspace tarball to the target directory
func (m *Manager) extractTar(ctx context.Context, tarball io.Reader, targetHostDir string) error {
	// Create target directory if it doesn't exist
	if err := os.MkdirAll(targetHostDir, 0755); err != nil {
		return fmt.Errorf("failed to create target directory %s: %w", targetHostDir, err)
	}

	tr := tar.NewReader(tarball)
	var fileCount, skipCount int

	for {
		// Check for context cancellation
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}

		header, err := tr.Next()
		if err == io.EOF {
			break // End of archive
		}
		if err != nil {
			return fmt.Errorf("failed to read tar header: %w", err)
		}

		// Clean and validate the path to prevent directory traversal
		cleanPath := filepath.Clean(header.Name)
		if strings.HasPrefix(cleanPath, "..") || filepath.IsAbs(cleanPath) {
			log.Printf("Warning: skipping potentially unsafe path: %s", header.Name)
			skipCount++
			continue
		}

		targetPath := filepath.Join(targetHostDir, cleanPath)

		switch header.Typeflag {
		case tar.TypeDir:
			// Create directory
			if err := os.MkdirAll(targetPath, os.FileMode(header.Mode)); err != nil {
				log.Printf("Warning: failed to create directory %s: %v", targetPath, err)
				skipCount++
			}

		case tar.TypeReg:
			// Create parent directory if needed
			parentDir := filepath.Dir(targetPath)
			if err := os.MkdirAll(parentDir, 0755); err != nil {
				log.Printf("Warning: failed to create parent directory %s: %v", parentDir, err)
				skipCount++
				continue
			}

			// Create and write file
			outFile, err := os.OpenFile(targetPath, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, os.FileMode(header.Mode))
			if err != nil {
				log.Printf("Warning: failed to create file %s: %v", targetPath, err)
				skipCount++
				continue
			}

			// Copy file contents
			if _, err := io.Copy(outFile, tr); err != nil {
				outFile.Close()
				log.Printf("Warning: failed to write file %s: %v", targetPath, err)
				skipCount++
				continue
			}
			outFile.Close()
			fileCount++

		case tar.TypeSymlink:
			// Remove existing symlink if present
			os.Remove(targetPath)

			// Create parent directory if needed
			parentDir := filepath.Dir(targetPath)
			if err := os.MkdirAll(parentDir, 0755); err != nil {
				log.Printf("Warning: failed to create parent directory %s: %v", parentDir, err)
				skipCount++
				continue
			}

			// Create symlink
			if err := os.Symlink(header.Linkname, targetPath); err != nil {
				log.Printf("Warning: failed to create symlink %s -> %s: %v", targetPath, header.Linkname, err)
				skipCount++
			}

		case tar.TypeLink:
			// Handle hard links
			linkTarget := filepath.Join(targetHostDir, header.Linkname)

			// Create parent directory if needed
			parentDir := filepath.Dir(targetPath)
			if err := os.MkdirAll(parentDir, 0755); err != nil {
				log.Printf("Warning: failed to create parent directory %s: %v", parentDir, err)
				skipCount++
				continue
			}

			// Remove existing file if present
			os.Remove(targetPath)

			// Create hard link
			if err := os.Link(linkTarget, targetPath); err != nil {
				log.Printf("Warning: failed to create hard link %s -> %s: %v", targetPath, linkTarget, err)
				skipCount++
			}

		default:
			log.Printf("Skipping unsupported tar entry type %c: %s", header.Typeflag, header.Name)
			skipCount++
		}
	}

	if skipCount > 0 {
		log.Printf("Extracted %d files to %s (skipped %d entries)", fileCount, targetHostDir, skipCount)
	} else {
		log.Printf("Successfully extracted %d files to: %s", fileCount, targetHostDir)
	}
	return nil
}
