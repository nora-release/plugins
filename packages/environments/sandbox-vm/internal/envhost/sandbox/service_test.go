package sandbox

import (
	"context"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/nora/sandbox-vm/internal/envhost"
	"github.com/nora/sandbox-vm/pkg/types"
)

func TestHostUsesDefaultRuntimeBridgeBaseURL(t *testing.T) {
	backend := &runtimeBridgeBackend{}
	service := NewHost("sandbox", "Sandbox Host", "Sandbox", "/workspace", backend, t.TempDir()+"/sandbox-store.json")

	if got := service.runtimeBase; got != envhost.DefaultRuntimeBridgeBaseURL() {
		t.Fatalf("unexpected runtime base: %q", got)
	}

	envVars := envhost.BuildExecutionEnv(nil, &envhost.RuntimeConfig{CapabilityToken: "cap"}, service.runtimeBase)
	if got := envVars[envhost.RuntimeEnvCapabilityURL]; got != envhost.DefaultRuntimeBridgeBaseURL()+"/cap" {
		t.Fatalf("unexpected capability url: %q", got)
	}
}

func TestHostRestoreEnvironment(t *testing.T) {
	service := NewHost("sandbox", "Sandbox Host", "Sandbox", "/workspace", &runtimeBridgeBackend{}, t.TempDir()+"/sandbox-store.json")

	created, err := service.CreateEnvironment(context.Background())
	if err != nil {
		t.Fatalf("CreateEnvironment returned error: %v", err)
	}

	restored, ok, err := service.RestoreEnvironment(context.Background())
	if err != nil {
		t.Fatalf("RestoreEnvironment returned error: %v", err)
	}
	if !ok {
		t.Fatalf("expected environment restore to succeed")
	}
	if restored.ID != created.ID {
		t.Fatalf("unexpected restored environment id: got %q want %q", restored.ID, created.ID)
	}
}

func TestHostRestoreEnvironmentPrefersDirtyLegacyMapping(t *testing.T) {
	backend := &runtimeBridgeBackend{
		existingSandboxes: map[string]bool{
			"sandbox-clean": true,
			"sandbox-dirty": true,
		},
		sandboxStates: map[string]*types.SandboxState{
			"sandbox-clean": {
				FileDiff: []types.FileDiff{{Path: "/workspace/clean.txt"}},
			},
			"sandbox-dirty": {
				FileDiff: []types.FileDiff{
					{Path: "/workspace/dirty-a.txt"},
					{Path: "/workspace/dirty-b.txt"},
				},
			},
		},
	}
	service := NewHost("sandbox", "Sandbox Host", "Sandbox", "/workspace", backend, t.TempDir()+"/sandbox-store.json")
	if err := service.store.Put("env-clean", "sandbox-clean"); err != nil {
		t.Fatalf("store put env-clean returned error: %v", err)
	}
	if err := service.store.Put("env-dirty", "sandbox-dirty"); err != nil {
		t.Fatalf("store put env-dirty returned error: %v", err)
	}

	restored, ok, err := service.RestoreEnvironment(context.Background())
	if err != nil {
		t.Fatalf("RestoreEnvironment returned error: %v", err)
	}
	if !ok {
		t.Fatalf("expected environment restore to succeed")
	}
	if restored.ID != "env-dirty" {
		t.Fatalf("unexpected restored environment id: got %q want %q", restored.ID, "env-dirty")
	}

	envID, backendID, ok := service.store.Single()
	if !ok {
		t.Fatalf("expected legacy mappings to be compacted to a single record")
	}
	if envID != "env-dirty" || backendID != "sandbox-dirty" {
		t.Fatalf("unexpected compacted mapping: env=%q backend=%q", envID, backendID)
	}
}

