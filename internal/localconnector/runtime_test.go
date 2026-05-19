package localconnector

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/nora/sandbox-vm/internal/envhost"
	"github.com/nora/sandbox-vm/internal/integrations/apiproxy"
	"github.com/nora/sandbox-vm/pkg/types"
)

type fakeLocalConnectorHost struct {
	root            string
	createdEnvID    string
	createdEnvIDs   []string
	deletedEnvID    string
	restoredEnvID   string
	restoreCalled   bool
	restoreOK       bool
	closed          bool
	lastRuntime     *envhost.RuntimeConfig
	cleanupCalls    int
	state           *types.SandboxState
	acceptResult    *types.AcceptChangesResult
	discardResult   *types.DiscardAllChangesResult
	acceptedPaths   []string
	acceptedBaseDir string
	envIndex        int
	envs            map[string]bool
	metadataErrs    map[string]error
}

func newFakeLocalConnectorHost(root string) *fakeLocalConnectorHost {
	return &fakeLocalConnectorHost{
		root: root,
		envs: make(map[string]bool),
	}
}

func (h *fakeLocalConnectorHost) CreateEnvironment(context.Context) (envhost.EnvironmentMetadata, error) {
	h.envIndex++
	h.createdEnvID = fmt.Sprintf("env-%d", h.envIndex)
	h.createdEnvIDs = append(h.createdEnvIDs, h.createdEnvID)
	h.envs[h.createdEnvID] = true
	return envhost.EnvironmentMetadata{ID: h.createdEnvID}, nil
}

func (h *fakeLocalConnectorHost) DeleteEnvironment(_ context.Context, envID string) error {
	h.deletedEnvID = envID
	delete(h.envs, envID)
	return nil
}

func (h *fakeLocalConnectorHost) RestoreEnvironment(context.Context) (envhost.EnvironmentMetadata, bool, error) {
	h.restoreCalled = true
	if !h.restoreOK {
		return envhost.EnvironmentMetadata{}, false, nil
	}
	if h.restoredEnvID == "" {
		h.restoredEnvID = "env-restored"
	}
	h.envs[h.restoredEnvID] = true
	return envhost.EnvironmentMetadata{ID: h.restoredEnvID}, true, nil
}

func (h *fakeLocalConnectorHost) Metadata(_ context.Context, envID string) (envhost.EnvironmentMetadata, error) {
	if err := h.metadataErrs[envID]; err != nil {
		return envhost.EnvironmentMetadata{}, err
	}
	if !h.envs[envID] {
		return envhost.EnvironmentMetadata{}, envhost.NewProtocolError(envhost.ErrCodeEnvironmentNotFound, fmt.Sprintf("environment %s not found", envID))
	}
	return envhost.EnvironmentMetadata{ID: envID}, nil
}

func (h *fakeLocalConnectorHost) OpenFileReadStream(_ context.Context, _ string, path string) (envhost.FileReadStream, error) {
	return envhost.OpenLocalFileReadStream(path)
}

func (h *fakeLocalConnectorHost) OpenFileWriteStream(_ context.Context, _ string, path string, opts envhost.FileWriteOptions) (envhost.FileWriteStream, error) {
	return envhost.OpenLocalFileWriteStream(path, opts)
}

func (h *fakeLocalConnectorHost) DeleteFile(_ context.Context, _ string, path string) error {
	return os.Remove(path)
}

func (h *fakeLocalConnectorHost) ExecuteCommand(ctx context.Context, _ string, args []string, workingDir string, envVars map[string]string, runtime *envhost.RuntimeConfig) (string, string, int, error) {
	h.lastRuntime = runtime
	cmd := exec.CommandContext(ctx, args[0], args[1:]...)
	cmd.Dir = workingDir
	cmd.Env = append([]string{}, os.Environ()...)
	for key, value := range envVars {
		cmd.Env = append(cmd.Env, key+"="+value)
	}

	var stdoutBuf bytes.Buffer
	var stderrBuf bytes.Buffer
	cmd.Stdout = &stdoutBuf
	cmd.Stderr = &stderrBuf
	if err := cmd.Run(); err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok {
			return stdoutBuf.String(), stderrBuf.String(), exitErr.ExitCode(), nil
		}
		return "", "", -1, err
	}
	return stdoutBuf.String(), stderrBuf.String(), 0, nil
}

func (h *fakeLocalConnectorHost) GetSandboxState(_ context.Context, envID string) (*types.SandboxState, error) {
	if h.state != nil {
		return h.state, nil
	}
	return &types.SandboxState{
		SandboxID:        envID,
		EnvironmentID:    envID,
		EnvironmentLabel: "Local VM",
	}, nil
}

func (h *fakeLocalConnectorHost) AcceptChanges(_ context.Context, envID string, paths []string, hostBaseDir string) (*types.AcceptChangesResult, error) {
	h.acceptedPaths = append([]string(nil), paths...)
	h.acceptedBaseDir = hostBaseDir
	if h.acceptResult != nil {
		return h.acceptResult, nil
	}
	return &types.AcceptChangesResult{
		AcceptedCount: len(paths),
		State: &types.SandboxState{
			SandboxID:        envID,
			EnvironmentID:    envID,
			EnvironmentLabel: "Local VM",
		},
		Summary: "accepted",
	}, nil
}

