package sandbox

import (
	"context"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"text/template"

	"github.com/google/uuid"
	"github.com/nora/sandbox-vm/internal/envhost"
	"github.com/nora/sandbox-vm/pkg/types"
)

const maxSandboxReviewDiffEntries = 100

const environmentDescription = "Isolated local Linux VM with the user's files mounted from the host. Commands run inside the VM, and most file changes stay in an overlay until the user accepts them onto the host. It shares the user's files with the host environment, so a task should choose either Local VM or host for filesystem work, not both."

const environmentTemplate = `You are executing in an isolated local Linux VM (Local VM / sandbox). This is usually the safest environment for tasks that need the user's files. The following directories from the user's computer are mounted into the VM:

**Mounted Directories:**
{{- range .Mounts}}
- {{.VMPath}}{{if .ReadOnly}} (read-only){{else if .Passthrough}} (pass-through){{end}}
{{- end}}

- **Strengths**: you can inspect the user's mounted files, run Linux tools safely, and stage file edits without directly acting on the host.
- **Shared filesystem rule**: this environment and the direct host environment see the same user files. Never operate on both the Local VM and host filesystems in the same task. If Local VM can complete the task, stay here and do not request host access. If the task truly requires host execution, stop using the Local VM filesystem for that task and switch fully to host.
- **Limits**: commands execute inside the VM only. They do not trigger direct host side effects such as launching macOS apps or changing host-only process state.
- **Normal mounts**: Changes are stored in a sandbox overlay filesystem and are NOT immediately written to the user's actual filesystem. The user must explicitly accept changes before they are permanently applied.
- **Pass-through mounts**: Changes are written **directly** to the user's real filesystem — no overlay, no review step. You may freely create, modify, and delete files in pass-through directories without user confirmation.
- **Read-only mounts**: Cannot be modified.

**Working Directory:**
- Your working directory is {{.WorkspaceDir}} (pass-through)
- Relative paths in file tools (read, write, edit) and bash are resolved relative to this directory

**Tool Execution Context:**
- bash/file tools execute inside the VM
- Both absolute and relative paths are supported; relative paths resolve to {{.WorkspaceDir}}
- For Desktop, Documents, and Downloads, use absolute macOS paths under '{{.HostUserHome}}'. For example, '{{.HostUserHome}}/Desktop/report.md', '{{.HostUserHome}}/Documents', and '{{.HostUserHome}}/Downloads'. Use the same absolute paths in file tools and bash.
- DO NOT use ~/ (tilde expands to /root in VM, not user directory)
- DO NOT use $HOME (expands to /root in VM, not actual user home)
- DO NOT modify any content in '{{.HostUserHome}}/Library/Application Support/Nora' or '{{.HostUserHome}}/.nora' (system folders for the application), except for explicitly mounted workspace paths.
- DO NOT access the /mnt or adjust mount configuration, it will disrupt the sandbox environment.
- When the task is mainly about the user's files, prefer this environment over direct host access.

**Path Reporting:**
- When presenting file paths to the user, always use **absolute paths**

**Workspace Changes (Overlay Mounts):**
- Users can view workspace changes and file contents in the chat window; you do not need to show every detail
- Use 'current_changes' tool to review what will be applied before task completion`

// SandboxMount is the public mount description used by sandbox-backed hosts.
type SandboxMount struct {
	HostPath    string
	VMPath      string
	ReadOnly    bool
	Passthrough bool
}