func TestHostRestoreEnvironmentDropsStaleMappings(t *testing.T) {
	backend := &runtimeBridgeBackend{
		existingSandboxes: map[string]bool{
			"sandbox-gone": false,
		},
	}
	service := NewHost("sandbox", "Sandbox Host", "Sandbox", "/workspace", backend, t.TempDir()+"/sandbox-store.json")
	if err := service.store.Put("env-gone", "sandbox-gone"); err != nil {
		t.Fatalf("store put env-gone returned error: %v", err)
	}

	_, ok, err := service.RestoreEnvironment(context.Background())
	if err != nil {
		t.Fatalf("RestoreEnvironment returned error: %v", err)
	}
	if ok {
		t.Fatalf("expected restore to fail when all mappings are stale")
	}
	if len(service.store.Records()) != 0 {
		t.Fatalf("expected stale mappings to be removed, got %+v", service.store.Records())
	}
}

func TestHostRestoreEnvironmentContinuesOnStateError(t *testing.T) {
	backend := &runtimeBridgeBackend{
		existingSandboxes: map[string]bool{
			"sandbox-errored": true,
			"sandbox-clean":   true,
		},
		sandboxStates: map[string]*types.SandboxState{
			"sandbox-clean": {
				FileDiff: []types.FileDiff{{Path: "/workspace/clean.txt"}},
			},
		},
		sandboxStateErrs: map[string]error{
			"sandbox-errored": io.ErrUnexpectedEOF,
		},
	}
	service := NewHost("sandbox", "Sandbox Host", "Sandbox", "/workspace", backend, t.TempDir()+"/sandbox-store.json")
	if err := service.store.Put("env-errored", "sandbox-errored"); err != nil {
		t.Fatalf("store put env-errored returned error: %v", err)
	}
	if err := service.store.Put("env-clean", "sandbox-clean"); err != nil {
		t.Fatalf("store put env-clean returned error: %v", err)
	}

	restored, ok, err := service.RestoreEnvironment(context.Background())
	if err != nil {
		t.Fatalf("RestoreEnvironment returned error: %v", err)
	}
	if !ok {
		t.Fatalf("expected environment restore to succeed")
	}
	if restored.ID != "env-clean" {
		t.Fatalf("unexpected restored environment id: got %q want %q", restored.ID, "env-clean")
	}
}

func TestHostMetadataDropsStaleSandboxMapping(t *testing.T) {
	backend := &runtimeBridgeBackend{
		existingSandboxes: map[string]bool{
			"sandbox-gone": false,
		},
	}
	service := NewHost("sandbox", "Sandbox Host", "Sandbox", "/workspace", backend, t.TempDir()+"/sandbox-store.json")
	if err := service.store.Put("env-gone", "sandbox-gone"); err != nil {
		t.Fatalf("store put env-gone returned error: %v", err)
	}

	_, err := service.Metadata(context.Background(), "env-gone")
	if err == nil {
		t.Fatal("expected metadata lookup to fail for stale mapping")
	}

	var protocolErr *envhost.ProtocolError
	if !errors.As(err, &protocolErr) || protocolErr.Code != envhost.ErrCodeEnvironmentNotFound {
		t.Fatalf("expected environment_not_found error, got %v", err)
	}
	if len(service.store.Records()) != 0 {
		t.Fatalf("expected stale mapping to be removed, got %+v", service.store.Records())
	}
}

func TestRenderPromptExplainsLocalVMTradeoffs(t *testing.T) {
	prompt := renderPrompt([]SandboxMount{
		{HostPath: "/Users/tester", VMPath: "/Users/tester"},
		{HostPath: "/workspace", VMPath: "/workspace", Passthrough: true},
	}, "/workspace")

	for _, want := range []string{
		"safest environment for tasks that need the user's files",
		"Never operate on both the Local VM and host filesystems in the same task",
		"commands execute inside the VM only",
		"user must explicitly accept changes",
		"/Users/tester/Desktop/report.md",
		"/Users/tester/Documents",
		"/Users/tester/Downloads",
		"prefer this environment over direct host access",
	} {
		if !strings.Contains(prompt, want) {
			t.Fatalf("sandbox prompt missing %q\n%s", want, prompt)
		}
	}
}