func (h *fakeLocalConnectorHost) DiscardAllChanges(_ context.Context, envID string) (*types.DiscardAllChangesResult, error) {
	if h.discardResult != nil {
		return h.discardResult, nil
	}
	return &types.DiscardAllChangesResult{
		State: &types.SandboxState{
			SandboxID:        envID,
			EnvironmentID:    envID,
			EnvironmentLabel: "Local VM",
		},
		Summary: "discarded",
	}, nil
}

func (h *fakeLocalConnectorHost) Cleanup(_ context.Context, _ string) error {
	h.cleanupCalls++
	return nil
}

func (h *fakeLocalConnectorHost) Close() error {
	h.closed = true
	return nil
}

type ctxBoundLocalConnectorHost struct {
	*fakeLocalConnectorHost
}

func (h *ctxBoundLocalConnectorHost) OpenFileReadStream(ctx context.Context, _ string, path string) (envhost.FileReadStream, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	info, err := os.Stat(path)
	if err != nil {
		return nil, err
	}
	return &ctxBoundFileReadStream{
		ctx:  ctx,
		data: data,
		info: envhost.FileStreamInfo{
			FileName:  filepath.Base(path),
			TotalSize: int64(len(data)),
			Mode:      uint32(info.Mode().Perm()),
		},
	}, nil
}

type ctxBoundFileReadStream struct {
	ctx    context.Context
	data   []byte
	info   envhost.FileStreamInfo
	offset int
}

func (s *ctxBoundFileReadStream) Read(p []byte) (int, error) {
	select {
	case <-s.ctx.Done():
		return 0, s.ctx.Err()
	default:
	}
	if s.offset >= len(s.data) {
		return 0, io.EOF
	}
	n := copy(p, s.data[s.offset:])
	s.offset += n
	return n, nil
}

func (s *ctxBoundFileReadStream) Close() error {
	return nil
}

func (s *ctxBoundFileReadStream) Info() envhost.FileStreamInfo {
	return s.info
}

type healthCheckLocalConnectorHost struct {
	*fakeLocalConnectorHost
	calls int
	err   error
}

func (h *healthCheckLocalConnectorHost) HealthCheck(context.Context) error {
	h.calls++
	return h.err
}

