package localconnector

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"path/filepath"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	"github.com/nora/sandbox-vm/internal/envhost"
	"github.com/nora/sandbox-vm/internal/integrations/apiproxy"
	"github.com/nora/sandbox-vm/internal/runtimepath"
	"github.com/nora/sandbox-vm/pkg/filestream"
	"github.com/nora/sandbox-vm/pkg/types"
)

const (
	defaultLocalConnectorReadMaxBytes   = 10 * 1024 * 1024
	defaultLocalConnectorMaxMatches     = 10_000
	defaultLocalConnectorExecMaxBytes   = 65_536
	defaultLocalConnectorExecTimeoutSec = 120
	localConnectorStreamOpenTimeout     = 30 * time.Second
	localConnectorStreamChunkTimeout    = 30 * time.Second
)

// Config bootstraps one long-lived VM-backed local connector.
type Config struct {
	MetadataDir        string
	RootPath           string
	Mounts             []Mount
	KernelPath         string
	RootfsPath         string
	RootfsOverlayDir   string
	BackendURL         string
	BackendAPIKey      string
	ReadMaxBytes       int
	MaxMatches         int
	ExecOutputMaxBytes int
}

type Mount struct {
	HostPath    string
	VMPath      string
	ReadOnly    bool
	Passthrough bool
}

type localConnectorHost interface {
	CreateEnvironment(ctx context.Context) (envhost.EnvironmentMetadata, error)
	DeleteEnvironment(ctx context.Context, envID string) error
	Metadata(ctx context.Context, envID string) (envhost.EnvironmentMetadata, error)
	OpenFileReadStream(ctx context.Context, envID string, path string) (envhost.FileReadStream, error)
	OpenFileWriteStream(ctx context.Context, envID string, path string, opts envhost.FileWriteOptions) (envhost.FileWriteStream, error)
	DeleteFile(ctx context.Context, envID string, path string) error
	ExecuteCommand(ctx context.Context, envID string, args []string, workingDir string, envVars map[string]string, runtime *envhost.RuntimeConfig) (stdout, stderr string, exitCode int, err error)
	GetSandboxState(ctx context.Context, envID string) (*types.SandboxState, error)
	AcceptChanges(ctx context.Context, envID string, paths []string, hostBaseDir string) (*types.AcceptChangesResult, error)
	DiscardAllChanges(ctx context.Context, envID string) (*types.DiscardAllChangesResult, error)
	// Cleanup must be safe to call repeatedly for the same environment. Review
	// flows call it before every state/apply/discard operation to ensure the VM
	// is unmounted and housekeeping has completed before diff inspection.
	Cleanup(ctx context.Context, envID string) error
}

type localConnectorCloser interface {
	Close() error
}

type localConnectorRestorableHost interface {
	RestoreEnvironment(ctx context.Context) (envhost.EnvironmentMetadata, bool, error)
}

type localConnectorOptions struct {
	metadataDir        string
	rootPath           string
	mounts             []Mount
	kernelPath         string
	rootfsPath         string
	rootfsOverlayDir   string
	backendURL         string
	backendAPIKey      string
	capabilityProvider *apiproxy.CapabilityProvider
	readMaxBytes       int
	maxMatches         int
	execOutputMaxBytes int
}

// Runtime exposes a small API for file ops, exec, and workspace review inside one VM-backed sandbox host.
type Runtime struct {
	mu                 sync.RWMutex
	host               localConnectorHost
	closer             localConnectorCloser
	environmentID      string
	rootPath           string
	mounts             []Mount
	capabilityProvider *apiproxy.CapabilityProvider
	backendURL         string
	backendAPIKey      string
	readMaxBytes       int
	maxMatches         int
	execOutputMaxBytes int
	closed             bool

	readStreamMu     sync.Mutex
	nextReadStreamID uint64
	readStreams      map[string]readStreamState
}

type readStreamState struct {
	stream envhost.FileReadStream
	cancel context.CancelFunc
}