func TestHostSerializesMountSensitiveOperationsPerEnvironment(t *testing.T) {
	workspace := t.TempDir()
	sourcePath := filepath.Join(workspace, "cat.txt")
	if err := os.WriteFile(sourcePath, []byte("cat"), 0o644); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}

	backend := &serialMountBackend{filePath: sourcePath}
	service := NewHost("sandbox", "Sandbox Host", "Sandbox", workspace, backend, t.TempDir()+"/sandbox-store.json")

	created, err := service.CreateEnvironment(context.Background())
	if err != nil {
		t.Fatalf("CreateEnvironment returned error: %v", err)
	}

	start := make(chan struct{})
	errCh := make(chan error, 2)
	var wg sync.WaitGroup
	wg.Add(2)

	go func() {
		defer wg.Done()
		<-start
		stream, err := service.OpenFileReadStream(context.Background(), created.ID, "cat.txt")
		if err == nil && stream != nil {
			_ = stream.Close()
		}
		errCh <- err
	}()

	go func() {
		defer wg.Done()
		<-start
		_, err := service.FileExists(context.Background(), created.ID, "cat.txt")
		errCh <- err
	}()

	close(start)
	wg.Wait()
	close(errCh)

	for err := range errCh {
		if err != nil {
			t.Fatalf("expected serialized operations to succeed, got %v", err)
		}
	}
	if got := atomic.LoadInt32(&backend.concurrentMountFailures); got != 0 {
		t.Fatalf("concurrent mount failures = %d, want 0", got)
	}
}

func TestHostAcceptChangesWithEnvLockReturnsUpdatedState(t *testing.T) {
	backend := &reviewBackend{
		runtimeBridgeBackend: runtimeBridgeBackend{
			sandboxStates: map[string]*types.SandboxState{
				"sandbox-1": {
					FileDiff: []types.FileDiff{{Path: "/workspace/cat.txt"}},
				},
			},
		},
		sandboxMounted: map[string]bool{"sandbox-1": false},
	}
	service := NewHost("sandbox", "Sandbox Host", "Sandbox", "/workspace", backend, t.TempDir()+"/sandbox-store.json")

	created, err := service.CreateEnvironment(context.Background())
	if err != nil {
		t.Fatalf("CreateEnvironment returned error: %v", err)
	}

	done := make(chan struct{})
	var result *types.AcceptChangesResult
	var acceptErr error
	go func() {
		result, acceptErr = service.AcceptChanges(context.Background(), created.ID, []string{"/workspace/cat.txt"}, string(filepath.Separator))
		close(done)
	}()

	select {
	case <-done:
	case <-time.After(500 * time.Millisecond):
		t.Fatal("AcceptChanges blocked while holding env lock")
	}

	if acceptErr != nil {
		t.Fatalf("AcceptChanges returned error: %v", acceptErr)
	}
	if result == nil {
		t.Fatal("expected accept result")
	}
	if result.AcceptedCount != 1 {
		t.Fatalf("unexpected accepted count: %d", result.AcceptedCount)
	}
	if result.RejectedCount != 0 {
		t.Fatalf("unexpected rejected count: %d", result.RejectedCount)
	}
	if len(result.ReviewDiff) != 1 || result.ReviewDiff[0].Path != "/workspace/cat.txt" {
		t.Fatalf("unexpected review diff: %+v", result.ReviewDiff)
	}
	if result.State == nil {
		t.Fatal("expected updated sandbox state")
	}
	if len(result.State.FileDiff) != 0 {
		t.Fatalf("expected accepted state to clear file diffs, got %+v", result.State.FileDiff)
	}
	if result.State.EnvironmentID != created.ID || result.State.SandboxID != created.ID {
		t.Fatalf("expected state ids to be rewritten to env id, got sandbox=%q env=%q want %q", result.State.SandboxID, result.State.EnvironmentID, created.ID)
	}

	backend.mu.Lock()
	defer backend.mu.Unlock()
	if backend.applyCalls != 1 {
		t.Fatalf("expected one apply call, got %d", backend.applyCalls)
	}
	if backend.housekeeperCalls != 1 {
		t.Fatalf("expected one housekeeper call, got %d", backend.housekeeperCalls)
	}
	if backend.applyHostBaseDir != string(filepath.Separator) {
		t.Fatalf("unexpected apply host base dir: %q", backend.applyHostBaseDir)
	}
	if len(backend.applyPaths) != 1 || backend.applyPaths[0] != "/workspace/cat.txt" {
		t.Fatalf("unexpected apply paths: %+v", backend.applyPaths)
	}
}