// Backend defines the VM-facing operations required by sandbox hosts.
type Backend interface {
	CreateSandbox(ctx context.Context) (string, error)
	DeleteSandbox(ctx context.Context, sandboxID string) error
	SandboxExists(ctx context.Context, sandboxID string) (bool, error)
	MountSandbox(ctx context.Context, sandboxID string) error
	UnmountSandbox(ctx context.Context, sandboxID string) error
	RunSandboxHousekeeper(ctx context.Context, sandboxID string) error
	ExecInSandboxWithEnv(ctx context.Context, sandboxID string, args []string, workingDir string, envVars map[string]string) (string, string, int, error)
	ExecInSandboxStreamWithEnv(ctx context.Context, sandboxID string, args []string, workingDir string, envVars map[string]string, onStdout func([]byte), onStderr func([]byte), onExit func(int)) error
	ExecutePython(ctx context.Context, sandboxID string, code string, workingDir string, envVars map[string]string) (string, string, int, error)
	ExecutePythonStream(ctx context.Context, sandboxID string, code string, workingDir string, envVars map[string]string, onStdout func([]byte), onStderr func([]byte), onExit func(int)) error
	ReadSandboxFile(ctx context.Context, sandboxID string, path string) ([]byte, error)
	OpenSandboxFileReadStream(ctx context.Context, sandboxID string, path string) (envhost.FileReadStream, error)
	WriteSandboxFile(ctx context.Context, sandboxID string, path string, content []byte, appendMode bool) error
	OpenSandboxFileWriteStream(ctx context.Context, sandboxID string, path string, opts envhost.FileWriteOptions) (envhost.FileWriteStream, error)
	DeleteSandboxFile(ctx context.Context, sandboxID string, path string) error
	SandboxFileExists(ctx context.Context, sandboxID string, path string) (bool, error)
	GetSandboxState(ctx context.Context, sandboxID string) (*types.SandboxState, error)
	ApplySandboxDiff(ctx context.Context, sandboxID string, paths []string, hostBaseDir string) error
	DiscardSandboxAllChanges(ctx context.Context, sandboxID string) error
	IsSandboxMounted(sandboxID string) bool
	GetMounts() []SandboxMount
	SetRuntimeConfig(ctx context.Context, runtime *envhost.RuntimeConfig) error
}

// Host provides sandbox-backed execution without importing internal VM types.
type Host struct {
	info         envhost.HostInfo
	workspaceDir string
	runtimeBase  string
	backend      Backend
	store        *environmentStore
	lockMu       sync.Mutex
	envLocks     map[string]*sync.Mutex
}

func NewHost(hostID, name, label, workspaceDir string, backend Backend, storePath string) *Host {
	hostID = strings.TrimSpace(hostID)
	if hostID == "" {
		hostID = envhost.DefaultSandboxHostID
	}
	name = strings.TrimSpace(name)
	if name == "" {
		name = "Sandbox Host"
	}
	label = strings.TrimSpace(label)
	if label == "" {
		label = "Sandbox"
	}

	return &Host{
		info: envhost.HostInfo{
			ID:        hostID,
			Name:      name,
			Label:     label,
			Type:      envhost.EnvironmentTypeSandbox,
			Available: true,
		},
		workspaceDir: workspaceDir,
		runtimeBase:  strings.TrimRight(envhost.DefaultRuntimeBridgeBaseURL(), "/"),
		backend:      backend,
		store:        newEnvironmentStore(storePath),
		envLocks:     make(map[string]*sync.Mutex),
	}
}

func (s *Host) Close() error {
	return nil
}

func (s *Host) Info() envhost.HostInfo {
	return s.info
}

func (s *Host) SubscribeHostStateChanged(onChange func()) func() {
	return func() {}
}

func (s *Host) CreateEnvironment(ctx context.Context) (envhost.EnvironmentMetadata, error) {
	if s.backend == nil {
		return envhost.EnvironmentMetadata{}, envhost.NewProtocolError(envhost.ErrCodeCapabilityNotSupported, "sandbox backend is not available")
	}

	backendID, err := s.backend.CreateSandbox(ctx)
	if err != nil {
		return envhost.EnvironmentMetadata{}, fmt.Errorf("create sandbox: %w", err)
	}

	envID := uuid.NewString()
	if err := s.store.Put(envID, backendID); err != nil {
		_ = s.backend.DeleteSandbox(ctx, backendID)
		return envhost.EnvironmentMetadata{}, fmt.Errorf("persist sandbox environment mapping: %w", err)
	}

	return s.metadata(envID), nil
}

