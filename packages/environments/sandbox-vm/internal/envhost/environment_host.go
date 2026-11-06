package envhost

import (
	"context"

	"github.com/nora/sandbox-vm/pkg/types"
)

// EnvironmentHost is the framework-side abstraction for one connected host.
// The implementation may be direct in-process or backed by RPC.
type EnvironmentHost interface {
	Info() HostInfo
	SubscribeHostStateChanged(onChange func()) (unsubscribe func())
	CreateEnvironment(ctx context.Context) (EnvironmentMetadata, error)
	DeleteEnvironment(ctx context.Context, envID string) error
	Metadata(ctx context.Context, envID string) (EnvironmentMetadata, error)
	Prompt(ctx context.Context, envID string) (string, error)
	ExecuteCommand(ctx context.Context, envID string, args []string, workingDir string, envVars map[string]string, runtime *RuntimeConfig) (stdout, stderr string, exitCode int, err error)
	ExecuteCommandStream(
		ctx context.Context,
		envID string,
		args []string,
		workingDir string,
		envVars map[string]string,
		runtime *RuntimeConfig,
		onStdout func([]byte),
		onStderr func([]byte),
		onExit func(int),
	) error
	ExecutePython(ctx context.Context, envID string, code string, envVars map[string]string, runtime *RuntimeConfig) (stdout, stderr string, exitCode int, err error)
	ExecutePythonStream(
		ctx context.Context,
		envID string,
		code string,
		envVars map[string]string,
		runtime *RuntimeConfig,
		onStdout func([]byte),
		onStderr func([]byte),
		onExit func(int),
	) error
	ReadFile(ctx context.Context, envID string, path string) ([]byte, error)
	OpenFileReadStream(ctx context.Context, envID string, path string) (FileReadStream, error)
	WriteFile(ctx context.Context, envID string, path string, content []byte, appendMode bool) error
	OpenFileWriteStream(ctx context.Context, envID string, path string, opts FileWriteOptions) (FileWriteStream, error)
	DeleteFile(ctx context.Context, envID string, path string) error
	FileExists(ctx context.Context, envID string, path string) (bool, error)
	GetSandboxState(ctx context.Context, envID string) (*types.SandboxState, error)
	AcceptChanges(ctx context.Context, envID string, paths []string, hostBaseDir string) (*types.AcceptChangesResult, error)
	DiscardAllChanges(ctx context.Context, envID string) (*types.DiscardAllChangesResult, error)
	ExportFile(ctx context.Context, envID string, srcPath string, dstPath string) (*types.ExportFileResult, error)
	Cleanup(ctx context.Context, envID string) error
}
