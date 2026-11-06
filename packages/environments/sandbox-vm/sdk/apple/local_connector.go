package sandboxvm

import (
	"encoding/json"
	"fmt"
	"strings"
	"sync"

	"github.com/nora/sandbox-vm/internal/localconnector"
)

// LocalConnectorConfig bootstraps one long-lived VM-backed local connector.
type LocalConnectorConfig struct {
	MetadataDir        string
	RootPath           string
	MountsJSON         string
	KernelPath         string
	RootfsPath         string
	RootfsOverlayDir   string
	BackendURL         string
	BackendAPIKey      string
	ReadMaxBytes       int
	MaxMatches         int
	ExecOutputMaxBytes int
}

// SharedLocalConnectorRuntime exposes one shared VM-backed host with one
// sandbox environment per session.
type SharedLocalConnectorRuntime struct {
	mu       sync.Mutex
	scopeKey string
	closed   bool
}

type sharedRuntimeRegistryEntry struct {
	runtime  *localconnector.SharedRuntime
	refCount int
	creating bool
	closing  bool
	waitCh   chan struct{}
}

var (
	sharedRuntimeRegistryMu sync.Mutex
	sharedRuntimeRegistry   = map[string]*sharedRuntimeRegistryEntry{}

	newSharedLocalConnectorRuntime = localconnector.NewShared
	closeSharedLocalConnector      = func(runtime *localconnector.SharedRuntime) error {
		return runtime.Close()
	}
)

// NewSharedLocalConnectorRuntime creates one shared VM-backed host and provisions one environment per session.
func NewSharedLocalConnectorRuntime(cfg *LocalConnectorConfig) (*SharedLocalConnectorRuntime, error) {
	internalCfg := makeInternalLocalConnectorConfig(cfg)
	scopeKey := sharedLocalConnectorScopeKey(internalCfg)

	if _, err := acquireSharedLocalConnectorRuntime(scopeKey, internalCfg); err != nil {
		return nil, err
	}
	return &SharedLocalConnectorRuntime{scopeKey: scopeKey}, nil
}

// Close tears down all session environments while preserving their mappings and closes the shared sandbox host.
func (r *SharedLocalConnectorRuntime) Close() error {
	if r == nil {
		return nil
	}

	r.mu.Lock()
	if r.closed {
		r.mu.Unlock()
		return nil
	}
	r.closed = true
	scopeKey := r.scopeKey
	r.scopeKey = ""
	r.mu.Unlock()

	return releaseSharedLocalConnectorRuntime(scopeKey)
}

// CloseSessionPreservingState releases one session runtime while keeping its persisted sandbox mapping intact.
func (r *SharedLocalConnectorRuntime) CloseSessionPreservingState(sessionID string) error {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return err
	}
	return runtime.CloseSessionPreservingState(sessionID)
}

// DeleteSessionState removes one session's environment and persisted mapping.
func (r *SharedLocalConnectorRuntime) DeleteSessionState(sessionID string) error {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return err
	}
	return runtime.DeleteSessionState(sessionID)
}

// HasSessionState reports whether the shared runtime has persisted state for one session.
func (r *SharedLocalConnectorRuntime) HasSessionState(sessionID string) bool {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return false
	}
	return runtime.HasSessionState(sessionID)
}

// UpdateBackendConfig refreshes the backend URL and API key used for future runtime-scoped bridge calls.
func (r *SharedLocalConnectorRuntime) UpdateBackendConfig(backendURL string, backendAPIKey string) error {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return err
	}
	runtime.UpdateBackendConfig(backendURL, backendAPIKey)
	return nil
}

// HealthCheck verifies that the shared VM runtime and daemon transport are responsive.
func (r *SharedLocalConnectorRuntime) HealthCheck() error {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return err
	}
	return runtime.HealthCheck()
}

// ReadJSON reads one file relative to the configured root and returns a JSON payload with utf8/base64 content.
func (r *SharedLocalConnectorRuntime) ReadJSON(sessionID string, path string, offset int, limit int) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	return runtime.ReadJSON(sessionID, path, offset, limit)
}

// OpenReadStreamJSON opens one file relative to the configured root for chunked streaming reads.
func (r *SharedLocalConnectorRuntime) OpenReadStreamJSON(sessionID string, path string) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	return runtime.OpenReadStreamJSON(sessionID, path)
}

// ReadStreamChunkJSON reads the next chunk from one previously opened read stream.
func (r *SharedLocalConnectorRuntime) ReadStreamChunkJSON(sessionID string, streamID string, maxBytes int) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	return runtime.ReadStreamChunkJSON(sessionID, streamID, maxBytes)
}

// CloseReadStream closes one previously opened read stream.
func (r *SharedLocalConnectorRuntime) CloseReadStream(sessionID string, streamID string) error {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return err
	}
	return runtime.CloseReadStream(sessionID, streamID)
}