func TestLocalConnectorRuntimeJSONOps(t *testing.T) {
	root := t.TempDir()
	if err := os.WriteFile(filepath.Join(root, "alpha.txt"), []byte("hello\nworld\nthird\n"), 0o644); err != nil {
		t.Fatalf("seed alpha.txt: %v", err)
	}
	if err := os.Mkdir(filepath.Join(root, "nested"), 0o755); err != nil {
		t.Fatalf("mkdir nested: %v", err)
	}
	if err := os.WriteFile(filepath.Join(root, "nested", "beta.txt"), []byte("grep me\n"), 0o644); err != nil {
		t.Fatalf("seed beta.txt: %v", err)
	}
	if err := os.WriteFile(filepath.Join(root, "binary.bin"), []byte{0x00, 0x01, 0x02}, 0o644); err != nil {
		t.Fatalf("seed binary.bin: %v", err)
	}

	host := newFakeLocalConnectorHost(root)
	runtime, err := newRuntimeWithHost(host, host, localConnectorOptions{
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("newRuntimeWithHost: %v", err)
	}
	defer runtime.Close()

	writeJSON, err := runtime.WriteJSON("notes.txt", "hello from write", "utf8", 0o600)
	if err != nil {
		t.Fatalf("WriteJSON: %v", err)
	}
	var writeResult map[string]any
	mustDecodeJSON(t, writeJSON, &writeResult)
	if got := int(writeResult["size"].(float64)); got != len("hello from write") {
		t.Fatalf("unexpected write size: %d", got)
	}

	readJSON, err := runtime.ReadJSON("notes.txt", 0, 0)
	if err != nil {
		t.Fatalf("ReadJSON: %v", err)
	}
	var readResult map[string]any
	mustDecodeJSON(t, readJSON, &readResult)
	if got := readResult["content"].(string); got != "hello from write" {
		t.Fatalf("unexpected read content: %q", got)
	}
	if got := readResult["encoding"].(string); got != "utf8" {
		t.Fatalf("unexpected read encoding: %q", got)
	}

	openStreamJSON, err := runtime.OpenReadStreamJSON("alpha.txt")
	if err != nil {
		t.Fatalf("OpenReadStreamJSON: %v", err)
	}
	var openStreamResult struct {
		StreamID string `json:"stream_id"`
		Size     int64  `json:"size"`
	}
	mustDecodeJSON(t, openStreamJSON, &openStreamResult)
	if openStreamResult.StreamID == "" {
		t.Fatalf("expected stream id")
	}
	if openStreamResult.Size != int64(len("hello\nworld\nthird\n")) {
		t.Fatalf("unexpected stream size: %d", openStreamResult.Size)
	}

	var streamed bytes.Buffer
	for {
		chunkJSON, err := runtime.ReadStreamChunkJSON(openStreamResult.StreamID, 5)
		if err != nil {
			t.Fatalf("ReadStreamChunkJSON: %v", err)
		}
		var chunkResult struct {
			Content string `json:"content"`
			Bytes   int    `json:"bytes"`
			EOF     bool   `json:"eof"`
		}
		mustDecodeJSON(t, chunkJSON, &chunkResult)
		chunk, err := base64.StdEncoding.DecodeString(chunkResult.Content)
		if err != nil {
			t.Fatalf("decode stream chunk: %v", err)
		}
		if len(chunk) != chunkResult.Bytes {
			t.Fatalf("unexpected chunk size: bytes=%d decoded=%d", chunkResult.Bytes, len(chunk))
		}
		streamed.Write(chunk)
		if chunkResult.EOF {
			break
		}
	}
	if got := streamed.String(); got != "hello\nworld\nthird\n" {
		t.Fatalf("unexpected streamed content: %q", got)
	}
	if err := runtime.CloseReadStream(openStreamResult.StreamID); err != nil {
		t.Fatalf("CloseReadStream: %v", err)
	}

	paginatedJSON, err := runtime.ReadJSON("alpha.txt", 2, 1)
	if err != nil {
		t.Fatalf("ReadJSON paginated: %v", err)
	}
	var paginatedResult map[string]any
	mustDecodeJSON(t, paginatedJSON, &paginatedResult)
	if got := paginatedResult["content"].(string); got != "world" {
		t.Fatalf("unexpected paginated content: %q", got)
	}

	binaryJSON, err := runtime.ReadJSON("binary.bin", 0, 0)
	if err != nil {
		t.Fatalf("ReadJSON binary: %v", err)
	}
	var binaryResult map[string]any
	mustDecodeJSON(t, binaryJSON, &binaryResult)
	if got := binaryResult["encoding"].(string); got != "base64" {
		t.Fatalf("unexpected binary encoding: %q", got)
	}
	decoded, err := base64.StdEncoding.DecodeString(binaryResult["content"].(string))
	if err != nil {
		t.Fatalf("decode binary result: %v", err)
	}
	if string(decoded) != string([]byte{0x00, 0x01, 0x02}) {
		t.Fatalf("unexpected binary bytes: %v", decoded)
	}

	statJSON, err := runtime.StatJSON("notes.txt")
	if err != nil {
		t.Fatalf("StatJSON: %v", err)
	}
	var statResult map[string]any
	mustDecodeJSON(t, statJSON, &statResult)
	if got := statResult["kind"].(string); got != "file" {
		t.Fatalf("unexpected stat kind: %q", got)
	}
	if got := statResult["path"].(string); got != "notes.txt" {
		t.Fatalf("unexpected stat path: %q", got)
	}

	listJSON, err := runtime.ListJSON(".")
	if err != nil {
		t.Fatalf("ListJSON: %v", err)
	}
	var listResult struct {
		Entries []struct {
			Name string `json:"name"`
		} `json:"entries"`
	}
	mustDecodeJSON(t, listJSON, &listResult)
	if !containsEntry(listResult.Entries, "notes.txt") || !containsEntry(listResult.Entries, "nested") {
		t.Fatalf("unexpected list entries: %+v", listResult.Entries)
	}

	globJSON, err := runtime.GlobJSON("*.txt", "")
	if err != nil {
		t.Fatalf("GlobJSON root: %v", err)
	}
	var globResult struct {
		Matches []string `json:"matches"`
	}
	mustDecodeJSON(t, globJSON, &globResult)
	if !containsString(globResult.Matches, "alpha.txt") || containsString(globResult.Matches, "nested/beta.txt") {
		t.Fatalf("unexpected root glob matches: %+v", globResult.Matches)
	}

	recursiveGlobJSON, err := runtime.GlobJSON("**/*.txt", "")
	if err != nil {
		t.Fatalf("GlobJSON recursive: %v", err)
	}
	var recursiveGlobResult struct {
		Matches []string `json:"matches"`
	}
	mustDecodeJSON(t, recursiveGlobJSON, &recursiveGlobResult)
	if !containsString(recursiveGlobResult.Matches, "nested/beta.txt") {
		t.Fatalf("unexpected recursive glob matches: %+v", recursiveGlobResult.Matches)
	}

	rootGrepJSON, err := runtime.GrepJSON("grep", "", "*.txt")
	if err != nil {
		t.Fatalf("GrepJSON root: %v", err)
	}
	var rootGrepResult struct {
		Matches []struct {
			File    string `json:"file"`
			Line    int    `json:"line"`
			Content string `json:"content"`
		} `json:"matches"`
	}
	mustDecodeJSON(t, rootGrepJSON, &rootGrepResult)
	if len(rootGrepResult.Matches) != 0 {
		t.Fatalf("unexpected root grep matches: %+v", rootGrepResult.Matches)
	}

	grepJSON, err := runtime.GrepJSON("grep", "", "**/*.txt")
	if err != nil {
		t.Fatalf("GrepJSON: %v", err)
	}
	var grepResult struct {
		Matches []struct {
			File    string `json:"file"`
			Line    int    `json:"line"`
			Content string `json:"content"`
		} `json:"matches"`
	}
	mustDecodeJSON(t, grepJSON, &grepResult)
	if len(grepResult.Matches) != 1 || grepResult.Matches[0].File != "nested/beta.txt" || grepResult.Matches[0].Line != 1 {
		t.Fatalf("unexpected grep matches: %+v", grepResult.Matches)
	}

	execJSON, err := runtime.ExecJSON("printf 'exec-ok'", "", 5)
	if err != nil {
		t.Fatalf("ExecJSON: %v", err)
	}
	var execResult map[string]any
	mustDecodeJSON(t, execJSON, &execResult)
	if got := execResult["stdout"].(string); got != "exec-ok" {
		t.Fatalf("unexpected exec stdout: %q", got)
	}
	if got := int(execResult["exit_code"].(float64)); got != 0 {
		t.Fatalf("unexpected exec exit code: %d", got)
	}

	execJSON, err = runtime.ExecWithRuntimeEnvJSON("printf %s \"$CUEBOARD_TEST_ENV\"", "", 5, map[string]string{
		"CUEBOARD_TEST_ENV": "exec-env-ok",
	}, "", "")
	if err != nil {
		t.Fatalf("ExecWithRuntimeEnvJSON: %v", err)
	}
	mustDecodeJSON(t, execJSON, &execResult)
	if got := execResult["stdout"].(string); got != "exec-env-ok" {
		t.Fatalf("unexpected exec env stdout: %q", got)
	}
}

func TestOpenReadStreamJSONKeepsContextAliveUntilStreamClose(t *testing.T) {
	root := t.TempDir()
	const content = "hello\nworld\nthird\n"
	if err := os.WriteFile(filepath.Join(root, "alpha.txt"), []byte(content), 0o644); err != nil {
		t.Fatalf("seed alpha.txt: %v", err)
	}

	host := &ctxBoundLocalConnectorHost{fakeLocalConnectorHost: newFakeLocalConnectorHost(root)}
	runtime, err := newRuntimeWithHost(host, host, localConnectorOptions{
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("newRuntimeWithHost: %v", err)
	}
	defer runtime.Close()

	openStreamJSON, err := runtime.OpenReadStreamJSON("alpha.txt")
	if err != nil {
		t.Fatalf("OpenReadStreamJSON: %v", err)
	}
	var openStreamResult struct {
		StreamID string `json:"stream_id"`
	}
	mustDecodeJSON(t, openStreamJSON, &openStreamResult)
	if openStreamResult.StreamID == "" {
		t.Fatal("expected stream id")
	}

	var streamed bytes.Buffer
	for {
		chunkJSON, err := runtime.ReadStreamChunkJSON(openStreamResult.StreamID, 5)
		if err != nil {
			t.Fatalf("ReadStreamChunkJSON: %v", err)
		}
		var chunkResult struct {
			Content string `json:"content"`
			EOF     bool   `json:"eof"`
		}
		mustDecodeJSON(t, chunkJSON, &chunkResult)
		chunk, err := base64.StdEncoding.DecodeString(chunkResult.Content)
		if err != nil {
			t.Fatalf("decode stream chunk: %v", err)
		}
		streamed.Write(chunk)
		if chunkResult.EOF {
			break
		}
	}

	if got := streamed.String(); got != content {
		t.Fatalf("streamed content = %q, want %q", got, content)
	}
}

func TestSharedRuntimeHealthCheckUsesHostHealthCheck(t *testing.T) {
	root := t.TempDir()
	healthErr := errors.New("daemon unavailable")
	host := &healthCheckLocalConnectorHost{
		fakeLocalConnectorHost: newFakeLocalConnectorHost(root),
		err:                    healthErr,
	}
	runtime, err := newSharedRuntimeWithHost(host, host, localConnectorOptions{
		rootPath:    root,
		metadataDir: t.TempDir(),
	})
	if err != nil {
		t.Fatalf("newSharedRuntimeWithHost: %v", err)
	}
	defer runtime.Close()

	if err := runtime.HealthCheck(); !errors.Is(err, healthErr) {
		t.Fatalf("HealthCheck error = %v, want %v", err, healthErr)
	}
	if host.calls != 1 {
		t.Fatalf("HealthCheck calls = %d, want 1", host.calls)
	}
}

func TestSharedRuntimeHealthCheckFallsBackToMetadata(t *testing.T) {
	root := t.TempDir()
	host := newFakeLocalConnectorHost(root)
	runtime, err := newSharedRuntimeWithHost(host, host, localConnectorOptions{
		rootPath:    root,
		metadataDir: t.TempDir(),
	})
	if err != nil {
		t.Fatalf("newSharedRuntimeWithHost: %v", err)
	}
	defer runtime.Close()

	if _, err := runtime.runtimeForSession("session-1"); err != nil {
		t.Fatalf("runtimeForSession: %v", err)
	}
	metadataErr := errors.New("metadata unavailable")
	host.metadataErrs = map[string]error{host.createdEnvID: metadataErr}

	if err := runtime.HealthCheck(); !errors.Is(err, metadataErr) {
		t.Fatalf("HealthCheck error = %v, want %v", err, metadataErr)
	}
}

func TestLocalConnectorRuntimeWorkspaceReviewJSONOps(t *testing.T) {
	root := t.TempDir()
	workspaceFile := filepath.Join(root, "notes.txt")
	host := newFakeLocalConnectorHost(root)
	host.state = &types.SandboxState{
		SandboxID:        "env-1",
		EnvironmentID:    "env-1",
		EnvironmentLabel: "Local VM",
		FileDiff: []types.FileDiff{{
			Path:      workspaceFile,
			IsUpdated: true,
		}},
	}
	host.acceptResult = &types.AcceptChangesResult{
		AcceptedCount: 1,
		RejectedCount: 0,
		State: &types.SandboxState{
			SandboxID:        "env-1",
			EnvironmentID:    "env-1",
			EnvironmentLabel: "Local VM",
		},
		ReviewDiff: []types.FileDiff{{
			Path:      workspaceFile,
			IsUpdated: true,
		}},
		Summary: "1 files changed, 1 accepted, 0 rejected.",
	}
	host.discardResult = &types.DiscardAllChangesResult{
		TotalFiles: 1,
		State: &types.SandboxState{
			SandboxID:        "env-1",
			EnvironmentID:    "env-1",
			EnvironmentLabel: "Local VM",
		},
		Summary: "1 files changed, 0 accepted, 1 rejected.",
	}

	runtime, err := newRuntimeWithHost(host, host, localConnectorOptions{
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("newRuntimeWithHost: %v", err)
	}
	defer runtime.Close()

	stateJSON, err := runtime.GetSandboxStateJSON()
	if err != nil {
		t.Fatalf("GetSandboxStateJSON: %v", err)
	}
	var state types.SandboxState
	mustDecodeJSON(t, stateJSON, &state)
	if got := len(state.FileDiff); got != 1 {
		t.Fatalf("unexpected file diff count: %d", got)
	}

	acceptJSON, err := runtime.AcceptChangesJSON(`["` + workspaceFile + `"]`)
	if err != nil {
		t.Fatalf("AcceptChangesJSON: %v", err)
	}
	var accept types.AcceptChangesResult
	mustDecodeJSON(t, acceptJSON, &accept)
	if accept.AcceptedCount != 1 {
		t.Fatalf("unexpected accepted count: %d", accept.AcceptedCount)
	}
	if len(host.acceptedPaths) != 1 || host.acceptedPaths[0] != workspaceFile {
		t.Fatalf("unexpected accepted paths: %+v", host.acceptedPaths)
	}
	if host.acceptedBaseDir != string(filepath.Separator) {
		t.Fatalf("unexpected accepted base dir: %q", host.acceptedBaseDir)
	}

	discardJSON, err := runtime.DiscardAllChangesJSON()
	if err != nil {
		t.Fatalf("DiscardAllChangesJSON: %v", err)
	}
	var discard types.DiscardAllChangesResult
	mustDecodeJSON(t, discardJSON, &discard)
	if discard.TotalFiles != 1 {
		t.Fatalf("unexpected discard total: %d", discard.TotalFiles)
	}

	if host.cleanupCalls < 3 {
		t.Fatalf("expected cleanup before each review operation, got %d", host.cleanupCalls)
	}
}

func TestLocalConnectorRuntimeDeleteAndClose(t *testing.T) {
	root := t.TempDir()
	if err := os.MkdirAll(filepath.Join(root, "dir", "child"), 0o755); err != nil {
		t.Fatalf("mkdir dir/child: %v", err)
	}
	if err := os.WriteFile(filepath.Join(root, "dir", "child", "file.txt"), []byte("x"), 0o644); err != nil {
		t.Fatalf("seed file.txt: %v", err)
	}

	host := newFakeLocalConnectorHost(root)
	runtime, err := newRuntimeWithHost(host, host, localConnectorOptions{
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("newRuntimeWithHost: %v", err)
	}

	if _, err := runtime.DeleteJSON("dir", true); err != nil {
		t.Fatalf("DeleteJSON recursive: %v", err)
	}
	if _, err := os.Stat(filepath.Join(root, "dir")); !os.IsNotExist(err) {
		t.Fatalf("expected dir to be removed, stat err=%v", err)
	}

	if err := runtime.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if host.deletedEnvID != host.createdEnvID || host.deletedEnvID == "" {
		t.Fatalf("unexpected deleted env id: created=%q deleted=%q", host.createdEnvID, host.deletedEnvID)
	}
	if !host.closed {
		t.Fatalf("expected host to be closed")
	}
	if _, err := runtime.ExecJSON("pwd", "", 1); err == nil || !strings.Contains(err.Error(), "closed") {
		t.Fatalf("expected closed runtime error, got %v", err)
	}
}

func TestLocalConnectorRuntimeClosePreservingState(t *testing.T) {
	root := t.TempDir()
	host := newFakeLocalConnectorHost(root)
	runtime, err := newRuntimeWithHost(host, host, localConnectorOptions{
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("newRuntimeWithHost: %v", err)
	}

	if err := runtime.ClosePreservingState(); err != nil {
		t.Fatalf("ClosePreservingState: %v", err)
	}
	if host.deletedEnvID != "" {
		t.Fatalf("expected preserve close to keep environment, deleted=%q", host.deletedEnvID)
	}
	if !host.closed {
		t.Fatalf("expected host to be closed")
	}
}

func TestLocalConnectorRuntimeRestoresExistingEnvironment(t *testing.T) {
	root := t.TempDir()
	host := newFakeLocalConnectorHost(root)
	host.restoreOK = true
	host.restoredEnvID = "env-restored"

	runtime, err := newRuntimeWithHost(host, host, localConnectorOptions{
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("newRuntimeWithHost: %v", err)
	}
	defer runtime.Close()

	if !host.restoreCalled {
		t.Fatalf("expected restore to be attempted")
	}
	if host.createdEnvID != "" {
		t.Fatalf("expected restore path to skip create, created=%q", host.createdEnvID)
	}
	if runtime.environmentID != "env-restored" {
		t.Fatalf("unexpected runtime environment id: %q", runtime.environmentID)
	}
}

func TestLocalConnectorRuntimeExecWithRuntime(t *testing.T) {
	root := t.TempDir()
	host := newFakeLocalConnectorHost(root)
	runtime, err := newRuntimeWithHost(host, host, localConnectorOptions{
		rootPath:           root,
		capabilityProvider: apiproxy.NewCapabilityProvider(),
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("newRuntimeWithHost: %v", err)
	}
	defer runtime.Close()

	if _, err := runtime.ExecWithRuntimeJSON("printf 'exec-ok'", "", 5, "session-1", "agent-1"); err != nil {
		t.Fatalf("ExecWithRuntimeJSON: %v", err)
	}
	if host.lastRuntime == nil || strings.TrimSpace(host.lastRuntime.CapabilityToken) == "" {
		t.Fatalf("expected runtime capability token, got %+v", host.lastRuntime)
	}
}

func TestLocalConnectorRuntimeRejectsEscapingPaths(t *testing.T) {
	root := t.TempDir()
	host := newFakeLocalConnectorHost(root)
	runtime, err := newRuntimeWithHost(host, host, localConnectorOptions{
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("newRuntimeWithHost: %v", err)
	}
	defer runtime.Close()

	if _, err := runtime.StatJSON("../outside.txt"); err == nil || !strings.Contains(err.Error(), "escapes root") {
		t.Fatalf("expected escapes root error, got %v", err)
	}
}

func TestSharedRuntimeSharesOneHostAcrossSessions(t *testing.T) {
	root := t.TempDir()
	host := newFakeLocalConnectorHost(root)
	runtime, err := newSharedRuntimeWithHost(host, host, localConnectorOptions{
		metadataDir:        filepath.Join(root, "metadata"),
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("newSharedRuntimeWithHost: %v", err)
	}

	if _, err := runtime.WriteJSON("session-1", "alpha.txt", "one", "utf8", 0o644); err != nil {
		t.Fatalf("WriteJSON session-1: %v", err)
	}
	if _, err := runtime.WriteJSON("session-2", "beta.txt", "two", "utf8", 0o644); err != nil {
		t.Fatalf("WriteJSON session-2: %v", err)
	}
	if _, err := runtime.StatJSON("session-1", "alpha.txt"); err != nil {
		t.Fatalf("StatJSON session-1: %v", err)
	}

	if got := len(host.createdEnvIDs); got != 2 {
		t.Fatalf("expected one environment per session, got %d (%v)", got, host.createdEnvIDs)
	}
	if !runtime.HasSessionState("session-1") || !runtime.HasSessionState("session-2") {
		t.Fatalf("expected shared runtime to persist both session mappings")
	}
	if host.closed {
		t.Fatalf("expected shared host to stay open before shared runtime close")
	}

	if err := runtime.CloseSessionPreservingState("session-1"); err != nil {
		t.Fatalf("CloseSessionPreservingState: %v", err)
	}
	if host.closed {
		t.Fatalf("expected closing one session to keep shared host alive")
	}

	if err := runtime.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}
	if !host.closed {
		t.Fatalf("expected shared host to close once")
	}
}

func TestSharedRuntimeRestoresSessionEnvironmentMapping(t *testing.T) {
	root := t.TempDir()
	metadataDir := filepath.Join(root, "metadata")

	firstHost := newFakeLocalConnectorHost(root)
	firstRuntime, err := newSharedRuntimeWithHost(firstHost, firstHost, localConnectorOptions{
		metadataDir:        metadataDir,
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("first newSharedRuntimeWithHost: %v", err)
	}
	if _, err := firstRuntime.WriteJSON("session-1", "alpha.txt", "one", "utf8", 0o644); err != nil {
		t.Fatalf("first WriteJSON: %v", err)
	}
	if err := firstRuntime.Close(); err != nil {
		t.Fatalf("first Close: %v", err)
	}

	secondHost := newFakeLocalConnectorHost(root)
	secondHost.envs["env-1"] = true
	secondRuntime, err := newSharedRuntimeWithHost(secondHost, secondHost, localConnectorOptions{
		metadataDir:        metadataDir,
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("second newSharedRuntimeWithHost: %v", err)
	}
	defer secondRuntime.Close()

	if !secondRuntime.HasSessionState("session-1") {
		t.Fatalf("expected session mapping to survive shared runtime restart")
	}
	if _, err := secondRuntime.ReadJSON("session-1", "alpha.txt", 0, 0); err != nil {
		t.Fatalf("ReadJSON restored session: %v", err)
	}
	if got := len(secondHost.createdEnvIDs); got != 0 {
		t.Fatalf("expected restored session to skip environment creation, got %d", got)
	}
}

func TestSharedRuntimeDeleteSessionStateRemovesMapping(t *testing.T) {
	root := t.TempDir()
	host := newFakeLocalConnectorHost(root)
	runtime, err := newSharedRuntimeWithHost(host, host, localConnectorOptions{
		metadataDir:        filepath.Join(root, "metadata"),
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("newSharedRuntimeWithHost: %v", err)
	}
	defer runtime.Close()

	if _, err := runtime.WriteJSON("session-1", "alpha.txt", "one", "utf8", 0o644); err != nil {
		t.Fatalf("WriteJSON: %v", err)
	}
	if err := runtime.DeleteSessionState("session-1"); err != nil {
		t.Fatalf("DeleteSessionState: %v", err)
	}
	if runtime.HasSessionState("session-1") {
		t.Fatalf("expected delete to remove session mapping")
	}
	if host.deletedEnvID != "env-1" {
		t.Fatalf("unexpected deleted env id: %q", host.deletedEnvID)
	}
	if _, err := runtime.WriteJSON("session-1", "alpha.txt", "two", "utf8", 0o644); err != nil {
		t.Fatalf("WriteJSON recreated session: %v", err)
	}
	if host.createdEnvID != "env-2" {
		t.Fatalf("expected recreated session to get a fresh environment, got %q", host.createdEnvID)
	}
}

func TestSharedRuntimeUpdateBackendConfigRefreshesExecRuntime(t *testing.T) {
	root := t.TempDir()
	host := newFakeLocalConnectorHost(root)
	runtime, err := newSharedRuntimeWithHost(host, host, localConnectorOptions{
		metadataDir:        filepath.Join(root, "metadata"),
		rootPath:           root,
		backendURL:         "http://backend-a",
		backendAPIKey:      "token-a",
		capabilityProvider: apiproxy.NewCapabilityProvider(),
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("newSharedRuntimeWithHost: %v", err)
	}
	defer runtime.Close()

	if _, err := runtime.ExecWithRuntimeJSON("session-1", "printf 'first'", "", 5, "session-1", "agent-1"); err != nil {
		t.Fatalf("ExecWithRuntimeJSON before update: %v", err)
	}
	if host.lastRuntime == nil {
		t.Fatalf("expected runtime config before update")
	}
	if host.lastRuntime.BackendURL != "http://backend-a" || host.lastRuntime.BackendAPIKey != "token-a" {
		t.Fatalf("unexpected runtime config before update: %+v", host.lastRuntime)
	}

	runtime.UpdateBackendConfig("http://backend-b", "token-b")

	if _, err := runtime.ExecWithRuntimeJSON("session-1", "printf 'second'", "", 5, "session-1", "agent-1"); err != nil {
		t.Fatalf("ExecWithRuntimeJSON after update: %v", err)
	}
	if host.lastRuntime == nil {
		t.Fatalf("expected runtime config after update")
	}
	if host.lastRuntime.BackendURL != "http://backend-b" || host.lastRuntime.BackendAPIKey != "token-b" {
		t.Fatalf("unexpected runtime config after update: %+v", host.lastRuntime)
	}
}

func TestSharedRuntimeStoreScopeMismatchSkipsRestore(t *testing.T) {
	root := t.TempDir()
	metadataDir := filepath.Join(root, "metadata")

	firstHost := newFakeLocalConnectorHost(root)
	firstRuntime, err := newSharedRuntimeWithHost(firstHost, firstHost, localConnectorOptions{
		metadataDir:        metadataDir,
		rootPath:           root,
		rootfsOverlayDir:   filepath.Join(root, "overlay-a"),
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("first newSharedRuntimeWithHost: %v", err)
	}
	if _, err := firstRuntime.WriteJSON("session-1", "alpha.txt", "one", "utf8", 0o644); err != nil {
		t.Fatalf("first WriteJSON: %v", err)
	}
	if err := firstRuntime.Close(); err != nil {
		t.Fatalf("first Close: %v", err)
	}

	secondHost := newFakeLocalConnectorHost(root)
	secondHost.envs["env-1"] = true
	secondRuntime, err := newSharedRuntimeWithHost(secondHost, secondHost, localConnectorOptions{
		metadataDir:        metadataDir,
		rootPath:           filepath.Join(root, "different-root"),
		rootfsOverlayDir:   filepath.Join(root, "overlay-b"),
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("second newSharedRuntimeWithHost: %v", err)
	}
	defer secondRuntime.Close()

	if secondRuntime.HasSessionState("session-1") {
		t.Fatalf("expected scope mismatch to discard persisted mapping")
	}
	if _, err := secondRuntime.WriteJSON("session-1", "beta.txt", "two", "utf8", 0o644); err != nil {
		t.Fatalf("second WriteJSON: %v", err)
	}
	if got := len(secondHost.createdEnvIDs); got != 1 {
		t.Fatalf("expected new environment after scope mismatch, got %d", got)
	}
}

func TestSharedRuntimeRecreatesMissingPersistedEnvironment(t *testing.T) {
	root := t.TempDir()
	metadataDir := filepath.Join(root, "metadata")

	firstHost := newFakeLocalConnectorHost(root)
	firstRuntime, err := newSharedRuntimeWithHost(firstHost, firstHost, localConnectorOptions{
		metadataDir:        metadataDir,
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("first newSharedRuntimeWithHost: %v", err)
	}
	if _, err := firstRuntime.WriteJSON("session-1", "alpha.txt", "one", "utf8", 0o644); err != nil {
		t.Fatalf("first WriteJSON: %v", err)
	}
	if err := firstRuntime.Close(); err != nil {
		t.Fatalf("first Close: %v", err)
	}

	secondHost := newFakeLocalConnectorHost(root)
	secondRuntime, err := newSharedRuntimeWithHost(secondHost, secondHost, localConnectorOptions{
		metadataDir:        metadataDir,
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("second newSharedRuntimeWithHost: %v", err)
	}
	defer secondRuntime.Close()

	if _, err := secondRuntime.WriteJSON("session-1", "beta.txt", "two", "utf8", 0o644); err != nil {
		t.Fatalf("second WriteJSON: %v", err)
	}
	if got := len(secondHost.createdEnvIDs); got != 1 {
		t.Fatalf("expected missing persisted environment to be recreated, got %d", got)
	}
}

func TestSharedRuntimeDoesNotReplacePersistedEnvironmentOnTransientMetadataError(t *testing.T) {
	root := t.TempDir()
	metadataDir := filepath.Join(root, "metadata")

	firstHost := newFakeLocalConnectorHost(root)
	firstRuntime, err := newSharedRuntimeWithHost(firstHost, firstHost, localConnectorOptions{
		metadataDir:        metadataDir,
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("first newSharedRuntimeWithHost: %v", err)
	}
	if _, err := firstRuntime.WriteJSON("session-1", "alpha.txt", "one", "utf8", 0o644); err != nil {
		t.Fatalf("first WriteJSON: %v", err)
	}
	if err := firstRuntime.Close(); err != nil {
		t.Fatalf("first Close: %v", err)
	}

	secondHost := newFakeLocalConnectorHost(root)
	secondHost.envs["env-1"] = true
	secondHost.metadataErrs = map[string]error{
		"env-1": errors.New("temporary metadata failure"),
	}
	secondRuntime, err := newSharedRuntimeWithHost(secondHost, secondHost, localConnectorOptions{
		metadataDir:        metadataDir,
		rootPath:           root,
		readMaxBytes:       1024,
		maxMatches:         100,
		execOutputMaxBytes: 1024,
	})
	if err != nil {
		t.Fatalf("second newSharedRuntimeWithHost: %v", err)
	}
	defer secondRuntime.Close()

	if _, err := secondRuntime.ReadJSON("session-1", "alpha.txt", 0, 0); err == nil || !strings.Contains(err.Error(), "temporary metadata failure") {
		t.Fatalf("expected transient metadata failure, got %v", err)
	}
	if got := len(secondHost.createdEnvIDs); got != 0 {
		t.Fatalf("expected transient metadata failure to avoid recreation, got %d", got)
	}
	if !secondRuntime.HasSessionState("session-1") {
		t.Fatalf("expected transient metadata failure to preserve persisted mapping")
	}
}

func mustDecodeJSON(t *testing.T, input string, target any) {
	t.Helper()
	if err := json.Unmarshal([]byte(input), target); err != nil {
		t.Fatalf("unmarshal json %q: %v", input, err)
	}
}

func containsString(values []string, want string) bool {
	for _, value := range values {
		if value == want {
			return true
		}
	}
	return false
}

func containsEntry(values []struct {
	Name string `json:"name"`
}, want string) bool {
	for _, value := range values {
		if value.Name == want {
			return true
		}
	}
	return false
}