func TestHostDiscardAllChangesWithEnvLockReturnsUpdatedState(t *testing.T) {
	backend := &reviewBackend{
		runtimeBridgeBackend: runtimeBridgeBackend{
			sandboxStates: map[string]*types.SandboxState{
				"sandbox-1": {
					FileDiff: []types.FileDiff{{Path: "/workspace/cat.txt"}},
				},
			},
		},
		sandboxMounted: map[string]bool{"sandbox-1": false},
	}
	service := NewHost("sandbox", "Sandbox Host", "Sandbox", "/workspace", backend, t.TempDir()+"/sandbox-store.json")

	created, err := service.CreateEnvironment(context.Background())
	if err != nil {
		t.Fatalf("CreateEnvironment returned error: %v", err)
	}

	done := make(chan struct{})
	var result *types.DiscardAllChangesResult
	var discardErr error
	go func() {
		result, discardErr = service.DiscardAllChanges(context.Background(), created.ID)
		close(done)
	}()

	select {
	case <-done:
	case <-time.After(500 * time.Millisecond):
		t.Fatal("DiscardAllChanges blocked while holding env lock")
	}

	if discardErr != nil {
		t.Fatalf("DiscardAllChanges returned error: %v", discardErr)
	}
	if result == nil {
		t.Fatal("expected discard result")
	}
	if result.TotalFiles != 1 {
		t.Fatalf("unexpected total file count: %d", result.TotalFiles)
	}
	if result.State == nil {
		t.Fatal("expected updated sandbox state")
	}
	if len(result.State.FileDiff) != 0 {
		t.Fatalf("expected discarded state to clear file diffs, got %+v", result.State.FileDiff)
	}
	if result.State.EnvironmentID != created.ID || result.State.SandboxID != created.ID {
		t.Fatalf("expected state ids to be rewritten to env id, got sandbox=%q env=%q want %q", result.State.SandboxID, result.State.EnvironmentID, created.ID)
	}

	backend.mu.Lock()
	defer backend.mu.Unlock()
	if backend.discardCalls != 1 {
		t.Fatalf("expected one discard call, got %d", backend.discardCalls)
	}
}

type runtimeBridgeBackend struct {
	existingSandboxes map[string]bool
	sandboxStates     map[string]*types.SandboxState
	sandboxStateErrs  map[string]error
	lastRuntime       *envhost.RuntimeConfig
}

type serialMountBackend struct {
	runtimeBridgeBackend
	filePath                string
	activeMounts            int32
	concurrentMountFailures int32
}

type reviewBackend struct {
	runtimeBridgeBackend
	mu               sync.Mutex
	sandboxMounted   map[string]bool
	applyCalls       int
	applyPaths       []string
	applyHostBaseDir string
	discardCalls     int
	housekeeperCalls int
}

func (b *serialMountBackend) MountSandbox(ctx context.Context, sandboxID string) error {
	_ = ctx
	_ = sandboxID
	if atomic.AddInt32(&b.activeMounts, 1) > 1 {
		atomic.AddInt32(&b.concurrentMountFailures, 1)
		atomic.AddInt32(&b.activeMounts, -1)
		return errors.New("concurrent mount")
	}
	time.Sleep(20 * time.Millisecond)
	atomic.AddInt32(&b.activeMounts, -1)
	return nil
}

