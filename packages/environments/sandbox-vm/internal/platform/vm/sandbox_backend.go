package vm

import (
	"context"
	"io"

	"github.com/nora/sandbox-vm/internal/envhost"
	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
	"github.com/nora/sandbox-vm/pkg/types"
)

// SandboxBackend is the common interface shared by Manager (local VM) and RemoteManager (fly-vault VM).
// It provides all sandbox operations needed by EnvironmentHost implementations.
type SandboxBackend interface {
	CreateSandbox(ctx context.Context) (string, error)
	DeleteSandbox(ctx context.Context, sandboxID string) error
	MountSandbox(ctx context.Context, sandboxID string) error
	UnmountSandbox(ctx context.Context, sandboxID string) error
	IsSandboxMounted(sandboxID string) bool
	RunSandboxHousekeeper(ctx context.Context, sandboxID string) error

	ExecInSandboxWithEnv(ctx context.Context, sandboxID string, args []string, workDir string, extraEnv map[string]string) (stdout, stderr string, exitCode int, err error)
	ExecInSandboxStreamWithEnv(ctx context.Context, sandboxID string, args []string, workDir string, extraEnv map[string]string, onStdout func([]byte), onStderr func([]byte), onExit func(int)) error
	ExecutePython(ctx context.Context, req *vmrpc.ExecutePythonRequest) (*vmrpc.ExecutePythonResponse, error)
	ExecutePythonStream(ctx context.Context, req *vmrpc.ExecutePythonRequest, onStdout func([]byte), onStderr func([]byte), onExit func(int)) error

	ReadSandboxFile(ctx context.Context, sandboxID, path string) ([]byte, error)
	OpenSandboxFileReadStream(ctx context.Context, sandboxID, path string) (envhost.FileReadStream, error)
	WriteSandboxFile(ctx context.Context, sandboxID, path string, content []byte, appendMode bool) error
	OpenSandboxFileWriteStream(ctx context.Context, sandboxID, path string, opts envhost.FileWriteOptions) (envhost.FileWriteStream, error)
	DeleteSandboxFile(ctx context.Context, sandboxID, path string) error
	SandboxFileExists(ctx context.Context, sandboxID, path string) (bool, error)
	ExportSandboxFile(ctx context.Context, sandboxID, srcPath, dstPath string) (*types.ExportFileResult, error)

	GetSandboxState(ctx context.Context, sandboxID string) (*types.SandboxState, error)
	ExportSandboxDiff(ctx context.Context, sandboxID string, paths []string, output io.Writer) (*types.ExportDiffResult, error)
	DiscardSandboxAllChanges(ctx context.Context, sandboxID string) error
	ApplySandboxDiff(ctx context.Context, sandboxID string, paths []string, hostBaseDir string) error

	GetMounts() []Mount
}
