package envhost

import (
	"context"

	"github.com/nora/sandbox-vm/pkg/types"
)

// BaseEnvironmentHost provides default EnvironmentHost implementations that
// report capability_not_supported. Embed it in tests or partial host
// implementations when only a subset of base host methods are relevant.
type BaseEnvironmentHost struct{}

func (BaseEnvironmentHost) Info() HostInfo {
	return HostInfo{}
}

func (BaseEnvironmentHost) SubscribeHostStateChanged(onChange func()) func() {
	return func() {}
}

func (BaseEnvironmentHost) CreateEnvironment(ctx context.Context) (EnvironmentMetadata, error) {
	return EnvironmentMetadata{}, NewProtocolError(ErrCodeCapabilityNotSupported, "create environment not supported")
}

func (BaseEnvironmentHost) DeleteEnvironment(ctx context.Context, envID string) error {
	return NewProtocolError(ErrCodeCapabilityNotSupported, "delete environment not supported")
}

func (BaseEnvironmentHost) Metadata(ctx context.Context, envID string) (EnvironmentMetadata, error) {
	return EnvironmentMetadata{}, NewProtocolError(ErrCodeCapabilityNotSupported, "get environment metadata not supported")
}

func (BaseEnvironmentHost) Prompt(ctx context.Context, envID string) (string, error) {
	return "", NewProtocolError(ErrCodeCapabilityNotSupported, "get environment prompt not supported")
}

func (BaseEnvironmentHost) ExecuteCommand(ctx context.Context, envID string, args []string, workingDir string, envVars map[string]string, runtime *RuntimeConfig) (string, string, int, error) {
	return "", "", 0, NewProtocolError(ErrCodeCapabilityNotSupported, "command execution not supported")
}

func (BaseEnvironmentHost) ExecuteCommandStream(ctx context.Context, envID string, args []string, workingDir string, envVars map[string]string, runtime *RuntimeConfig, onStdout func([]byte), onStderr func([]byte), onExit func(int)) error {
	return NewProtocolError(ErrCodeCapabilityNotSupported, "command streaming not supported")
}

func (BaseEnvironmentHost) ExecutePython(ctx context.Context, envID string, code string, envVars map[string]string, runtime *RuntimeConfig) (string, string, int, error) {
	return "", "", 0, NewProtocolError(ErrCodeCapabilityNotSupported, "python execution not supported")
}

func (BaseEnvironmentHost) ExecutePythonStream(ctx context.Context, envID string, code string, envVars map[string]string, runtime *RuntimeConfig, onStdout func([]byte), onStderr func([]byte), onExit func(int)) error {
	return NewProtocolError(ErrCodeCapabilityNotSupported, "python streaming not supported")
}

func (BaseEnvironmentHost) ReadFile(ctx context.Context, envID string, path string) ([]byte, error) {
	return nil, NewProtocolError(ErrCodeCapabilityNotSupported, "file read not supported")
}

func (BaseEnvironmentHost) OpenFileReadStream(ctx context.Context, envID string, path string) (FileReadStream, error) {
	return nil, NewProtocolError(ErrCodeCapabilityNotSupported, "file read streaming not supported")
}

func (BaseEnvironmentHost) WriteFile(ctx context.Context, envID string, path string, content []byte, appendMode bool) error {
	return NewProtocolError(ErrCodeCapabilityNotSupported, "file write not supported")
}

func (BaseEnvironmentHost) OpenFileWriteStream(ctx context.Context, envID string, path string, opts FileWriteOptions) (FileWriteStream, error) {
	return nil, NewProtocolError(ErrCodeCapabilityNotSupported, "file write streaming not supported")
}

func (BaseEnvironmentHost) DeleteFile(ctx context.Context, envID string, path string) error {
	return NewProtocolError(ErrCodeCapabilityNotSupported, "file delete not supported")
}

func (BaseEnvironmentHost) FileExists(ctx context.Context, envID string, path string) (bool, error) {
	return false, NewProtocolError(ErrCodeCapabilityNotSupported, "file exists not supported")
}

func (BaseEnvironmentHost) GetSandboxState(ctx context.Context, envID string) (*types.SandboxState, error) {
	return nil, NewProtocolError(ErrCodeCapabilityNotSupported, "workspace state not supported")
}

func (BaseEnvironmentHost) AcceptChanges(ctx context.Context, envID string, paths []string, hostBaseDir string) (*types.AcceptChangesResult, error) {
	return nil, NewProtocolError(ErrCodeCapabilityNotSupported, "workspace review not supported")
}

func (BaseEnvironmentHost) DiscardAllChanges(ctx context.Context, envID string) (*types.DiscardAllChangesResult, error) {
	return nil, NewProtocolError(ErrCodeCapabilityNotSupported, "workspace review not supported")
}

func (BaseEnvironmentHost) ExportFile(ctx context.Context, envID string, srcPath string, dstPath string) (*types.ExportFileResult, error) {
	return nil, NewProtocolError(ErrCodeCapabilityNotSupported, "workspace file export not supported")
}

func (BaseEnvironmentHost) Cleanup(ctx context.Context, envID string) error {
	return nil
}