func (b *serialMountBackend) OpenSandboxFileReadStream(ctx context.Context, sandboxID string, path string) (envhost.FileReadStream, error) {
	_ = ctx
	_ = sandboxID
	_ = path
	return envhost.OpenLocalFileReadStream(b.filePath)
}

func (b *serialMountBackend) SandboxFileExists(ctx context.Context, sandboxID string, path string) (bool, error) {
	_ = ctx
	_ = sandboxID
	_ = path
	return true, nil
}

func (b *runtimeBridgeBackend) CreateSandbox(ctx context.Context) (string, error) {
	_ = ctx
	return "sandbox-1", nil
}

func (b *runtimeBridgeBackend) DeleteSandbox(ctx context.Context, sandboxID string) error {
	_ = ctx
	_ = sandboxID
	return nil
}

func (b *runtimeBridgeBackend) SandboxExists(ctx context.Context, sandboxID string) (bool, error) {
	_ = ctx
	if b.existingSandboxes != nil {
		exists, ok := b.existingSandboxes[sandboxID]
		if ok {
			return exists, nil
		}
		return false, nil
	}
	return true, nil
}

func (b *runtimeBridgeBackend) MountSandbox(ctx context.Context, sandboxID string) error {
	_ = ctx
	_ = sandboxID
	return nil
}

func (b *runtimeBridgeBackend) UnmountSandbox(ctx context.Context, sandboxID string) error {
	_ = ctx
	_ = sandboxID
	return nil
}

func (b *runtimeBridgeBackend) RunSandboxHousekeeper(ctx context.Context, sandboxID string) error {
	_ = ctx
	_ = sandboxID
	return nil
}

func (b *reviewBackend) RunSandboxHousekeeper(ctx context.Context, sandboxID string) error {
	_ = ctx
	_ = sandboxID
	b.mu.Lock()
	defer b.mu.Unlock()
	b.housekeeperCalls++
	return nil
}

func (b *runtimeBridgeBackend) ExecInSandboxWithEnv(ctx context.Context, sandboxID string, args []string, workingDir string, envVars map[string]string) (string, string, int, error) {
	_ = ctx
	_ = sandboxID
	_ = args
	_ = workingDir
	_ = envVars
	return "", "", 0, nil
}

func (b *runtimeBridgeBackend) ExecInSandboxStreamWithEnv(ctx context.Context, sandboxID string, args []string, workingDir string, envVars map[string]string, onStdout func([]byte), onStderr func([]byte), onExit func(int)) error {
	_ = ctx
	_ = sandboxID
	_ = args
	_ = workingDir
	_ = envVars
	_ = onStdout
	_ = onStderr
	if onExit != nil {
		onExit(0)
	}
	return nil
}

func (b *runtimeBridgeBackend) ExecutePython(ctx context.Context, sandboxID string, code string, workingDir string, envVars map[string]string) (string, string, int, error) {
	_ = ctx
	_ = sandboxID
	_ = code
	_ = workingDir
	_ = envVars
	return "", "", 0, nil
}

func (b *runtimeBridgeBackend) ExecutePythonStream(ctx context.Context, sandboxID string, code string, workingDir string, envVars map[string]string, onStdout func([]byte), onStderr func([]byte), onExit func(int)) error {
	_ = ctx
	_ = sandboxID
	_ = code
	_ = workingDir
	_ = envVars
	_ = onStdout
	_ = onStderr
	if onExit != nil {
		onExit(0)
	}
	return nil
}

func (b *runtimeBridgeBackend) ReadSandboxFile(ctx context.Context, sandboxID string, path string) ([]byte, error) {
	_ = ctx
	_ = sandboxID
	_ = path
	return nil, nil
}

func (b *runtimeBridgeBackend) OpenSandboxFileReadStream(ctx context.Context, sandboxID string, path string) (envhost.FileReadStream, error) {
	_ = ctx
	_ = sandboxID
	_ = path
	return nil, envhost.NewProtocolError(envhost.ErrCodeCapabilityNotSupported, "streaming not supported in test backend")
}

