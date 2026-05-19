package sandbox

import (
	"context"
	"fmt"
	"log"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/nora/sandbox-vm/internal/envhost"
	"github.com/nora/sandbox-vm/internal/platform/vm"
	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
	"github.com/nora/sandbox-vm/pkg/types"
)

// ManagedHost owns one VM-backed sandbox host and exposes
// the optional local sandbox runtime capability needed by embedding callers.
type ManagedHost struct {
	*Host

	mu     sync.Mutex
	shared *managedHostShared
	closed bool
}

type managedHostShared struct {
	key     string
	host    *Host
	manager *vm.Manager
}

type managedHostRegistryEntry struct {
	shared   *managedHostShared
	refCount int
	creating bool
	closing  bool
	waitCh   chan struct{}
}

var (
	managedHostRegistryMu sync.Mutex
	managedHostRegistry   = map[string]*managedHostRegistryEntry{}
)

// NewManagedHost creates one VM-backed sandbox host and keeps
// VM lifecycle ownership inside the sandbox package.
func NewManagedHost(ctx context.Context, cfg InProcessHostConfig) (*ManagedHost, error) {
	shared, err := acquireManagedHostShared(ctx, cfg)
	if err != nil {
		return nil, err
	}
	return &ManagedHost{
		Host:   shared.host,
		shared: shared,
	}, nil
}

func buildManagedHostShared(ctx context.Context, cfg InProcessHostConfig) (*managedHostShared, error) {
	vmManager, err := createVMManager(ctx, cfg.VMOptions)
	if err != nil {
		return nil, err
	}
	if cfg.InstallSignalHandler {
		vm.SetupSignalHandler(vmManager, nil)
	}
	log.Println("✅ Sandbox mode enabled - each task runs in isolated namespace")

	workspaceDir := strings.TrimSpace(cfg.WorkspaceDir)
	if workspaceDir == "" && cfg.VMOptions != nil {
		workspaceDir = strings.TrimSpace(cfg.VMOptions.WorkspaceDir)
	}

	hostID := strings.TrimSpace(cfg.HostID)
	if hostID == "" {
		hostID = envhost.DefaultSandboxHostID
	}
	hostName := strings.TrimSpace(cfg.HostName)
	if hostName == "" {
		hostName = "Built-in Sandbox Host"
	}
	hostLabel := strings.TrimSpace(cfg.HostLabel)
	if hostLabel == "" {
		hostLabel = "Sandbox"
	}

	return &managedHostShared{
		key: managedHostScopeKey(cfg),
		host: NewHost(
			hostID,
			hostName,
			hostLabel,
			workspaceDir,
			&vmSandboxBackend{manager: vmManager},
			filepath.Join(cfg.MetadataDir, "sandbox-host-store.json"),
		),
		manager: vmManager,
	}, nil
}

// NewInProcessHost creates one VM-backed in-process sandbox host.
func NewInProcessHost(ctx context.Context, cfg InProcessHostConfig) (envhost.EnvironmentHost, error) {
	host, err := NewManagedHost(ctx, cfg)
	if err != nil {
		return nil, err
	}
	return host, nil
}

func (s *ManagedHost) Close() error {
	if s == nil {
		return nil
	}

	s.mu.Lock()
	if s.closed {
		s.mu.Unlock()
		return nil
	}
	s.closed = true
	shared := s.shared
	s.shared = nil
	s.Host = nil
	s.mu.Unlock()

	if shared == nil {
		return nil
	}
	return releaseManagedHostShared(shared.key)
}

func (s *managedHostShared) close() error {
	if s == nil {
		return nil
	}
	if s.host != nil {
		_ = s.host.Close()
	}
	if s.manager == nil {
		return nil
	}
	log.Println("🛑 Stopping VM...")
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := s.manager.Stop(ctx); err != nil {
		log.Printf("Error stopping VM: %v", err)
		return err
	}
	log.Println("✅ VM stopped successfully")
	return nil
}