// WriteJSON writes utf8 or base64 content to one file relative to the configured root and returns a JSON result.
func (r *SharedLocalConnectorRuntime) WriteJSON(sessionID string, path string, content string, encoding string, mode int) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	return runtime.WriteJSON(sessionID, path, content, encoding, mode)
}

// DeleteJSON removes one file or directory relative to the configured root.
func (r *SharedLocalConnectorRuntime) DeleteJSON(sessionID string, path string, recursive bool) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	return runtime.DeleteJSON(sessionID, path, recursive)
}

// StatJSON returns a JSON description of one file or directory relative to the configured root.
func (r *SharedLocalConnectorRuntime) StatJSON(sessionID string, path string) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	return runtime.StatJSON(sessionID, path)
}

// ListJSON lists entries under one directory relative to the configured root.
func (r *SharedLocalConnectorRuntime) ListJSON(sessionID string, path string) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	return runtime.ListJSON(sessionID, path)
}

// GlobJSON returns a JSON payload of file matches rooted at the configured root or one child directory.
func (r *SharedLocalConnectorRuntime) GlobJSON(sessionID string, pattern string, path string) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	return runtime.GlobJSON(sessionID, pattern, path)
}

// GrepJSON returns JSON match objects rooted at the configured root or one child directory.
func (r *SharedLocalConnectorRuntime) GrepJSON(sessionID string, pattern string, path string, glob string) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	return runtime.GrepJSON(sessionID, pattern, path, glob)
}

// ExecJSON executes one shell command inside the long-lived sandbox via `sh -lc` and returns a JSON result.
func (r *SharedLocalConnectorRuntime) ExecJSON(sessionID string, command string, workingDir string, timeoutSeconds int) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	return runtime.ExecJSON(sessionID, command, workingDir, timeoutSeconds)
}

// ExecWithRuntimeJSON executes one shell command with an optional runtime capability scope.
func (r *SharedLocalConnectorRuntime) ExecWithRuntimeJSON(sessionID string, command string, workingDir string, timeoutSeconds int, capabilitySessionID string, callerAgentID string) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	return runtime.ExecWithRuntimeJSON(sessionID, command, workingDir, timeoutSeconds, capabilitySessionID, callerAgentID)
}

// ExecWithRuntimeEnvJSON executes one shell command with explicit environment overrides and an optional runtime capability scope.
func (r *SharedLocalConnectorRuntime) ExecWithRuntimeEnvJSON(sessionID string, command string, workingDir string, timeoutSeconds int, envJSON string, capabilitySessionID string, callerAgentID string) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	envVars, err := decodeExecutionEnvJSON(envJSON)
	if err != nil {
		return "", err
	}
	return runtime.ExecWithRuntimeEnvJSON(sessionID, command, workingDir, timeoutSeconds, envVars, capabilitySessionID, callerAgentID)
}

// Cleanup unmounts one session environment and runs housekeeping so workspace review is safe.
func (r *SharedLocalConnectorRuntime) Cleanup(sessionID string) error {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return err
	}
	return runtime.Cleanup(sessionID)
}

// GetSandboxStateJSON returns one session's current workspace-review state as JSON.
func (r *SharedLocalConnectorRuntime) GetSandboxStateJSON(sessionID string) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	return runtime.GetSandboxStateJSON(sessionID)
}

// AcceptChangesJSON applies the selected workspace changes for one session and returns the review result as JSON.
func (r *SharedLocalConnectorRuntime) AcceptChangesJSON(sessionID string, pathsJSON string) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	return runtime.AcceptChangesJSON(sessionID, pathsJSON)
}

// DiscardAllChangesJSON discards all workspace changes for one session and returns the result as JSON.
func (r *SharedLocalConnectorRuntime) DiscardAllChangesJSON(sessionID string) (string, error) {
	runtime, err := r.requireSharedRuntime()
	if err != nil {
		return "", err
	}
	return runtime.DiscardAllChangesJSON(sessionID)
}

func (r *SharedLocalConnectorRuntime) requireSharedRuntime() (*localconnector.SharedRuntime, error) {
	if r == nil {
		return nil, fmt.Errorf("shared local connector runtime is not initialized")
	}

	r.mu.Lock()
	closed := r.closed
	scopeKey := r.scopeKey
	r.mu.Unlock()
	if closed || scopeKey == "" {
		return nil, fmt.Errorf("shared local connector runtime is not initialized")
	}

	sharedRuntimeRegistryMu.Lock()
	defer sharedRuntimeRegistryMu.Unlock()

	entry := sharedRuntimeRegistry[scopeKey]
	if entry == nil || entry.creating || entry.runtime == nil {
		return nil, fmt.Errorf("shared local connector runtime is not initialized")
	}
	return entry.runtime, nil
}