func (s *Host) RestoreEnvironment(ctx context.Context) (envhost.EnvironmentMetadata, bool, error) {
	if s.backend == nil {
		return envhost.EnvironmentMetadata{}, false, envhost.NewProtocolError(envhost.ErrCodeCapabilityNotSupported, "sandbox backend is not available")
	}

	records := s.store.Records()
	if len(records) == 0 {
		return envhost.EnvironmentMetadata{}, false, nil
	}

	type restoreCandidate struct {
		envID     string
		backendID string
		diffCount int
	}

	candidates := make([]restoreCandidate, 0, len(records))
	for envID, backendID := range records {
		exists, err := s.backend.SandboxExists(ctx, backendID)
		if err != nil {
			return envhost.EnvironmentMetadata{}, false, fmt.Errorf("check sandbox %s exists: %w", backendID, err)
		}
		if !exists {
			continue
		}

		diffCount := 0
		state, err := s.backend.GetSandboxState(ctx, backendID)
		if err != nil {
			log.Printf("sandbox host: failed to inspect restore candidate %s (%s): %v", envID, backendID, err)
		} else if state != nil {
			diffCount = len(state.FileDiff)
		}

		candidates = append(candidates, restoreCandidate{
			envID:     envID,
			backendID: backendID,
			diffCount: diffCount,
		})
	}

	if len(candidates) == 0 {
		for envID := range records {
			if err := s.store.Delete(envID); err != nil {
				return envhost.EnvironmentMetadata{}, false, fmt.Errorf("remove stale sandbox environment mapping: %w", err)
			}
		}
		return envhost.EnvironmentMetadata{}, false, nil
	}

	sort.Slice(candidates, func(i, j int) bool {
		if candidates[i].diffCount != candidates[j].diffCount {
			return candidates[i].diffCount > candidates[j].diffCount
		}
		return candidates[i].envID < candidates[j].envID
	})

	selected := candidates[0]
	if len(records) != 1 || records[selected.envID] != selected.backendID {
		if err := s.store.ReplaceSingle(selected.envID, selected.backendID); err != nil {
			return envhost.EnvironmentMetadata{}, false, fmt.Errorf("compact sandbox environment mapping: %w", err)
		}
	}

	return s.metadata(selected.envID), true, nil
}

func (s *Host) DeleteEnvironment(ctx context.Context, envID string) error {
	return s.withEnvLockRemoving(envID, func() error {
		backendID, err := s.backendID(envID)
		if err != nil {
			return err
		}

		if err := s.backend.DeleteSandbox(ctx, backendID); err != nil {
			return err
		}
		if err := s.store.Delete(envID); err != nil {
			return fmt.Errorf("remove sandbox environment mapping: %w", err)
		}
		return nil
	})
}

func (s *Host) Metadata(ctx context.Context, envID string) (envhost.EnvironmentMetadata, error) {
	var metadata envhost.EnvironmentMetadata
	err := s.withEnvLock(envID, func() error {
		backendID, err := s.backendID(envID)
		if err != nil {
			return err
		}
		exists, err := s.backend.SandboxExists(ctx, backendID)
		if err != nil {
			return fmt.Errorf("check sandbox %s exists: %w", backendID, err)
		}
		if !exists {
			if err := s.store.Delete(envID); err != nil {
				log.Printf("sandbox host: failed to drop stale mapping %s -> %s: %v", envID, backendID, err)
			}
			return envhost.NewProtocolError(envhost.ErrCodeEnvironmentNotFound, fmt.Sprintf("environment %q not found", envID))
		}
		metadata = s.metadata(envID)
		return nil
	})
	if err != nil {
		return envhost.EnvironmentMetadata{}, err
	}
	return metadata, nil
}

func (s *Host) Prompt(ctx context.Context, envID string) (string, error) {
	if _, err := s.backendID(envID); err != nil {
		return "", err
	}
	return renderPrompt(s.backend.GetMounts(), s.workspaceDir), nil
}

func (s *Host) Cleanup(ctx context.Context, envID string) error {
	return s.withEnvLock(envID, func() error {
		backendID, err := s.backendID(envID)
		if err != nil {
			return err
		}
		if err := s.backend.UnmountSandbox(ctx, backendID); err != nil {
			log.Printf("sandbox host: failed to unmount %s: %v", envID, err)
			return err
		}
		if err := s.backend.RunSandboxHousekeeper(ctx, backendID); err != nil {
			log.Printf("sandbox host: failed to run housekeeper for %s: %v", envID, err)
		}
		return nil
	})
}