func (s *ManagedHost) IsRunning() bool {
	if s == nil || s.shared == nil || s.shared.manager == nil {
		return false
	}
	return s.shared.manager.IsRunning()
}

func (s *ManagedHost) HealthCheck(ctx context.Context) error {
	if s == nil || s.shared == nil || s.shared.manager == nil {
		return fmt.Errorf("managed host is closed")
	}
	return s.shared.manager.EnsureReady(ctx)
}

func (s *ManagedHost) CurrentOverlayPath() string {
	if s == nil || s.shared == nil || s.shared.manager == nil {
		return ""
	}
	return s.shared.manager.GetCurrentOverlayPath()
}

func acquireManagedHostShared(ctx context.Context, cfg InProcessHostConfig) (*managedHostShared, error) {
	key := managedHostScopeKey(cfg)
	for {
		managedHostRegistryMu.Lock()
		entry := managedHostRegistry[key]
		switch {
		case entry == nil:
			entry = &managedHostRegistryEntry{
				creating: true,
				waitCh:   make(chan struct{}),
			}
			managedHostRegistry[key] = entry
			managedHostRegistryMu.Unlock()

			shared, err := buildManagedHostShared(ctx, cfg)

			managedHostRegistryMu.Lock()
			if err != nil {
				delete(managedHostRegistry, key)
				close(entry.waitCh)
				managedHostRegistryMu.Unlock()
				return nil, err
			}
			shared.key = key
			entry.shared = shared
			entry.refCount = 1
			entry.creating = false
			close(entry.waitCh)
			managedHostRegistryMu.Unlock()
			return shared, nil
		case entry.creating || entry.closing:
			waitCh := entry.waitCh
			managedHostRegistryMu.Unlock()
			<-waitCh
		default:
			entry.refCount++
			shared := entry.shared
			managedHostRegistryMu.Unlock()
			return shared, nil
		}
	}
}

func releaseManagedHostShared(key string) error {
	if key == "" {
		return nil
	}

	var shared *managedHostShared

	managedHostRegistryMu.Lock()
	entry := managedHostRegistry[key]
	if entry == nil {
		managedHostRegistryMu.Unlock()
		return nil
	}
	entry.refCount--
	if entry.refCount <= 0 {
		shared = entry.shared
		entry.shared = nil
		entry.closing = true
		entry.waitCh = make(chan struct{})
	}
	managedHostRegistryMu.Unlock()

	if shared == nil {
		return nil
	}
	closeErr := shared.close()

	managedHostRegistryMu.Lock()
	entry = managedHostRegistry[key]
	if entry != nil && entry.closing {
		close(entry.waitCh)
		delete(managedHostRegistry, key)
	}
	managedHostRegistryMu.Unlock()

	return closeErr
}

func managedHostScopeKey(cfg InProcessHostConfig) string {
	var builder strings.Builder
	writePart := func(value string) {
		builder.WriteString(strings.TrimSpace(value))
		builder.WriteByte('\n')
	}

	writePart(cfg.MetadataDir)
	writePart(cfg.WorkspaceDir)
	writePart(cfg.HostID)
	writePart(cfg.HostName)
	writePart(cfg.HostLabel)
	builder.WriteString(fmt.Sprintf("%t\n", cfg.InstallSignalHandler))

	if cfg.VMOptions == nil {
		builder.WriteString("<nil>\n")
		return builder.String()
	}

	writePart(cfg.VMOptions.ResourcesDir)
	writePart(cfg.VMOptions.KernelPath)
	writePart(cfg.VMOptions.RootfsPath)
	writePart(cfg.VMOptions.RootfsOverlayDir)
	writePart(cfg.VMOptions.WorkspaceDir)
	writePart(cfg.VMOptions.HTTPProxy)
	writePart(cfg.VMOptions.HTTPSProxy)
	writePart(cfg.VMOptions.NoProxy)
	builder.WriteString(fmt.Sprintf("%d\n", cfg.VMOptions.RuntimeBridgeGuestPort))
	for _, mount := range cfg.VMOptions.Mounts {
		builder.WriteString(fmt.Sprintf(
			"%s|%s|%t|%t\n",
			strings.TrimSpace(mount.HostPath),
			strings.TrimSpace(mount.VMPath),
			mount.ReadOnly,
			mount.Passthrough,
		))
	}
	return builder.String()
}

