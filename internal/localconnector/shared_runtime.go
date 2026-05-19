package localconnector

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/nora/sandbox-vm/internal/envhost"
)

const sharedSessionStoreFilename = "session-environments.json"

type localConnectorHealthCheckHost interface {
	HealthCheck(ctx context.Context) error
}

// SharedRuntime exposes one shared VM-backed host with one environment per session.
type SharedRuntime struct {
	mu      sync.Mutex
	host    localConnectorHost
	closer  localConnectorCloser
	opts    localConnectorOptions
	store   *sessionEnvironmentStore
	session map[string]*Runtime
	closed  bool
}

func newSharedRuntimeWithHost(host localConnectorHost, closer localConnectorCloser, opts localConnectorOptions) (*SharedRuntime, error) {
	if host == nil {
		return nil, fmt.Errorf("connector host is required")
	}

	return &SharedRuntime{
		host:    host,
		closer:  closer,
		opts:    opts,
		store:   newSessionEnvironmentStore(filepath.Join(opts.metadataDir, sharedSessionStoreFilename), sharedRuntimeStoreScope(opts)),
		session: make(map[string]*Runtime),
	}, nil
}

func (r *SharedRuntime) Close() error {
	if r == nil {
		return nil
	}

	r.mu.Lock()
	if r.closed {
		r.mu.Unlock()
		return nil
	}
	r.closed = true
	sessionRuntimes := make([]*Runtime, 0, len(r.session))
	for _, runtime := range r.session {
		sessionRuntimes = append(sessionRuntimes, runtime)
	}
	r.session = nil
	closer := r.closer
	r.host = nil
	r.closer = nil
	r.mu.Unlock()

	var closeErr error
	for _, runtime := range sessionRuntimes {
		closeErr = errorsJoin(closeErr, runtime.ClosePreservingState())
	}
	if closer != nil {
		closeErr = errorsJoin(closeErr, closer.Close())
	}
	return closeErr
}

func (r *SharedRuntime) CloseSessionPreservingState(sessionID string) error {
	runtime, _, err := r.detachSessionRuntime(sessionID)
	if err != nil {
		return err
	}
	if runtime == nil {
		return nil
	}
	return runtime.ClosePreservingState()
}

func (r *SharedRuntime) DeleteSessionState(sessionID string) error {
	sessionKey := normalizeSessionKey(sessionID)
	runtime, host, err := r.detachSessionRuntime(sessionKey)
	if err != nil {
		return err
	}

	if runtime != nil {
		if err := runtime.Close(); err != nil {
			return err
		}
		return r.store.Delete(sessionKey)
	}

	envID, ok := r.store.Get(sessionKey)
	if !ok {
		return nil
	}

	if host == nil {
		return fmt.Errorf("shared connector runtime is closed")
	}

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := host.DeleteEnvironment(ctx, envID); err != nil {
		return err
	}
	return r.store.Delete(sessionKey)
}

func (r *SharedRuntime) HasSessionState(sessionID string) bool {
	if r == nil {
		return false
	}
	return r.store.Has(normalizeSessionKey(sessionID))
}

func (r *SharedRuntime) UpdateBackendConfig(backendURL string, backendAPIKey string) {
	if r == nil {
		return
	}

	backendURL = strings.TrimSpace(backendURL)
	backendAPIKey = strings.TrimSpace(backendAPIKey)

	r.mu.Lock()
	defer r.mu.Unlock()
	r.opts.backendURL = backendURL
	r.opts.backendAPIKey = backendAPIKey
	for _, runtime := range r.session {
		if runtime != nil {
			runtime.UpdateBackendConfig(backendURL, backendAPIKey)
		}
	}
}

func (r *SharedRuntime) HealthCheck() error {
	if r == nil {
		return fmt.Errorf("shared connector runtime is required")
	}

	r.mu.Lock()
	closed := r.closed
	host := r.host
	r.mu.Unlock()
	if closed || host == nil {
		return fmt.Errorf("shared connector runtime is closed")
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if healthHost, ok := host.(localConnectorHealthCheckHost); ok {
		return healthHost.HealthCheck(ctx)
	}
	for _, envID := range r.store.Records() {
		_, err := host.Metadata(ctx, envID)
		return err
	}
	return nil
}

func (r *SharedRuntime) ReadJSON(sessionID string, path string, offset int, limit int) (string, error) {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return "", err
	}
	return runtime.ReadJSON(path, offset, limit)
}

func (r *SharedRuntime) OpenReadStreamJSON(sessionID string, path string) (string, error) {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return "", err
	}
	return runtime.OpenReadStreamJSON(path)
}