// Cleanup unmounts the environment and runs housekeeping so workspace state can
// be inspected or reviewed safely after a task completes. It is intentionally
// idempotent because review flows may call it multiple times in succession.
func (r *Runtime) Cleanup() error {
	host, environmentID, err := r.runtimeHandle()
	if err != nil {
		return err
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	return host.Cleanup(ctx, environmentID)
}

func newRuntimeWithHost(host localConnectorHost, closer localConnectorCloser, opts localConnectorOptions) (*Runtime, error) {
	if host == nil {
		return nil, fmt.Errorf("connector host is required")
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	metadata, err := restoreOrCreateEnvironment(ctx, host)
	if err != nil {
		return nil, err
	}
	if strings.TrimSpace(metadata.ID) == "" {
		return nil, fmt.Errorf("connector environment id is empty")
	}

	return newRuntimeWithEnvironment(host, closer, opts, metadata.ID)
}

func newRuntimeWithEnvironment(host localConnectorHost, closer localConnectorCloser, opts localConnectorOptions, environmentID string) (*Runtime, error) {
	if host == nil {
		return nil, fmt.Errorf("connector host is required")
	}
	if strings.TrimSpace(environmentID) == "" {
		return nil, fmt.Errorf("connector environment id is empty")
	}

	return &Runtime{
		host:               host,
		closer:             closer,
		environmentID:      strings.TrimSpace(environmentID),
		rootPath:           opts.rootPath,
		mounts:             opts.mounts,
		capabilityProvider: opts.capabilityProvider,
		backendURL:         opts.backendURL,
		backendAPIKey:      opts.backendAPIKey,
		readMaxBytes:       opts.readMaxBytes,
		maxMatches:         opts.maxMatches,
		execOutputMaxBytes: opts.execOutputMaxBytes,
	}, nil
}

func (r *Runtime) UpdateBackendConfig(backendURL string, backendAPIKey string) {
	if r == nil {
		return
	}

	r.mu.Lock()
	defer r.mu.Unlock()
	r.backendURL = strings.TrimSpace(backendURL)
	r.backendAPIKey = strings.TrimSpace(backendAPIKey)
}

func restoreOrCreateEnvironment(ctx context.Context, host localConnectorHost) (envhost.EnvironmentMetadata, error) {
	if restorableHost, ok := host.(localConnectorRestorableHost); ok {
		metadata, restored, err := restorableHost.RestoreEnvironment(ctx)
		if err != nil {
			return envhost.EnvironmentMetadata{}, fmt.Errorf("restore connector environment: %w", err)
		}
		if restored {
			return metadata, nil
		}
	}

	metadata, err := host.CreateEnvironment(ctx)
	if err != nil {
		return envhost.EnvironmentMetadata{}, fmt.Errorf("create connector environment: %w", err)
	}
	return metadata, nil
}

func normalizeConfig(cfg Config) (localConnectorOptions, error) {
	cfgCopy := cfg

	rootPath := strings.TrimSpace(cfgCopy.RootPath)
	if rootPath == "" {
		return localConnectorOptions{}, fmt.Errorf("root path is required")
	}
	absRoot, err := filepath.Abs(rootPath)
	if err != nil {
		return localConnectorOptions{}, fmt.Errorf("resolve root path: %w", err)
	}
	absRoot = filepath.Clean(absRoot)

	metadataDir := strings.TrimSpace(cfgCopy.MetadataDir)
	if metadataDir == "" {
		metadataDir = filepath.Join(runtimepath.DefaultRootDir(), "local-connector")
	}
	metadataDir, err = filepath.Abs(metadataDir)
	if err != nil {
		return localConnectorOptions{}, fmt.Errorf("resolve metadata dir: %w", err)
	}
	metadataDir = filepath.Clean(metadataDir)

	kernelPath := strings.TrimSpace(cfgCopy.KernelPath)
	if kernelPath == "" {
		return localConnectorOptions{}, fmt.Errorf("kernel path is required")
	}
	rootfsPath := strings.TrimSpace(cfgCopy.RootfsPath)
	if rootfsPath == "" {
		return localConnectorOptions{}, fmt.Errorf("rootfs path is required")
	}

	readMaxBytes := cfgCopy.ReadMaxBytes
	if readMaxBytes <= 0 {
		readMaxBytes = defaultLocalConnectorReadMaxBytes
	}
	maxMatches := cfgCopy.MaxMatches
	if maxMatches <= 0 {
		maxMatches = defaultLocalConnectorMaxMatches
	}
	execOutputMaxBytes := cfgCopy.ExecOutputMaxBytes
	if execOutputMaxBytes <= 0 {
		execOutputMaxBytes = defaultLocalConnectorExecMaxBytes
	}

	mounts := normalizeMounts(cfgCopy.Mounts, absRoot)

	return localConnectorOptions{
		metadataDir:        metadataDir,
		rootPath:           absRoot,
		mounts:             mounts,
		kernelPath:         kernelPath,
		rootfsPath:         rootfsPath,
		rootfsOverlayDir:   strings.TrimSpace(cfgCopy.RootfsOverlayDir),
		backendURL:         strings.TrimSpace(cfgCopy.BackendURL),
		backendAPIKey:      strings.TrimSpace(cfgCopy.BackendAPIKey),
		readMaxBytes:       readMaxBytes,
		maxMatches:         maxMatches,
		execOutputMaxBytes: execOutputMaxBytes,
	}, nil
}

func normalizeMounts(mounts []Mount, rootPath string) []Mount {
	if len(mounts) == 0 {
		return []Mount{{HostPath: rootPath, VMPath: rootPath}}
	}

	result := make([]Mount, 0, len(mounts))
	seen := make(map[string]struct{}, len(mounts))
	for _, mount := range mounts {
		hostPath := filepath.Clean(strings.TrimSpace(mount.HostPath))
		if hostPath == "." || hostPath == "" {
			continue
		}
		if !filepath.IsAbs(hostPath) {
			continue
		}
		vmPath := filepath.Clean(strings.TrimSpace(mount.VMPath))
		if vmPath == "." || vmPath == "" {
			vmPath = hostPath
		}
		if !filepath.IsAbs(vmPath) {
			continue
		}
		key := hostPath + "\x00" + vmPath
		if _, ok := seen[key]; ok {
			continue
		}
		seen[key] = struct{}{}
		result = append(result, Mount{
			HostPath:    hostPath,
			VMPath:      vmPath,
			ReadOnly:    mount.ReadOnly,
			Passthrough: mount.Passthrough,
		})
	}
	if len(result) == 0 {
		return []Mount{{HostPath: rootPath, VMPath: rootPath}}
	}
	return result
}

// Close tears down the connector environment and closes the underlying sandbox host.
func (r *Runtime) Close() error {
	return r.close(true)
}

// ClosePreservingState stops the runtime without deleting the session's
// persisted sandbox mapping so a later process can restore the same overlay.
func (r *Runtime) ClosePreservingState() error {
	return r.close(false)
}

func (r *Runtime) close(deleteEnvironment bool) error {
	host, environmentID, closer, err := r.acquireForClose()
	if err != nil {
		return err
	}

	var closeErr error
	for _, state := range r.takeAllReadStreams() {
		if state.cancel != nil {
			state.cancel()
		}
		closeErr = errors.Join(closeErr, state.stream.Close())
	}
	if deleteEnvironment && host != nil && environmentID != "" {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		if err := host.DeleteEnvironment(ctx, environmentID); err != nil {
			closeErr = errors.Join(closeErr, err)
		}
		cancel()
	}
	if closer != nil {
		closeErr = errors.Join(closeErr, closer.Close())
	}
	return closeErr
}

func (r *Runtime) acquireForClose() (localConnectorHost, string, localConnectorCloser, error) {
	if r == nil {
		return nil, "", nil, fmt.Errorf("connector runtime is required")
	}

	r.mu.Lock()
	defer r.mu.Unlock()
	if r.closed {
		return nil, "", nil, nil
	}
	if r.host == nil {
		r.closed = true
		return nil, "", nil, nil
	}

	host := r.host
	environmentID := r.environmentID
	closer := r.closer
	r.closed = true
	r.host = nil
	r.closer = nil
	r.environmentID = ""
	return host, environmentID, closer, nil
}

// OpenReadStreamJSON opens one file relative to the configured root for chunked streaming reads.
func (r *Runtime) OpenReadStreamJSON(path string) (string, error) {
	host, environmentID, err := r.runtimeHandle()
	if err != nil {
		return "", err
	}
	resolvedPath, err := r.resolveRequiredPath(path)
	if err != nil {
		return "", err
	}

	ctx, cancel := context.WithCancel(context.Background())
	openTimer := time.AfterFunc(localConnectorStreamOpenTimeout, cancel)

	stream, err := host.OpenFileReadStream(ctx, environmentID, resolvedPath)
	if err != nil {
		openTimer.Stop()
		cancel()
		return "", err
	}
	if !openTimer.Stop() {
		_ = stream.Close()
		cancel()
		return "", fmt.Errorf("open read stream timed out")
	}

	info := stream.Info()
	streamID := r.storeReadStream(stream, cancel)
	return marshalJSON(map[string]any{
		"stream_id": streamID,
		"file_name": info.FileName,
		"mime_type": info.MimeType,
		"size":      info.TotalSize,
		"mode":      info.Mode,
	})
}

// ReadStreamChunkJSON reads the next chunk from one previously opened read stream.
func (r *Runtime) ReadStreamChunkJSON(streamID string, maxBytes int) (string, error) {
	stream, ok := r.lookupReadStream(streamID)
	if !ok {
		return "", fmt.Errorf("read stream not found: %s", streamID)
	}
	if maxBytes <= 0 {
		maxBytes = 64 * 1024
	}

	buf := make([]byte, maxBytes)
	type readResult struct {
		n   int
		err error
	}
	resultCh := make(chan readResult, 1)
	go func() {
		n, err := stream.Read(buf)
		resultCh <- readResult{n: n, err: err}
	}()

	var n int
	var err error
	select {
	case result := <-resultCh:
		n = result.n
		err = result.err
	case <-time.After(localConnectorStreamChunkTimeout):
		_ = r.removeReadStream(streamID)
		return "", fmt.Errorf("read stream chunk timed out")
	}
	if err != nil && err != io.EOF {
		_ = r.removeReadStream(streamID)
		return "", fmt.Errorf("read stream chunk: %w", err)
	}

	eof := err == io.EOF
	if eof {
		_ = r.removeReadStream(streamID)
	}

	return marshalJSON(map[string]any{
		"content": base64.StdEncoding.EncodeToString(buf[:n]),
		"bytes":   n,
		"eof":     eof,
	})
}

// CloseReadStream closes one previously opened read stream. Missing streams are ignored.
func (r *Runtime) CloseReadStream(streamID string) error {
	return r.removeReadStream(streamID)
}

// ReadJSON reads one file relative to the configured root and returns a JSON payload with utf8/base64 content.
func (r *Runtime) ReadJSON(path string, offset int, limit int) (string, error) {
	host, environmentID, err := r.runtimeHandle()
	if err != nil {
		return "", err
	}
	resolvedPath, err := r.resolveRequiredPath(path)
	if err != nil {
		return "", err
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	stream, err := host.OpenFileReadStream(ctx, environmentID, resolvedPath)
	if err != nil {
		return "", err
	}
	defer stream.Close()

	info := stream.Info()
	limitReader := io.LimitReader(stream, int64(r.readMaxBytes)+1)
	data, err := io.ReadAll(limitReader)
	if err != nil {
		return "", fmt.Errorf("read file: %w", err)
	}

	totalSize := info.TotalSize
	if totalSize <= 0 {
		totalSize = int64(len(data))
	}
	truncated := len(data) > r.readMaxBytes || totalSize > int64(r.readMaxBytes)
	if len(data) > r.readMaxBytes {
		data = data[:r.readMaxBytes]
	}

	if detectBinaryContent(data) {
		return marshalJSON(map[string]any{
			"content":   base64.StdEncoding.EncodeToString(data),
			"encoding":  "base64",
			"size":      totalSize,
			"truncated": truncated,
		})
	}

	text := strings.ToValidUTF8(string(data), "\uFFFD")
	if offset > 0 || limit > 0 {
		lines := strings.Split(text, "\n")
		start := 0
		if offset > 0 {
			start = offset - 1
		}
		if start < 0 {
			start = 0
		}
		if start >= len(lines) {
			lines = nil
		} else {
			lines = lines[start:]
		}
		if limit > 0 && limit < len(lines) {
			lines = lines[:limit]
		}
		text = strings.Join(lines, "\n")
	}

	return marshalJSON(map[string]any{
		"content":   text,
		"encoding":  "utf8",
		"size":      totalSize,
		"truncated": truncated,
	})
}

// GetSandboxStateJSON returns the current workspace-review state for the
// connector environment after a Cleanup pass. Cleanup is expected to be
// idempotent because review operations may repeat it between user actions.
func (r *Runtime) GetSandboxStateJSON() (string, error) {
	host, environmentID, err := r.runtimeHandle()
	if err != nil {
		return "", err
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	if err := host.Cleanup(ctx, environmentID); err != nil {
		return "", err
	}

	state, err := host.GetSandboxState(ctx, environmentID)
	if err != nil {
		return "", err
	}
	return marshalJSON(state)
}

// AcceptChangesJSON applies the selected workspace changes to the host root and
// returns the review result as JSON. It runs Cleanup first so accepts always
// operate on a settled overlay state.
func (r *Runtime) AcceptChangesJSON(pathsJSON string) (string, error) {
	host, environmentID, err := r.runtimeHandle()
	if err != nil {
		return "", err
	}

	var paths []string
	trimmed := strings.TrimSpace(pathsJSON)
	if trimmed != "" && trimmed != "[]" {
		if err := json.Unmarshal([]byte(trimmed), &paths); err != nil {
			return "", fmt.Errorf("invalid paths json: %w", err)
		}
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	if err := host.Cleanup(ctx, environmentID); err != nil {
		return "", err
	}

	// Sandbox diffs already carry VM mount paths rooted at the host absolute
	// path, so accepts must apply them relative to "/" instead of prefixing the
	// workspace root again.
	result, err := host.AcceptChanges(ctx, environmentID, paths, string(filepath.Separator))
	if err != nil {
		return "", err
	}
	return marshalJSON(result)
}

// DiscardAllChangesJSON discards all pending workspace changes and returns the
// discard result as JSON. It runs Cleanup first so discard observes the same
// settled overlay state as review/accept.
func (r *Runtime) DiscardAllChangesJSON() (string, error) {
	host, environmentID, err := r.runtimeHandle()
	if err != nil {
		return "", err
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	if err := host.Cleanup(ctx, environmentID); err != nil {
		return "", err
	}

	result, err := host.DiscardAllChanges(ctx, environmentID)
	if err != nil {
		return "", err
	}
	return marshalJSON(result)
}

// WriteJSON writes utf8 or base64 content to one file relative to the configured root and returns a JSON result.
func (r *Runtime) WriteJSON(path string, content string, encoding string, mode int) (string, error) {
	host, environmentID, err := r.runtimeHandle()
	if err != nil {
		return "", err
	}
	resolvedPath, err := r.resolveRequiredPath(path)
	if err != nil {
		return "", err
	}

	var data []byte
	switch strings.ToLower(strings.TrimSpace(encoding)) {
	case "", "utf8", "utf-8":
		data = []byte(content)
	case "base64":
		decoded, err := base64.StdEncoding.DecodeString(content)
		if err != nil {
			return "", fmt.Errorf("decode base64 content: %w", err)
		}
		data = decoded
	default:
		return "", fmt.Errorf("unsupported encoding: %s", encoding)
	}

	if mode <= 0 {
		mode = 0o644
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	stream, err := host.OpenFileWriteStream(ctx, environmentID, resolvedPath, envhost.FileWriteOptions{
		Overwrite: true,
		Mode:      uint32(mode),
	})
	if err != nil {
		return "", err
	}

	if _, err := io.Copy(stream, bytes.NewReader(data)); err != nil {
		_ = stream.Abort()
		return "", fmt.Errorf("write file: %w", err)
	}

	digest := sha256.Sum256(data)
	result, err := stream.Commit(filestream.FileStreamDone{
		BytesSent: int64(len(data)),
		SHA256:    hex.EncodeToString(digest[:]),
	})
	if err != nil {
		return "", fmt.Errorf("commit file write: %w", err)
	}

	return marshalJSON(map[string]any{
		"size":        len(data),
		"sha256":      result.SHA256,
		"created":     result.Created,
		"overwritten": result.Overwritten,
	})
}

// DeleteJSON removes one file or directory relative to the configured root.
func (r *Runtime) DeleteJSON(path string, recursive bool) (string, error) {
	host, environmentID, err := r.runtimeHandle()
	if err != nil {
		return "", err
	}
	resolvedPath, err := r.resolveRequiredPath(path)
	if err != nil {
		return "", err
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	if recursive {
		stdout, stderr, exitCode, err := host.ExecuteCommand(ctx, environmentID, []string{"rm", "-rf", "--", resolvedPath}, r.rootPath, nil, nil)
		if err != nil {
			return "", err
		}
		if exitCode != 0 {
			return "", fmt.Errorf("delete failed (exit %d): %s%s", exitCode, stdout, stderr)
		}
	} else {
		if err := host.DeleteFile(ctx, environmentID, resolvedPath); err != nil {
			return "", err
		}
	}

	return marshalJSON(map[string]any{"deleted": true})
}

// StatJSON returns a JSON description of one file or directory relative to the configured root.
func (r *Runtime) StatJSON(path string) (string, error) {
	resolvedPath, err := r.resolveRequiredPath(path)
	if err != nil {
		return "", err
	}

	stdout, err := r.runPythonJSON(30*time.Second, statPythonScript, []string{resolvedPath, path})
	if err != nil {
		return "", err
	}
	return stdout, nil
}

// ListJSON lists entries under one directory relative to the configured root.
func (r *Runtime) ListJSON(path string) (string, error) {
	resolvedPath, err := r.resolveRequiredPath(path)
	if err != nil {
		return "", err
	}

	stdout, err := r.runPythonJSON(30*time.Second, listPythonScript, []string{resolvedPath})
	if err != nil {
		return "", err
	}
	return stdout, nil
}

// GlobJSON returns a JSON payload of file matches rooted at the configured root or one child directory.
func (r *Runtime) GlobJSON(pattern string, path string) (string, error) {
	pattern = strings.TrimSpace(pattern)
	if pattern == "" {
		return "", fmt.Errorf("glob pattern is required")
	}

	basePath, err := r.resolveBasePath(path)
	if err != nil {
		return "", err
	}

	stdout, err := r.runPythonJSON(60*time.Second, globPythonScript, []string{basePath, pattern, fmt.Sprintf("%d", r.maxMatches)})
	if err != nil {
		return "", err
	}
	return stdout, nil
}

// GrepJSON returns JSON match objects rooted at the configured root or one child directory.
func (r *Runtime) GrepJSON(pattern string, path string, glob string) (string, error) {
	pattern = strings.TrimSpace(pattern)
	if pattern == "" {
		return "", fmt.Errorf("grep pattern is required")
	}

	basePath, err := r.resolveBasePath(path)
	if err != nil {
		return "", err
	}

	stdout, err := r.runPythonJSON(60*time.Second, grepPythonScript, []string{basePath, pattern, glob, fmt.Sprintf("%d", r.maxMatches)})
	if err != nil {
		return "", err
	}
	return stdout, nil
}

// ExecJSON executes one shell command inside the long-lived sandbox and returns a JSON result.
// The command string is executed through `sh -lc`; callers are responsible for any needed sanitization.
func (r *Runtime) ExecJSON(command string, workingDir string, timeoutSeconds int) (string, error) {
	return r.ExecWithRuntimeJSON(command, workingDir, timeoutSeconds, "", "")
}

// ExecWithRuntimeJSON executes one shell command with an optional runtime capability scope.
func (r *Runtime) ExecWithRuntimeJSON(command string, workingDir string, timeoutSeconds int, sessionID string, callerAgentID string) (string, error) {
	return r.ExecWithRuntimeEnvJSON(command, workingDir, timeoutSeconds, nil, sessionID, callerAgentID)
}

// ExecWithRuntimeEnvJSON executes one shell command with explicit environment overrides and an optional runtime capability scope.
func (r *Runtime) ExecWithRuntimeEnvJSON(command string, workingDir string, timeoutSeconds int, envVars map[string]string, sessionID string, callerAgentID string) (string, error) {
	host, environmentID, err := r.runtimeHandle()
	if err != nil {
		return "", err
	}
	command = strings.TrimSpace(command)
	if command == "" {
		return "", fmt.Errorf("command is required")
	}

	resolvedWorkingDir, err := r.resolveBasePath(workingDir)
	if err != nil {
		return "", err
	}
	if timeoutSeconds <= 0 {
		timeoutSeconds = defaultLocalConnectorExecTimeoutSec
	}

	ctx, cancel := context.WithTimeout(context.Background(), time.Duration(timeoutSeconds)*time.Second)
	defer cancel()

	stdout, stderr, exitCode, err := host.ExecuteCommand(
		ctx,
		environmentID,
		[]string{"sh", "-lc", command},
		resolvedWorkingDir,
		envVars,
		r.executionRuntime(sessionID, callerAgentID),
	)
	if err != nil {
		return "", err
	}

	stdout, stdoutTruncated := truncateUTF8ByBytes(stdout, r.execOutputMaxBytes)
	stderr, stderrTruncated := truncateUTF8ByBytes(stderr, r.execOutputMaxBytes)
	result := map[string]any{
		"exit_code": exitCode,
		"stdout":    stdout,
		"stderr":    stderr,
	}
	if stdoutTruncated || stderrTruncated {
		result["truncated"] = true
	}
	return marshalJSON(result)
}

func (r *Runtime) executionRuntime(sessionID string, callerAgentID string) *envhost.RuntimeConfig {
	if r == nil || r.capabilityProvider == nil {
		return nil
	}
	runtime := r.capabilityProvider.ExecutionRuntime(sessionID, callerAgentID)
	if runtime == nil {
		return nil
	}
	r.mu.RLock()
	backendURL := strings.TrimSpace(r.backendURL)
	backendAPIKey := strings.TrimSpace(r.backendAPIKey)
	r.mu.RUnlock()
	runtime.SessionID = strings.TrimSpace(sessionID)
	runtime.CallerAgentID = strings.TrimSpace(callerAgentID)
	runtime.BackendURL = backendURL
	runtime.BackendAPIKey = backendAPIKey
	return runtime
}

func (r *Runtime) runtimeHandle() (localConnectorHost, string, error) {
	if r == nil {
		return nil, "", fmt.Errorf("connector runtime is required")
	}

	r.mu.RLock()
	defer r.mu.RUnlock()
	if r.closed || r.host == nil || strings.TrimSpace(r.environmentID) == "" {
		return nil, "", fmt.Errorf("connector runtime is closed")
	}
	return r.host, r.environmentID, nil
}

func (r *Runtime) resolveRequiredPath(path string) (string, error) {
	path = strings.TrimSpace(path)
	if path == "" {
		return "", fmt.Errorf("path is required")
	}
	return r.resolvePathWithinRoot(path)
}

func (r *Runtime) resolveBasePath(path string) (string, error) {
	path = strings.TrimSpace(path)
	if path == "" {
		return r.rootPath, nil
	}
	return r.resolvePathWithinRoot(path)
}

func (r *Runtime) resolvePathWithinRoot(path string) (string, error) {
	if r == nil {
		return "", fmt.Errorf("connector runtime is required")
	}

	rootPath := r.rootPath
	candidate := path
	if !filepath.IsAbs(candidate) {
		candidate = filepath.Join(rootPath, candidate)
	}
	candidate = filepath.Clean(candidate)
	if pathWithinRoot(candidate, rootPath) {
		return candidate, nil
	}
	for _, mount := range r.mounts {
		mountPath := filepath.Clean(strings.TrimSpace(mount.VMPath))
		if mountPath == "." || mountPath == "" {
			mountPath = filepath.Clean(strings.TrimSpace(mount.HostPath))
		}
		if pathWithinRoot(candidate, mountPath) {
			return candidate, nil
		}
	}
	return "", fmt.Errorf("path escapes root or mounted roots: %s", path)
}

func pathWithinRoot(candidate string, rootPath string) bool {
	rootPath = filepath.Clean(rootPath)
	if candidate == rootPath {
		return true
	}
	rel, err := filepath.Rel(rootPath, candidate)
	if err != nil {
		return false
	}
	rel = filepath.Clean(rel)
	return rel != ".." && !strings.HasPrefix(rel, ".."+string(filepath.Separator))
}

func (r *Runtime) storeReadStream(stream envhost.FileReadStream, cancel context.CancelFunc) string {
	r.readStreamMu.Lock()
	defer r.readStreamMu.Unlock()
	r.nextReadStreamID++
	streamID := fmt.Sprintf("stream-%d", r.nextReadStreamID)
	if r.readStreams == nil {
		r.readStreams = make(map[string]readStreamState)
	}
	r.readStreams[streamID] = readStreamState{stream: stream, cancel: cancel}
	return streamID
}

func (r *Runtime) lookupReadStream(streamID string) (envhost.FileReadStream, bool) {
	r.readStreamMu.Lock()
	defer r.readStreamMu.Unlock()
	state, ok := r.readStreams[streamID]
	return state.stream, ok
}

func (r *Runtime) removeReadStream(streamID string) error {
	r.readStreamMu.Lock()
	state, ok := r.readStreams[streamID]
	if ok {
		delete(r.readStreams, streamID)
	}
	r.readStreamMu.Unlock()
	if !ok {
		return nil
	}
	if state.cancel != nil {
		state.cancel()
	}
	return state.stream.Close()
}

func (r *Runtime) takeAllReadStreams() []readStreamState {
	r.readStreamMu.Lock()
	defer r.readStreamMu.Unlock()
	if len(r.readStreams) == 0 {
		return nil
	}
	streams := make([]readStreamState, 0, len(r.readStreams))
	for streamID, state := range r.readStreams {
		streams = append(streams, state)
		delete(r.readStreams, streamID)
	}
	return streams
}

func (r *Runtime) runPythonJSON(timeout time.Duration, code string, args []string) (string, error) {
	host, environmentID, err := r.runtimeHandle()
	if err != nil {
		return "", err
	}
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()

	cmdArgs := append([]string{"python3", "-c", code}, args...)
	stdout, stderr, exitCode, err := host.ExecuteCommand(ctx, environmentID, cmdArgs, r.rootPath, nil, nil)
	if err != nil {
		return "", err
	}
	if exitCode != 0 {
		return "", fmt.Errorf("connector helper failed (exit %d): %s%s", exitCode, stdout, stderr)
	}
	return strings.TrimSpace(stdout), nil
}

func detectBinaryContent(data []byte) bool {
	check := data
	if len(check) > 512 {
		check = check[:512]
	}
	return bytes.IndexByte(check, 0) >= 0
}

func truncateUTF8ByBytes(value string, maxBytes int) (string, bool) {
	if maxBytes <= 0 || len(value) <= maxBytes {
		return value, false
	}
	truncated := value[:maxBytes]
	for !utf8.ValidString(truncated) && len(truncated) > 0 {
		truncated = truncated[:len(truncated)-1]
	}
	return truncated, true
}

func marshalJSON(value any) (string, error) {
	data, err := json.Marshal(value)
	if err != nil {
		return "", fmt.Errorf("marshal json: %w", err)
	}
	return string(data), nil
}

const statPythonScript = `
import json, os, stat, sys
path = sys.argv[1]
display_path = sys.argv[2]
st = os.lstat(path)
if stat.S_ISDIR(st.st_mode):
    kind = 'dir'
elif stat.S_ISLNK(st.st_mode):
    kind = 'symlink'
else:
    kind = 'file'
print(json.dumps({
    'path': display_path,
    'kind': kind,
    'size': int(st.st_size),
    'mode': int(st.st_mode & 0o777),
    'modified_at': int(st.st_mtime),
}))
`

const listPythonScript = `
import json, os, stat, sys
path = sys.argv[1]
entries = []
with os.scandir(path) as it:
    for entry in it:
        st = entry.stat(follow_symlinks=False)
        if stat.S_ISDIR(st.st_mode):
            kind = 'dir'
        elif stat.S_ISLNK(st.st_mode):
            kind = 'symlink'
        else:
            kind = 'file'
        entries.append({
            'name': entry.name,
            'kind': kind,
            'size': int(st.st_size),
            'modified_at': int(st.st_mtime),
        })
entries.sort(key=lambda item: item['name'])
print(json.dumps({'entries': entries}))
`

const globPythonScript = `
import glob, json, os, sys
base_path, pattern, max_matches = sys.argv[1], sys.argv[2], int(sys.argv[3])
matches = []
truncated = False
for match in glob.iglob(pattern, root_dir=base_path, recursive=True):
    if any(part.startswith('.') for part in match.split(os.sep) if part):
        continue
    full_path = os.path.join(base_path, match)
    if not os.path.isfile(full_path):
        continue
    matches.append(match.replace(os.sep, '/'))
    if len(matches) >= max_matches:
        truncated = True
        break
print(json.dumps({'matches': matches, 'truncated': truncated}))
`

const grepPythonScript = `
import glob, json, os, re, sys
base_path, pattern, glob_pattern, max_matches = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4])
regex = re.compile(pattern)
matches = []
truncated = False
allowed_paths = None
if glob_pattern:
    allowed_paths = set()
    for match in glob.iglob(glob_pattern, root_dir=base_path, recursive=True):
        if any(part.startswith('.') for part in match.split(os.sep) if part):
            continue
        full_path = os.path.join(base_path, match)
        if os.path.isfile(full_path):
            allowed_paths.add(match.replace(os.sep, '/'))
for dirpath, dirnames, filenames in os.walk(base_path):
    dirnames[:] = sorted(name for name in dirnames if not name.startswith('.'))
    for filename in sorted(filenames):
        if filename.startswith('.'):
            continue
        full_path = os.path.join(dirpath, filename)
        rel_path = os.path.relpath(full_path, base_path).replace(os.sep, '/')
        if allowed_paths is not None and rel_path not in allowed_paths:
            continue
        try:
            with open(full_path, 'rb') as fh:
                if b'\x00' in fh.read(512):
                    continue
            with open(full_path, 'r', encoding='utf-8', errors='replace') as fh:
                for line_number, line in enumerate(fh, start=1):
                    line = line.rstrip('\n')
                    if not regex.search(line):
                        continue
                    matches.append({
                        'file': rel_path,
                        'line': line_number,
                        'content': line,
                    })
                    if len(matches) >= max_matches:
                        truncated = True
                        raise StopIteration
        except StopIteration:
            break
        except Exception:
            continue
    if truncated:
        break
print(json.dumps({'matches': matches, 'truncated': truncated}))
`