type vmSandboxBackend struct {
	manager *vm.Manager
}

func (b *vmSandboxBackend) ensureReady(ctx context.Context) error {
	if b == nil || b.manager == nil {
		return nil
	}
	return b.manager.EnsureReady(ctx)
}

func (b *vmSandboxBackend) CreateSandbox(ctx context.Context) (string, error) {
	if err := b.ensureReady(ctx); err != nil {
		return "", err
	}
	return b.manager.CreateSandbox(ctx)
}

func (b *vmSandboxBackend) DeleteSandbox(ctx context.Context, sandboxID string) error {
	if err := b.ensureReady(ctx); err != nil {
		return err
	}
	return b.manager.DeleteSandbox(ctx, sandboxID)
}

func (b *vmSandboxBackend) SandboxExists(ctx context.Context, sandboxID string) (bool, error) {
	if err := b.ensureReady(ctx); err != nil {
		return false, err
	}
	return b.manager.SandboxExists(ctx, sandboxID)
}

func (b *vmSandboxBackend) MountSandbox(ctx context.Context, sandboxID string) error {
	if err := b.ensureReady(ctx); err != nil {
		return err
	}
	return b.manager.MountSandbox(ctx, sandboxID)
}

func (b *vmSandboxBackend) UnmountSandbox(ctx context.Context, sandboxID string) error {
	if err := b.ensureReady(ctx); err != nil {
		return err
	}
	return b.manager.UnmountSandbox(ctx, sandboxID)
}

func (b *vmSandboxBackend) RunSandboxHousekeeper(ctx context.Context, sandboxID string) error {
	if err := b.ensureReady(ctx); err != nil {
		return err
	}
	return b.manager.RunSandboxHousekeeper(ctx, sandboxID)
}

func (b *vmSandboxBackend) ExecInSandboxWithEnv(ctx context.Context, sandboxID string, args []string, workingDir string, envVars map[string]string) (string, string, int, error) {
	if err := b.ensureReady(ctx); err != nil {
		return "", "", -1, err
	}
	return b.manager.ExecInSandboxWithEnv(ctx, sandboxID, args, workingDir, envVars)
}

func (b *vmSandboxBackend) ExecInSandboxStreamWithEnv(
	ctx context.Context,
	sandboxID string,
	args []string,
	workingDir string,
	envVars map[string]string,
	onStdout func([]byte),
	onStderr func([]byte),
	onExit func(int),
) error {
	if err := b.ensureReady(ctx); err != nil {
		return err
	}
	return b.manager.ExecInSandboxStreamWithEnv(ctx, sandboxID, args, workingDir, envVars, onStdout, onStderr, onExit)
}

func (b *vmSandboxBackend) ExecutePython(ctx context.Context, sandboxID string, code string, workingDir string, envVars map[string]string) (string, string, int, error) {
	if err := b.ensureReady(ctx); err != nil {
		return "", "", -1, err
	}
	resp, err := b.manager.ExecutePython(ctx, &vmrpc.ExecutePythonRequest{
		SandboxId:  sandboxID,
		Code:       code,
		WorkingDir: workingDir,
		Env:        envVars,
	})
	if err != nil {
		return "", "", -1, err
	}
	return resp.Stdout, resp.Stderr, int(resp.ExitCode), nil
}