func makeInternalLocalConnectorConfig(cfg *LocalConnectorConfig) localconnector.Config {
	var internalCfg localconnector.Config
	if cfg == nil {
		return internalCfg
	}
	internalCfg = localconnector.Config{
		MetadataDir:        cfg.MetadataDir,
		RootPath:           cfg.RootPath,
		Mounts:             decodeMountsJSON(cfg.MountsJSON),
		KernelPath:         cfg.KernelPath,
		RootfsPath:         cfg.RootfsPath,
		RootfsOverlayDir:   cfg.RootfsOverlayDir,
		BackendURL:         cfg.BackendURL,
		BackendAPIKey:      cfg.BackendAPIKey,
		ReadMaxBytes:       cfg.ReadMaxBytes,
		MaxMatches:         cfg.MaxMatches,
		ExecOutputMaxBytes: cfg.ExecOutputMaxBytes,
	}
	return internalCfg
}

func sharedLocalConnectorScopeKey(cfg localconnector.Config) string {
	return fmt.Sprintf(
		"%s|%s|%v|%s|%s|%s",
		cfg.MetadataDir,
		cfg.RootPath,
		cfg.Mounts,
		cfg.KernelPath,
		cfg.RootfsPath,
		cfg.RootfsOverlayDir,
	)
}

type localConnectorMountJSON struct {
	HostPath    string `json:"host_path"`
	VMPath      string `json:"vm_path"`
	ReadOnly    bool   `json:"read_only"`
	Passthrough bool   `json:"passthrough"`
}

func decodeMountsJSON(raw string) []localconnector.Mount {
	raw = strings.TrimSpace(raw)
	if raw == "" {
		return nil
	}
	var payload []localConnectorMountJSON
	if err := json.Unmarshal([]byte(raw), &payload); err != nil {
		return nil
	}
	result := make([]localconnector.Mount, 0, len(payload))
	for _, mount := range payload {
		result = append(result, localconnector.Mount{
			HostPath:    mount.HostPath,
			VMPath:      mount.VMPath,
			ReadOnly:    mount.ReadOnly,
			Passthrough: mount.Passthrough,
		})
	}
	return result
}

func acquireSharedLocalConnectorRuntime(scopeKey string, cfg localconnector.Config) (*localconnector.SharedRuntime, error) {
	for {
		sharedRuntimeRegistryMu.Lock()
		entry := sharedRuntimeRegistry[scopeKey]
		switch {
		case entry == nil:
			entry = &sharedRuntimeRegistryEntry{
				creating: true,
				waitCh:   make(chan struct{}),
			}
			sharedRuntimeRegistry[scopeKey] = entry
			sharedRuntimeRegistryMu.Unlock()

			runtime, err := newSharedLocalConnectorRuntime(cfg)

			sharedRuntimeRegistryMu.Lock()
			if err != nil {
				delete(sharedRuntimeRegistry, scopeKey)
				close(entry.waitCh)
				sharedRuntimeRegistryMu.Unlock()
				return nil, err
			}
			entry.runtime = runtime
			entry.refCount = 1
			entry.creating = false
			close(entry.waitCh)
			sharedRuntimeRegistryMu.Unlock()
			return runtime, nil
		case entry.creating || entry.closing:
			waitCh := entry.waitCh
			sharedRuntimeRegistryMu.Unlock()
			<-waitCh
		default:
			entry.refCount++
			runtime := entry.runtime
			sharedRuntimeRegistryMu.Unlock()
			return runtime, nil
		}
	}
}

func decodeExecutionEnvJSON(envJSON string) (map[string]string, error) {
	envJSON = strings.TrimSpace(envJSON)
	if envJSON == "" || envJSON == "null" {
		return nil, nil
	}

	var envVars map[string]string
	if err := json.Unmarshal([]byte(envJSON), &envVars); err != nil {
		return nil, fmt.Errorf("decode exec env: %w", err)
	}
	if len(envVars) == 0 {
		return nil, nil
	}
	return envVars, nil
}

func releaseSharedLocalConnectorRuntime(scopeKey string) error {
	if scopeKey == "" {
		return nil
	}

	var runtime *localconnector.SharedRuntime

	sharedRuntimeRegistryMu.Lock()
	entry := sharedRuntimeRegistry[scopeKey]
	if entry == nil {
		sharedRuntimeRegistryMu.Unlock()
		return nil
	}
	entry.refCount--
	if entry.refCount <= 0 {
		runtime = entry.runtime
		entry.runtime = nil
		entry.closing = true
		entry.waitCh = make(chan struct{})
	}
	sharedRuntimeRegistryMu.Unlock()

	if runtime == nil {
		return nil
	}
	closeErr := closeSharedLocalConnector(runtime)

	sharedRuntimeRegistryMu.Lock()
	entry = sharedRuntimeRegistry[scopeKey]
	if entry != nil && entry.closing {
		close(entry.waitCh)
		delete(sharedRuntimeRegistry, scopeKey)
	}
	sharedRuntimeRegistryMu.Unlock()

	return closeErr
}