func (s *Host) ExecuteCommand(ctx context.Context, envID string, args []string, workingDir string, envVars map[string]string, runtime *envhost.RuntimeConfig) (string, string, int, error) {
	var stdout string
	var stderr string
	exitCode := -1
	err := s.withEnvLock(envID, func() error {
		if err := s.prepareRuntime(ctx, runtime); err != nil {
			return err
		}
		backendID, err := s.ensureMounted(ctx, envID)
		if err != nil {
			return err
		}
		stdoutResult, stderrResult, exitCodeResult, err := s.backend.ExecInSandboxWithEnv(ctx, backendID, args, workingDir, envhost.BuildExecutionEnv(envVars, runtime, s.runtimeBase))
		if err != nil {
			return err
		}
		stdout = stdoutResult
		stderr = stderrResult
		exitCode = exitCodeResult
		return nil
	})
	if err != nil {
		return "", "", -1, err
	}
	return stdout, stderr, exitCode, nil
}

func (s *Host) ExecuteCommandStream(
	ctx context.Context,
	envID string,
	args []string,
	workingDir string,
	envVars map[string]string,
	runtime *envhost.RuntimeConfig,
	onStdout func([]byte),
	onStderr func([]byte),
	onExit func(int),
) error {
	return s.withEnvLock(envID, func() error {
		if err := s.prepareRuntime(ctx, runtime); err != nil {
			return err
		}
		backendID, err := s.ensureMounted(ctx, envID)
		if err != nil {
			return err
		}
		return s.backend.ExecInSandboxStreamWithEnv(
			ctx,
			backendID,
			args,
			workingDir,
			envhost.BuildExecutionEnv(envVars, runtime, s.runtimeBase),
			onStdout,
			onStderr,
			onExit,
		)
	})
}

func (s *Host) ExecutePython(ctx context.Context, envID string, code string, envVars map[string]string, runtime *envhost.RuntimeConfig) (string, string, int, error) {
	var stdout string
	var stderr string
	exitCode := -1
	err := s.withEnvLock(envID, func() error {
		if err := s.prepareRuntime(ctx, runtime); err != nil {
			return err
		}
		backendID, err := s.ensureMounted(ctx, envID)
		if err != nil {
			return err
		}
		stdoutResult, stderrResult, exitCodeResult, err := s.backend.ExecutePython(ctx, backendID, code, s.workspaceDir, envhost.BuildExecutionEnv(envVars, runtime, s.runtimeBase))
		if err != nil {
			return err
		}
		stdout = stdoutResult
		stderr = stderrResult
		exitCode = exitCodeResult
		return nil
	})
	if err != nil {
		return "", "", -1, err
	}
	return stdout, stderr, exitCode, nil
}

func (s *Host) ExecutePythonStream(
	ctx context.Context,
	envID string,
	code string,
	envVars map[string]string,
	runtime *envhost.RuntimeConfig,
	onStdout func([]byte),
	onStderr func([]byte),
	onExit func(int),
) error {
	return s.withEnvLock(envID, func() error {
		if err := s.prepareRuntime(ctx, runtime); err != nil {
			return err
		}
		backendID, err := s.ensureMounted(ctx, envID)
		if err != nil {
			return err
		}
		return s.backend.ExecutePythonStream(
			ctx,
			backendID,
			code,
			s.workspaceDir,
			envhost.BuildExecutionEnv(envVars, runtime, s.runtimeBase),
			onStdout,
			onStderr,
			onExit,
		)
	})
}

func (s *Host) ReadFile(ctx context.Context, envID string, path string) ([]byte, error) {
	var content []byte
	err := s.withEnvLock(envID, func() error {
		backendID, err := s.backendID(envID)
		if err != nil {
			return err
		}
		if s.backend.IsSandboxMounted(backendID) {
			content, err = s.backend.ReadSandboxFile(ctx, backendID, resolvePath(s.workspaceDir, path))
			return err
		}
		content, err = s.backend.ReadSandboxFile(ctx, backendID, path)
		if err == nil || filepath.IsAbs(path) {
			return err
		}
		content, err = s.backend.ReadSandboxFile(ctx, backendID, resolvePath(s.workspaceDir, path))
		return err
	})
	if err != nil {
		return nil, err
	}
	return content, nil
}

func (s *Host) OpenFileReadStream(ctx context.Context, envID string, path string) (envhost.FileReadStream, error) {
	var stream envhost.FileReadStream
	err := s.withEnvLock(envID, func() error {
		backendID, err := s.ensureMounted(ctx, envID)
		if err != nil {
			return err
		}
		stream, err = s.backend.OpenSandboxFileReadStream(ctx, backendID, resolvePath(s.workspaceDir, path))
		return err
	})
	if err != nil {
		return nil, err
	}
	return stream, nil
}