func (b *vmSandboxBackend) ExecutePythonStream(
	ctx context.Context,
	sandboxID string,
	code string,
	workingDir string,
	envVars map[string]string,
	onStdout func([]byte),
	onStderr func([]byte),
	onExit func(int),
) error {
	if err := b.ensureReady(ctx); err != nil {
		return err
	}
	return b.manager.ExecutePythonStream(ctx, &vmrpc.ExecutePythonRequest{
		SandboxId:  sandboxID,
		Code:       code,
		WorkingDir: workingDir,
		Env:        envVars,
	}, onStdout, onStderr, onExit)
}

func (b *vmSandboxBackend) ReadSandboxFile(ctx context.Context, sandboxID string, path string) ([]byte, error) {
	if err := b.ensureReady(ctx); err != nil {
		return nil, err
	}
	return b.manager.ReadSandboxFile(ctx, sandboxID, path)
}

func (b *vmSandboxBackend) OpenSandboxFileReadStream(ctx context.Context, sandboxID string, path string) (envhost.FileReadStream, error) {
	return b.manager.OpenSandboxFileReadStream(ctx, sandboxID, path)
}

func (b *vmSandboxBackend) WriteSandboxFile(ctx context.Context, sandboxID string, path string, content []byte, appendMode bool) error {
	if err := b.ensureReady(ctx); err != nil {
		return err
	}
	return b.manager.WriteSandboxFile(ctx, sandboxID, path, content, appendMode)
}

func (b *vmSandboxBackend) OpenSandboxFileWriteStream(ctx context.Context, sandboxID string, path string, opts envhost.FileWriteOptions) (envhost.FileWriteStream, error) {
	return b.manager.OpenSandboxFileWriteStream(ctx, sandboxID, path, opts)
}

func (b *vmSandboxBackend) DeleteSandboxFile(ctx context.Context, sandboxID string, path string) error {
	if err := b.ensureReady(ctx); err != nil {
		return err
	}
	return b.manager.DeleteSandboxFile(ctx, sandboxID, path)
}

func (b *vmSandboxBackend) SandboxFileExists(ctx context.Context, sandboxID string, path string) (bool, error) {
	if err := b.ensureReady(ctx); err != nil {
		return false, err
	}
	return b.manager.SandboxFileExists(ctx, sandboxID, path)
}

func (b *vmSandboxBackend) GetSandboxState(ctx context.Context, sandboxID string) (*types.SandboxState, error) {
	if err := b.ensureReady(ctx); err != nil {
		return nil, err
	}
	return b.manager.GetSandboxState(ctx, sandboxID)
}

func (b *vmSandboxBackend) ApplySandboxDiff(ctx context.Context, sandboxID string, paths []string, hostBaseDir string) error {
	if err := b.ensureReady(ctx); err != nil {
		return err
	}
	return b.manager.ApplySandboxDiff(ctx, sandboxID, paths, hostBaseDir)
}

func (b *vmSandboxBackend) DiscardSandboxAllChanges(ctx context.Context, sandboxID string) error {
	if err := b.ensureReady(ctx); err != nil {
		return err
	}
	return b.manager.DiscardSandboxAllChanges(ctx, sandboxID)
}

func (b *vmSandboxBackend) IsSandboxMounted(sandboxID string) bool {
	return b.manager.IsSandboxMounted(sandboxID)
}

func (b *vmSandboxBackend) GetMounts() []SandboxMount {
	mounts := b.manager.GetMounts()
	result := make([]SandboxMount, 0, len(mounts))
	for _, mount := range mounts {
		result = append(result, SandboxMount{
			HostPath:    mount.HostPath,
			VMPath:      mount.VMPath,
			ReadOnly:    mount.ReadOnly,
			Passthrough: mount.Passthrough,
		})
	}
	return result
}

func (b *vmSandboxBackend) SetRuntimeConfig(ctx context.Context, runtime *envhost.RuntimeConfig) error {
	if b == nil || b.manager == nil || runtime == nil {
		return nil
	}
	return b.manager.SetRuntimeConfig(ctx, runtime)
}