func (r *SharedRuntime) ReadStreamChunkJSON(sessionID string, streamID string, maxBytes int) (string, error) {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return "", err
	}
	return runtime.ReadStreamChunkJSON(streamID, maxBytes)
}

func (r *SharedRuntime) CloseReadStream(sessionID string, streamID string) error {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return err
	}
	return runtime.CloseReadStream(streamID)
}

func (r *SharedRuntime) WriteJSON(sessionID string, path string, content string, encoding string, mode int) (string, error) {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return "", err
	}
	return runtime.WriteJSON(path, content, encoding, mode)
}

func (r *SharedRuntime) DeleteJSON(sessionID string, path string, recursive bool) (string, error) {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return "", err
	}
	return runtime.DeleteJSON(path, recursive)
}

func (r *SharedRuntime) StatJSON(sessionID string, path string) (string, error) {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return "", err
	}
	return runtime.StatJSON(path)
}

func (r *SharedRuntime) ListJSON(sessionID string, path string) (string, error) {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return "", err
	}
	return runtime.ListJSON(path)
}

func (r *SharedRuntime) GlobJSON(sessionID string, pattern string, path string) (string, error) {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return "", err
	}
	return runtime.GlobJSON(pattern, path)
}

func (r *SharedRuntime) GrepJSON(sessionID string, pattern string, path string, glob string) (string, error) {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return "", err
	}
	return runtime.GrepJSON(pattern, path, glob)
}

func (r *SharedRuntime) ExecJSON(sessionID string, command string, workingDir string, timeoutSeconds int) (string, error) {
	return r.ExecWithRuntimeJSON(sessionID, command, workingDir, timeoutSeconds, "", "")
}

func (r *SharedRuntime) ExecWithRuntimeJSON(sessionID string, command string, workingDir string, timeoutSeconds int, capabilitySessionID string, callerAgentID string) (string, error) {
	return r.ExecWithRuntimeEnvJSON(sessionID, command, workingDir, timeoutSeconds, nil, capabilitySessionID, callerAgentID)
}

func (r *SharedRuntime) ExecWithRuntimeEnvJSON(sessionID string, command string, workingDir string, timeoutSeconds int, envVars map[string]string, capabilitySessionID string, callerAgentID string) (string, error) {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return "", err
	}
	return runtime.ExecWithRuntimeEnvJSON(command, workingDir, timeoutSeconds, envVars, capabilitySessionID, callerAgentID)
}

func (r *SharedRuntime) Cleanup(sessionID string) error {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return err
	}
	return runtime.Cleanup()
}

func (r *SharedRuntime) GetSandboxStateJSON(sessionID string) (string, error) {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return "", err
	}
	return runtime.GetSandboxStateJSON()
}

func (r *SharedRuntime) AcceptChangesJSON(sessionID string, pathsJSON string) (string, error) {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return "", err
	}
	return runtime.AcceptChangesJSON(pathsJSON)
}

func (r *SharedRuntime) DiscardAllChangesJSON(sessionID string) (string, error) {
	runtime, err := r.runtimeForSession(sessionID)
	if err != nil {
		return "", err
	}
	return runtime.DiscardAllChangesJSON()
}

func (r *SharedRuntime) runtimeForSession(sessionID string) (*Runtime, error) {
	sessionKey := normalizeSessionKey(sessionID)

	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed || r.host == nil {
		return nil, fmt.Errorf("shared connector runtime is closed")
	}
	if runtime := r.session[sessionKey]; runtime != nil {
		return runtime, nil
	}

	envID, err := r.ensureEnvironmentLocked(sessionKey)
	if err != nil {
		return nil, err
	}

	runtime, err := newRuntimeWithEnvironment(r.host, nil, r.opts, envID)
	if err != nil {
		return nil, err
	}
	r.session[sessionKey] = runtime
	return runtime, nil
}

func (r *SharedRuntime) ensureEnvironmentLocked(sessionKey string) (string, error) {
	if envID, ok := r.store.Get(sessionKey); ok {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if _, err := r.host.Metadata(ctx, envID); err == nil {
			return envID, nil
		} else if !isEnvironmentNotFoundError(err) {
			return "", fmt.Errorf("inspect connector environment %s: %w", envID, err)
		}
		if err := r.store.Delete(sessionKey); err != nil {
			return "", err
		}
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	metadata, err := r.host.CreateEnvironment(ctx)
	if err != nil {
		return "", fmt.Errorf("create connector environment: %w", err)
	}
	if strings.TrimSpace(metadata.ID) == "" {
		return "", fmt.Errorf("connector environment id is empty")
	}
	if err := r.store.Put(sessionKey, metadata.ID); err != nil {
		_ = r.host.DeleteEnvironment(ctx, metadata.ID)
		return "", fmt.Errorf("persist connector session mapping: %w", err)
	}
	return metadata.ID, nil
}

func (r *SharedRuntime) detachSessionRuntime(sessionID string) (*Runtime, localConnectorHost, error) {
	if r == nil {
		return nil, nil, nil
	}

	sessionKey := normalizeSessionKey(sessionID)

	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return nil, nil, nil
	}
	runtime := r.session[sessionKey]
	delete(r.session, sessionKey)
	return runtime, r.host, nil
}