func (s *Host) WriteFile(ctx context.Context, envID string, path string, content []byte, appendMode bool) error {
	return s.withEnvLock(envID, func() error {
		backendID, err := s.ensureMounted(ctx, envID)
		if err != nil {
			return err
		}
		return s.backend.WriteSandboxFile(ctx, backendID, resolvePath(s.workspaceDir, path), content, appendMode)
	})
}

func (s *Host) OpenFileWriteStream(ctx context.Context, envID string, path string, opts envhost.FileWriteOptions) (envhost.FileWriteStream, error) {
	var stream envhost.FileWriteStream
	err := s.withEnvLock(envID, func() error {
		backendID, err := s.ensureMounted(ctx, envID)
		if err != nil {
			return err
		}
		stream, err = s.backend.OpenSandboxFileWriteStream(ctx, backendID, resolvePath(s.workspaceDir, path), opts)
		return err
	})
	if err != nil {
		return nil, err
	}
	return stream, nil
}

func (s *Host) DeleteFile(ctx context.Context, envID string, path string) error {
	return s.withEnvLock(envID, func() error {
		backendID, err := s.ensureMounted(ctx, envID)
		if err != nil {
			return err
		}
		return s.backend.DeleteSandboxFile(ctx, backendID, resolvePath(s.workspaceDir, path))
	})
}

func (s *Host) FileExists(ctx context.Context, envID string, path string) (bool, error) {
	exists := false
	err := s.withEnvLock(envID, func() error {
		backendID, err := s.ensureMounted(ctx, envID)
		if err != nil {
			return err
		}
		existsResult, err := s.backend.SandboxFileExists(ctx, backendID, resolvePath(s.workspaceDir, path))
		if err != nil {
			return err
		}
		exists = existsResult
		return nil
	})
	if err != nil {
		return false, err
	}
	return exists, nil
}

func (s *Host) prepareRuntime(ctx context.Context, runtime *envhost.RuntimeConfig) error {
	if s == nil || s.backend == nil || runtime == nil {
		return nil
	}
	if strings.TrimSpace(runtime.CapabilityToken) == "" {
		return nil
	}
	return s.backend.SetRuntimeConfig(ctx, runtime)
}

func (s *Host) GetSandboxState(ctx context.Context, envID string) (*types.SandboxState, error) {
	var state *types.SandboxState
	err := s.withEnvLock(envID, func() error {
		stateResult, err := s.getSandboxStateLocked(ctx, envID)
		if err != nil {
			return err
		}
		state = stateResult
		return nil
	})
	if err != nil {
		return nil, err
	}
	return state, nil
}