func (b *runtimeBridgeBackend) WriteSandboxFile(ctx context.Context, sandboxID string, path string, content []byte, appendMode bool) error {
	_ = ctx
	_ = sandboxID
	_ = path
	_ = content
	_ = appendMode
	return nil
}

func (b *runtimeBridgeBackend) OpenSandboxFileWriteStream(ctx context.Context, sandboxID string, path string, opts envhost.FileWriteOptions) (envhost.FileWriteStream, error) {
	_ = ctx
	_ = sandboxID
	_ = path
	_ = opts
	return nil, envhost.NewProtocolError(envhost.ErrCodeCapabilityNotSupported, "streaming not supported in test backend")
}

func (b *runtimeBridgeBackend) DeleteSandboxFile(ctx context.Context, sandboxID string, path string) error {
	_ = ctx
	_ = sandboxID
	_ = path
	return nil
}

func (b *runtimeBridgeBackend) SandboxFileExists(ctx context.Context, sandboxID string, path string) (bool, error) {
	_ = ctx
	_ = sandboxID
	_ = path
	return false, nil
}

func (b *runtimeBridgeBackend) GetSandboxState(ctx context.Context, sandboxID string) (*types.SandboxState, error) {
	_ = ctx
	if b.sandboxStateErrs != nil {
		if err, ok := b.sandboxStateErrs[sandboxID]; ok {
			return nil, err
		}
	}
	if b.sandboxStates != nil {
		if state, ok := b.sandboxStates[sandboxID]; ok {
			return state, nil
		}
	}
	return nil, nil
}

func (b *runtimeBridgeBackend) ApplySandboxDiff(ctx context.Context, sandboxID string, paths []string, hostBaseDir string) error {
	_ = ctx
	_ = sandboxID
	_ = paths
	_ = hostBaseDir
	return nil
}

func (b *reviewBackend) ApplySandboxDiff(ctx context.Context, sandboxID string, paths []string, hostBaseDir string) error {
	_ = ctx
	b.mu.Lock()
	defer b.mu.Unlock()
	b.applyCalls++
	b.applyPaths = append([]string(nil), paths...)
	b.applyHostBaseDir = hostBaseDir
	if b.sandboxStates == nil {
		b.sandboxStates = make(map[string]*types.SandboxState)
	}
	b.sandboxStates[sandboxID] = &types.SandboxState{}
	return nil
}

func (b *runtimeBridgeBackend) DiscardSandboxAllChanges(ctx context.Context, sandboxID string) error {
	_ = ctx
	_ = sandboxID
	return nil
}

func (b *reviewBackend) DiscardSandboxAllChanges(ctx context.Context, sandboxID string) error {
	_ = ctx
	b.mu.Lock()
	defer b.mu.Unlock()
	b.discardCalls++
	if b.sandboxStates == nil {
		b.sandboxStates = make(map[string]*types.SandboxState)
	}
	b.sandboxStates[sandboxID] = &types.SandboxState{}
	return nil
}

func (b *runtimeBridgeBackend) IsSandboxMounted(sandboxID string) bool {
	_ = sandboxID
	return true
}

func (b *reviewBackend) IsSandboxMounted(sandboxID string) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	if b.sandboxMounted != nil {
		if mounted, ok := b.sandboxMounted[sandboxID]; ok {
			return mounted
		}
	}
	return b.runtimeBridgeBackend.IsSandboxMounted(sandboxID)
}

func (b *runtimeBridgeBackend) GetMounts() []SandboxMount {
	return nil
}

func (b *runtimeBridgeBackend) SetRuntimeConfig(ctx context.Context, runtime *envhost.RuntimeConfig) error {
	_ = ctx
	b.lastRuntime = envhost.CloneRuntimeConfig(runtime)
	return nil
}