func normalizeSessionKey(sessionID string) string {
	trimmed := strings.TrimSpace(sessionID)
	if trimmed == "" {
		return "default"
	}
	return trimmed
}

type sessionEnvironmentStore struct {
	path    string
	scope   string
	mu      sync.RWMutex
	records map[string]string
}

type sessionEnvironmentStoreFile struct {
	Scope   string            `json:"scope,omitempty"`
	Records map[string]string `json:"records"`
}

func newSessionEnvironmentStore(path string, scope string) *sessionEnvironmentStore {
	store := &sessionEnvironmentStore{
		path:    path,
		scope:   strings.TrimSpace(scope),
		records: make(map[string]string),
	}
	_ = store.load()
	return store
}

func (s *sessionEnvironmentStore) Has(sessionKey string) bool {
	_, ok := s.Get(sessionKey)
	return ok
}

func (s *sessionEnvironmentStore) Get(sessionKey string) (string, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	envID, ok := s.records[sessionKey]
	return envID, ok && strings.TrimSpace(envID) != ""
}

func (s *sessionEnvironmentStore) Records() map[string]string {
	s.mu.RLock()
	defer s.mu.RUnlock()
	records := make(map[string]string, len(s.records))
	for sessionKey, envID := range s.records {
		records[sessionKey] = envID
	}
	return records
}

func (s *sessionEnvironmentStore) Put(sessionKey string, envID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.records[sessionKey] = strings.TrimSpace(envID)
	return s.saveLocked()
}

func (s *sessionEnvironmentStore) Delete(sessionKey string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.records, sessionKey)
	return s.saveLocked()
}

func (s *sessionEnvironmentStore) load() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.path == "" {
		return nil
	}

	data, err := os.ReadFile(s.path)
	if err != nil {
		if os.IsNotExist(err) {
			s.records = make(map[string]string)
			return nil
		}
		return err
	}

	var file sessionEnvironmentStoreFile
	if err := json.Unmarshal(data, &file); err != nil {
		return err
	}
	if file.Records == nil {
		file.Records = make(map[string]string)
	}
	if strings.TrimSpace(file.Scope) != s.scope {
		s.records = make(map[string]string)
		return nil
	}
	s.records = file.Records
	return nil
}

func (s *sessionEnvironmentStore) saveLocked() error {
	if s.path == "" {
		return nil
	}
	dir := filepath.Dir(s.path)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}
	data, err := json.MarshalIndent(sessionEnvironmentStoreFile{
		Scope:   s.scope,
		Records: s.records,
	}, "", "  ")
	if err != nil {
		return err
	}

	tempFile, err := os.CreateTemp(dir, filepath.Base(s.path)+".tmp-*")
	if err != nil {
		return err
	}
	tempPath := tempFile.Name()
	defer os.Remove(tempPath)

	if _, err := tempFile.Write(data); err != nil {
		_ = tempFile.Close()
		return err
	}
	if err := tempFile.Chmod(0o644); err != nil {
		_ = tempFile.Close()
		return err
	}
	if err := tempFile.Sync(); err != nil {
		_ = tempFile.Close()
		return err
	}
	if err := tempFile.Close(); err != nil {
		return err
	}
	return os.Rename(tempPath, s.path)
}

func sharedRuntimeStoreScope(opts localConnectorOptions) string {
	sum := sha256.Sum256([]byte(strings.Join([]string{
		filepath.Clean(strings.TrimSpace(opts.rootPath)),
		filepath.Clean(strings.TrimSpace(opts.metadataDir)),
		filepath.Clean(strings.TrimSpace(opts.rootfsOverlayDir)),
	}, "\n")))
	return hex.EncodeToString(sum[:16])
}

func errorsJoin(current error, next error) error {
	if current == nil {
		return next
	}
	if next == nil {
		return current
	}
	return fmt.Errorf("%w; %w", current, next)
}

func isEnvironmentNotFoundError(err error) bool {
	var protocolErr *envhost.ProtocolError
	return errors.As(err, &protocolErr) && protocolErr.Code == envhost.ErrCodeEnvironmentNotFound
}