func (s *Host) AcceptChanges(ctx context.Context, envID string, paths []string, hostBaseDir string) (*types.AcceptChangesResult, error) {
	var result *types.AcceptChangesResult
	err := s.withEnvLock(envID, func() error {
		backendID, err := s.backendID(envID)
		if err != nil {
			return err
		}
		if s.backend.IsSandboxMounted(backendID) {
			return fmt.Errorf("cannot apply diff while environment %s is in use", envID)
		}

		var totalFiles int
		var preDiff []types.FileDiff
		if preState, err := s.backend.GetSandboxState(ctx, backendID); err == nil {
			totalFiles = len(preState.FileDiff)
			preDiff = append(preDiff, preState.FileDiff...)
		}
		reviewDiff := filterReviewDiffByPaths(preDiff, paths)

		acceptedCount := len(paths)
		if acceptedCount == 0 {
			acceptedCount = totalFiles
		}

		if err := s.backend.ApplySandboxDiff(ctx, backendID, paths, hostBaseDir); err != nil {
			return err
		}
		if err := s.backend.RunSandboxHousekeeper(ctx, backendID); err != nil {
			log.Printf("sandbox host: failed to run housekeeper after accept for %s: %v", envID, err)
		}

		rejectedCount := 0
		var state *types.SandboxState
		if stateResult, err := s.getSandboxStateForBackendLocked(ctx, envID, backendID); err == nil {
			state = stateResult
			if state != nil {
				rejectedCount = len(state.FileDiff)
			}
		}

		summary := fmt.Sprintf("%d files changed, %d accepted, %d rejected.", totalFiles, acceptedCount, rejectedCount)
		result = &types.AcceptChangesResult{
			AcceptedCount: acceptedCount,
			RejectedCount: rejectedCount,
			State:         state,
			ReviewDiff:    reviewDiff,
			Summary:       summary,
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return result, nil
}

func (s *Host) DiscardAllChanges(ctx context.Context, envID string) (*types.DiscardAllChangesResult, error) {
	var result *types.DiscardAllChangesResult
	err := s.withEnvLock(envID, func() error {
		backendID, err := s.backendID(envID)
		if err != nil {
			return err
		}
		if s.backend.IsSandboxMounted(backendID) {
			return fmt.Errorf("cannot discard changes while environment %s is in use", envID)
		}

		totalFiles := 0
		if preState, err := s.backend.GetSandboxState(ctx, backendID); err == nil {
			totalFiles = len(preState.FileDiff)
		}

		if err := s.backend.DiscardSandboxAllChanges(ctx, backendID); err != nil {
			return err
		}

		var state *types.SandboxState
		if stateResult, err := s.getSandboxStateForBackendLocked(ctx, envID, backendID); err == nil {
			state = stateResult
		}

		summary := fmt.Sprintf("%d files changed, 0 accepted, %d rejected.", totalFiles, totalFiles)
		result = &types.DiscardAllChangesResult{
			TotalFiles: totalFiles,
			State:      state,
			Summary:    summary,
		}
		return nil
	})
	if err != nil {
		return nil, err
	}
	return result, nil
}

func (s *Host) ExportFile(ctx context.Context, envID string, srcPath string, dstPath string) (*types.ExportFileResult, error) {
	return nil, envhost.NewProtocolError(envhost.ErrCodeCapabilityNotSupported, "workspace file export not supported")
}

func (s *Host) metadata(envID string) envhost.EnvironmentMetadata {
	return envhost.EnvironmentMetadata{
		ID:           strings.TrimSpace(envID),
		Type:         envhost.EnvironmentTypeSandbox,
		Description:  environmentDescription,
		WorkspaceDir: s.workspaceDir,
		Capabilities: envhost.EnvironmentCapabilities{
			WorkspaceState:  true,
			WorkspaceReview: true,
			WorkspaceFileIO: true,
		},
	}
}

func (s *Host) environmentLock(envID string) *sync.Mutex {
	s.lockMu.Lock()
	defer s.lockMu.Unlock()
	if s.envLocks == nil {
		s.envLocks = make(map[string]*sync.Mutex)
	}
	lock := s.envLocks[envID]
	if lock == nil {
		lock = &sync.Mutex{}
		s.envLocks[envID] = lock
	}
	return lock
}

func (s *Host) deleteEnvironmentLock(envID string, lock *sync.Mutex) {
	s.lockMu.Lock()
	defer s.lockMu.Unlock()
	if current := s.envLocks[envID]; current == lock {
		delete(s.envLocks, envID)
	}
}

func (s *Host) withEnvLock(envID string, fn func() error) error {
	lock := s.environmentLock(envID)
	lock.Lock()
	defer lock.Unlock()
	return fn()
}

func (s *Host) withEnvLockRemoving(envID string, fn func() error) error {
	lock := s.environmentLock(envID)
	lock.Lock()
	defer func() {
		lock.Unlock()
		s.deleteEnvironmentLock(envID, lock)
	}()
	return fn()
}

func (s *Host) getSandboxStateLocked(ctx context.Context, envID string) (*types.SandboxState, error) {
	backendID, err := s.backendID(envID)
	if err != nil {
		return nil, err
	}
	return s.getSandboxStateForBackendLocked(ctx, envID, backendID)
}

func (s *Host) getSandboxStateForBackendLocked(ctx context.Context, envID string, backendID string) (*types.SandboxState, error) {
	state, err := s.backend.GetSandboxState(ctx, backendID)
	if err != nil {
		return nil, err
	}
	if state == nil {
		return nil, nil
	}
	state.SandboxID = envID
	state.EnvironmentID = envID
	return state, nil
}

func (s *Host) ensureMounted(ctx context.Context, envID string) (string, error) {
	backendID, err := s.backendID(envID)
	if err != nil {
		return "", err
	}
	if err := s.backend.MountSandbox(ctx, backendID); err != nil {
		return "", err
	}
	return backendID, nil
}

func (s *Host) backendID(envID string) (string, error) {
	envID = strings.TrimSpace(envID)
	if envID == "" {
		return "", envhost.NewProtocolError(envhost.ErrCodeInvalidRequest, "environment_id is required")
	}
	backendID, ok := s.store.Get(envID)
	if !ok || strings.TrimSpace(backendID) == "" {
		return "", envhost.NewProtocolError(envhost.ErrCodeEnvironmentNotFound, fmt.Sprintf("environment %q not found", envID))
	}
	return backendID, nil
}

func resolvePath(workspaceDir, path string) string {
	if filepath.IsAbs(path) {
		return path
	}
	return filepath.Join(workspaceDir, path)
}

func renderPrompt(mounts []SandboxMount, workspaceDir string) string {
	if len(mounts) == 0 {
		return ""
	}

	mountInfos := make([]SandboxMount, len(mounts))
	copy(mountInfos, mounts)
	for i := range mountInfos {
		if mountInfos[i].VMPath == "" {
			mountInfos[i].VMPath = mountInfos[i].HostPath
		}
	}

	tmpl, err := template.New("sandboxEnvironment").Parse(environmentTemplate)
	if err != nil {
		log.Printf("sandbox host: failed to parse environment prompt template: %v", err)
		return ""
	}

	data := struct {
		Mounts       []SandboxMount
		HostUserHome string
		WorkspaceDir string
	}{
		Mounts:       mountInfos,
		HostUserHome: resolveHostUserHome(mountInfos),
		WorkspaceDir: workspaceDir,
	}

	var rendered strings.Builder
	if err := tmpl.Execute(&rendered, data); err != nil {
		log.Printf("sandbox host: failed to render environment prompt: %v", err)
		return ""
	}

	return strings.TrimSpace(rendered.String())
}

func filterReviewDiffByPaths(diffs []types.FileDiff, paths []string) []types.FileDiff {
	if len(diffs) == 0 {
		return nil
	}
	if len(paths) == 0 {
		result := make([]types.FileDiff, len(diffs))
		copy(result, diffs)
		return result
	}

	normalizedPaths := normalizeReviewPaths(paths)
	if len(normalizedPaths) == 0 {
		return nil
	}

	filtered := make([]types.FileDiff, 0, len(diffs))
	for _, diff := range diffs {
		if pathMatchesAny(diff.Path, normalizedPaths) || (diff.MovedFrom != "" && pathMatchesAny(diff.MovedFrom, normalizedPaths)) {
			filtered = append(filtered, diff)
		}
	}

	if len(filtered) > maxSandboxReviewDiffEntries {
		filtered = filtered[:maxSandboxReviewDiffEntries]
	}
	return filtered
}

func resolveHostUserHome(mounts []SandboxMount) string {
	for _, mount := range mounts {
		if mount.ReadOnly || mount.Passthrough {
			continue
		}
		hostPath := filepath.Clean(strings.TrimSpace(mount.HostPath))
		if isLikelyUserHome(hostPath) {
			return hostPath
		}
	}

	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Clean(home)
}

func isLikelyUserHome(path string) bool {
	if !strings.HasPrefix(path, "/Users/") {
		return false
	}
	rest := strings.TrimPrefix(path, "/Users/")
	return rest != "" && !strings.Contains(rest, "/")
}

func normalizeReviewPaths(paths []string) []string {
	seen := make(map[string]struct{}, len(paths))
	normalized := make([]string, 0, len(paths))
	for _, raw := range paths {
		path := normalizeReviewPath(raw)
		if path == "" {
			continue
		}
		if _, ok := seen[path]; ok {
			continue
		}
		seen[path] = struct{}{}
		normalized = append(normalized, path)
	}
	return normalized
}

func normalizeReviewPath(path string) string {
	path = strings.TrimSpace(path)
	if path == "" {
		return ""
	}
	if path == "/" {
		return "/"
	}
	path = filepath.Clean(path)
	if path == "." {
		return ""
	}
	path = strings.TrimPrefix(path, "./")
	path = strings.TrimPrefix(path, "/")
	return path
}

func pathMatchesAny(candidate string, paths []string) bool {
	candidate = normalizeReviewPath(candidate)
	if candidate == "" {
		return false
	}
	for _, path := range paths {
		if path == "/" || candidate == path || strings.HasPrefix(candidate, path+"/") {
			return true
		}
	}
	return false
}
