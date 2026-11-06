//go:build linux

// Package server implements the sandbox gRPC server that runs inside the VM.
package server

import (
	"archive/tar"
	"compress/gzip"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"log"
	"mime"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/google/uuid"
	"github.com/nora/sandbox-vm/pkg/filestream"
	"google.golang.org/grpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"

	"github.com/nora/sandbox-vm/internal/platform/vm/vmd/ops"
	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
)

var sandboxIDPattern = regexp.MustCompile(`^[a-zA-Z0-9._-]+$`)

// vmDaemonServer implements vmrpc.VMServiceServer.
type vmDaemonServer struct {
	vmrpc.UnimplementedVMServiceServer
	config       *ops.PathConfig
	sandboxMgr   *ops.SandboxManager
	workspaceMgr *ops.WorkspaceManager
	proxyMgr     *ops.ProxyManager
}

type execOutputSender struct {
	mu     sync.Mutex
	stream grpc.ServerStream
	err    error
}

func (s *execOutputSender) send(msg *vmrpc.ExecOutput) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.err != nil {
		return s.err
	}
	if err := s.stream.SendMsg(msg); err != nil {
		s.err = err
	}
	return s.err
}

func (s *execOutputSender) Err() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.err
}

func newVMDaemonServer() (*vmDaemonServer, error) {
	config, err := ops.DefaultPathConfig()
	if err != nil {
		return nil, fmt.Errorf("failed to initialize path config: %w", err)
	}

	srv := &vmDaemonServer{
		config:       config,
		sandboxMgr:   ops.NewSandboxManager(config),
		workspaceMgr: ops.NewWorkspaceManager(config),
		proxyMgr:     ops.NewProxyManager(),
	}
	return srv, nil
}

func validateID(id string) error {
	if id == "" {
		return status.Error(codes.InvalidArgument, "ID is required")
	}
	if !sandboxIDPattern.MatchString(id) {
		return status.Errorf(codes.InvalidArgument, "invalid ID: must match pattern %s", sandboxIDPattern.String())
	}
	return nil
}

// getOrRestoreSandbox gets a sandbox from memory, or restores it from disk if it exists.
// This enables sandbox persistence across VM restarts.
func (s *vmDaemonServer) getOrRestoreSandbox(sandboxID string) (*ops.Sandbox, error) {
	sandbox, err := s.sandboxMgr.Get(sandboxID)
	if err != nil {
		// Sandbox not in memory, try to restore from disk if it exists
		if s.sandboxMgr.Exists(sandboxID) {
			log.Printf("[gRPC] Sandbox %s not in memory, restoring from disk", sandboxID)
			sandbox, err = s.sandboxMgr.Restore(sandboxID)
			if err != nil {
				return nil, fmt.Errorf("failed to restore sandbox: %w", err)
			}
			return sandbox, nil
		}
		return nil, fmt.Errorf("sandbox %s not found", sandboxID)
	}
	return sandbox, nil
}

func (s *vmDaemonServer) Health(ctx context.Context, req *vmrpc.HealthRequest) (*vmrpc.HealthResponse, error) {
	return &vmrpc.HealthResponse{Status: "ok"}, nil
}

func (s *vmDaemonServer) ResetSharedEnv(ctx context.Context, req *vmrpc.ResetSharedEnvRequest) (*vmrpc.ResetSharedEnvResponse, error) {
	log.Printf("[gRPC] ResetSharedEnv called")
	return nil, status.Errorf(codes.Unimplemented, "reset not supported: restart VM to reset shared environment")
}

func (s *vmDaemonServer) CreateSandbox(ctx context.Context, req *vmrpc.CreateSandboxRequest) (*vmrpc.CreateSandboxResponse, error) {
	// Generate a new sandbox ID
	sandboxID := uuid.New().String()
	log.Printf("[gRPC] CreateSandbox called: generated sandboxID=%s", sandboxID)

	sandbox, err := s.sandboxMgr.Create(sandboxID)
	if err != nil {
		log.Printf("[gRPC] CreateSandbox failed: %v", err)
		return nil, status.Errorf(codes.Internal, "failed to create sandbox: %v", err)
	}
	log.Printf("[gRPC] CreateSandbox succeeded: %s", sandbox.RootDir)
	return &vmrpc.CreateSandboxResponse{SandboxId: sandboxID, SandboxRoot: sandbox.RootDir}, nil
}

func (s *vmDaemonServer) DeleteSandbox(ctx context.Context, req *vmrpc.DeleteSandboxRequest) (*vmrpc.DeleteSandboxResponse, error) {
	log.Printf("[gRPC] DeleteSandbox called: sandboxID=%s", req.SandboxId)
	if err := validateID(req.SandboxId); err != nil {
		return nil, err
	}
	sandbox, err := s.sandboxMgr.Get(req.SandboxId)
	if err != nil {
		log.Printf("[gRPC] DeleteSandbox: sandbox not found, nothing to cleanup")
		return &vmrpc.DeleteSandboxResponse{}, nil
	}
	if err := sandbox.Cleanup(); err != nil {
		log.Printf("[gRPC] DeleteSandbox failed: %v", err)
		return nil, status.Errorf(codes.Internal, "failed to cleanup sandbox: %v", err)
	}
	s.sandboxMgr.Delete(req.SandboxId)
	log.Printf("[gRPC] DeleteSandbox succeeded")
	return &vmrpc.DeleteSandboxResponse{}, nil
}

func (s *vmDaemonServer) SandboxExists(ctx context.Context, req *vmrpc.SandboxExistsRequest) (*vmrpc.SandboxExistsResponse, error) {
	log.Printf("[gRPC] SandboxExists called: sandboxID=%s", req.SandboxId)
	if err := validateID(req.SandboxId); err != nil {
		return nil, err
	}
	exists := s.sandboxMgr.Exists(req.SandboxId)
	log.Printf("[gRPC] SandboxExists: %s exists=%v", req.SandboxId, exists)
	return &vmrpc.SandboxExistsResponse{Exists: exists}, nil
}

func (s *vmDaemonServer) MountSandbox(ctx context.Context, req *vmrpc.MountSandboxRequest) (*vmrpc.MountSandboxResponse, error) {
	log.Printf("[gRPC] MountSandbox called: sandboxID=%s", req.SandboxId)
	if err := validateID(req.SandboxId); err != nil {
		return nil, err
	}
	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return nil, status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}
	if err := sandbox.Mount(); err != nil {
		log.Printf("[gRPC] MountSandbox failed: %v", err)
		return nil, status.Errorf(codes.Internal, "failed to mount sandbox: %v", err)
	}
	log.Printf("[gRPC] MountSandbox succeeded")
	return &vmrpc.MountSandboxResponse{}, nil
}

func (s *vmDaemonServer) UnmountSandbox(ctx context.Context, req *vmrpc.UnmountSandboxRequest) (*vmrpc.UnmountSandboxResponse, error) {
	log.Printf("[gRPC] UnmountSandbox called: sandboxID=%s", req.SandboxId)
	if err := validateID(req.SandboxId); err != nil {
		return nil, err
	}
	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return nil, status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}
	if err := sandbox.Unmount(); err != nil {
		log.Printf("[gRPC] UnmountSandbox failed: %v", err)
		return nil, status.Errorf(codes.Internal, "failed to unmount sandbox: %v", err)
	}
	log.Printf("[gRPC] UnmountSandbox succeeded")
	return &vmrpc.UnmountSandboxResponse{}, nil
}

func (s *vmDaemonServer) RunSandboxHousekeeper(ctx context.Context, req *vmrpc.RunSandboxHousekeeperRequest) (*vmrpc.RunSandboxHousekeeperResponse, error) {
	overallStart := time.Now()
	log.Printf("[gRPC] RunSandboxHousekeeper called: sandboxID=%s", req.SandboxId)
	if err := validateID(req.SandboxId); err != nil {
		return nil, err
	}
	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return nil, status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}
	err = sandbox.RunHousekeeper()
	if err != nil {
		log.Printf("[gRPC] RunSandboxHousekeeper failed: %v", err)
		return nil, status.Errorf(codes.Internal, "failed to run housekeeper: %v", err)
	}
	log.Printf("[gRPC] RunSandboxHousekeeper succeeded, took %v", time.Since(overallStart))
	return &vmrpc.RunSandboxHousekeeperResponse{}, nil
}

func (s *vmDaemonServer) Exec(ctx context.Context, req *vmrpc.ExecRequest) (*vmrpc.ExecResponse, error) {
	log.Printf("[gRPC] Exec called: sandboxID=%s, command=%v", req.SandboxId, req.Command)
	if err := validateID(req.SandboxId); err != nil {
		return nil, err
	}
	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return nil, status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}
	stdout, stderr, exitCode, err := sandbox.ExecWithContext(ctx, req.Command, req.WorkingDir, req.Env)
	if err != nil {
		log.Printf("[gRPC] Exec failed: %v", err)
		return nil, status.Errorf(codes.Internal, "failed to exec: %v", err)
	}
	log.Printf("[gRPC] Exec succeeded: exitCode=%d", exitCode)
	return &vmrpc.ExecResponse{
		Stdout:   []byte(stdout),
		Stderr:   []byte(stderr),
		ExitCode: int32(exitCode),
	}, nil
}

func (s *vmDaemonServer) ExecStream(req *vmrpc.ExecRequest, stream vmrpc.VMService_ExecStreamServer) error {
	log.Printf("[gRPC] ExecStream called: sandboxID=%s, command=%v", req.SandboxId, req.Command)
	if err := validateID(req.SandboxId); err != nil {
		return err
	}

	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}

	sender := &execOutputSender{stream: stream}
	err = sandbox.ExecStream(stream.Context(), req.Command, req.WorkingDir, req.Env,
		func(data []byte) {
			if len(data) == 0 {
				return
			}
			_ = sender.send(&vmrpc.ExecOutput{Type: vmrpc.ExecOutput_STDOUT, Data: data})
		},
		func(data []byte) {
			if len(data) == 0 {
				return
			}
			_ = sender.send(&vmrpc.ExecOutput{Type: vmrpc.ExecOutput_STDERR, Data: data})
		},
		func(exitCode int) {
			if sender.Err() != nil {
				return
			}
			_ = sender.send(&vmrpc.ExecOutput{Type: vmrpc.ExecOutput_EXIT, ExitCode: int32(exitCode)})
			log.Printf("[gRPC] ExecStream succeeded: exitCode=%d", exitCode)
		},
	)

	if err != nil {
		log.Printf("[gRPC] ExecStream failed: %v", err)
		return status.Errorf(codes.Internal, "failed to exec: %v", err)
	}
	if err := sender.Err(); err != nil {
		return err
	}
	return nil
}

func (s *vmDaemonServer) GetSandboxFile(ctx context.Context, req *vmrpc.GetSandboxFileRequest) (*vmrpc.GetSandboxFileResponse, error) {
	log.Printf("[gRPC] GetSandboxFile called: sandboxID=%s, path=%s", req.SandboxId, req.Path)
	if req.Path == "" {
		return nil, status.Error(codes.InvalidArgument, "path is required")
	}

	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return nil, status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}

	content, err := sandbox.ReadFile(req.Path)
	if err != nil {
		return nil, status.Errorf(codes.NotFound, "file not found: %v", err)
	}

	log.Printf("[gRPC] GetSandboxFile succeeded: %d bytes", len(content))
	return &vmrpc.GetSandboxFileResponse{Content: content}, nil
}

// NewVMService creates a new VMService implementation.
func NewVMService() (vmrpc.VMServiceServer, error) {
	return newVMDaemonServer()
}

func (s *vmDaemonServer) SetupWorkspaces(ctx context.Context, req *vmrpc.SetupWorkspacesRequest) (*vmrpc.SetupWorkspacesResponse, error) {
	log.Printf("[gRPC] SetupWorkspaces called: %d mounts", len(req.Mounts))

	// Convert proto mounts to ops mounts
	mounts := make([]ops.MountConfig, len(req.Mounts))
	for i, m := range req.Mounts {
		mounts[i] = ops.MountConfig{
			VirtioTag:   m.VirtioTag,
			MountPath:   m.MountPath,
			ReadOnly:    m.ReadOnly,
			Passthrough: m.Passthrough,
		}
		log.Printf("[gRPC] SetupWorkspaces mount[%d]: tag=%s, path=%s, readOnly=%v, passthrough=%v", i, m.VirtioTag, m.MountPath, m.ReadOnly, m.Passthrough)
	}

	result, err := s.workspaceMgr.SetupAll(mounts)
	if err != nil {
		log.Printf("[gRPC] SetupWorkspaces failed: %v", err)
		return nil, status.Errorf(codes.Internal, "failed to setup workspaces: %v", err)
	}

	// Convert results to proto
	results := make([]*vmrpc.WorkspaceResult, len(result.Results))
	for i, r := range result.Results {
		results[i] = &vmrpc.WorkspaceResult{
			MountedPath: r.MountedPath,
		}
	}

	log.Printf("[gRPC] SetupWorkspaces succeeded: %d workspaces mounted", len(results))
	return &vmrpc.SetupWorkspacesResponse{
		Results: results,
	}, nil
}

func (s *vmDaemonServer) SetProxyEnv(ctx context.Context, req *vmrpc.SetProxyEnvRequest) (*vmrpc.SetProxyEnvResponse, error) {
	log.Printf("[gRPC] SetProxyEnv called: http=%s, https=%s, no_proxy=%s", req.HttpProxy, req.HttpsProxy, req.NoProxy)

	if err := s.proxyMgr.SetProxyEnv(req.HttpProxy, req.HttpsProxy, req.NoProxy); err != nil {
		log.Printf("[gRPC] SetProxyEnv failed: %v", err)
		return nil, status.Errorf(codes.Internal, "failed to set proxy env: %v", err)
	}

	log.Printf("[gRPC] SetProxyEnv succeeded")
	return &vmrpc.SetProxyEnvResponse{}, nil
}

func (s *vmDaemonServer) WriteSandboxFile(ctx context.Context, req *vmrpc.WriteSandboxFileRequest) (*vmrpc.WriteSandboxFileResponse, error) {
	if req.Path == "" {
		return nil, status.Error(codes.InvalidArgument, "path is required")
	}

	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return nil, status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}

	if err := sandbox.WriteFile(req.Path, req.Content, req.Append); err != nil {
		return nil, status.Errorf(codes.Internal, "failed to write file: %v", err)
	}

	return &vmrpc.WriteSandboxFileResponse{}, nil
}

func (s *vmDaemonServer) UploadSandboxFile(stream vmrpc.VMService_UploadSandboxFileServer) error {
	first, err := stream.Recv()
	if err != nil {
		if err == io.EOF {
			return status.Error(codes.InvalidArgument, "upload stream is missing initial info frame")
		}
		return status.Errorf(codes.Internal, "failed to receive upload metadata: %v", err)
	}

	info := first.GetInfo()
	if info == nil {
		return status.Error(codes.InvalidArgument, "upload stream must start with an info frame")
	}
	if info.Path == "" {
		return status.Error(codes.InvalidArgument, "path is required")
	}

	sandbox, err := s.getOrRestoreSandbox(info.SandboxId)
	if err != nil {
		return status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}
	if !sandbox.IsMounted() {
		return status.Error(codes.FailedPrecondition, "workspace must be mounted to upload files")
	}

	hostPath, err := sandbox.ResolveHostPath(info.Path)
	if err != nil {
		return status.Errorf(codes.Internal, "failed to resolve upload destination: %v", err)
	}

	writer, err := filestream.OpenLocalFileWriteStream(hostPath, filestream.FileWriteOptions{
		Overwrite:     info.Overwrite,
		Mode:          info.Mode,
		MimeType:      info.MimeType,
		TotalSizeHint: info.TotalSizeHint,
	})
	if err != nil {
		return status.Errorf(codes.Internal, "failed to open upload destination: %v", err)
	}

	committed := false
	defer func() {
		if !committed {
			_ = writer.Abort()
		}
	}()

	for {
		chunk, err := stream.Recv()
		if err != nil {
			if err == io.EOF {
				return status.Error(codes.InvalidArgument, "upload stream ended without completion metadata")
			}
			return status.Errorf(codes.Internal, "failed to receive upload chunk: %v", err)
		}

		switch payload := chunk.GetPayload().(type) {
		case *vmrpc.UploadSandboxFileChunk_Info:
			return status.Error(codes.InvalidArgument, "upload stream may only contain one initial info frame")
		case *vmrpc.UploadSandboxFileChunk_Data:
			data := payload.Data
			written := 0
			for written < len(data) {
				n, err := writer.Write(data[written:])
				if err != nil {
					return status.Errorf(codes.Internal, "failed to write upload chunk: %v", err)
				}
				written += n
			}
		case *vmrpc.UploadSandboxFileChunk_Done:
			result, err := writer.Commit(filestream.FileStreamDone{
				BytesSent: payload.Done.GetBytesSent(),
				SHA256:    payload.Done.GetSha256(),
			})
			if err != nil {
				return status.Errorf(codes.InvalidArgument, "failed to finalize upload: %v", err)
			}
			committed = true
			return stream.SendAndClose(&vmrpc.UploadSandboxFileResponse{
				BytesWritten: result.BytesWritten,
				Sha256:       result.SHA256,
				Created:      result.Created,
				Overwritten:  result.Overwritten,
			})
		default:
			return status.Error(codes.InvalidArgument, "upload chunk payload is required")
		}
	}
}

func (s *vmDaemonServer) StreamSandboxFile(req *vmrpc.StreamSandboxFileRequest, stream vmrpc.VMService_StreamSandboxFileServer) error {
	if req.Path == "" {
		return status.Error(codes.InvalidArgument, "path is required")
	}

	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}

	file, info, err := sandbox.OpenFileForRead(req.Path)
	if err != nil {
		if os.IsNotExist(err) {
			return status.Errorf(codes.NotFound, "file not found: %s", req.Path)
		}
		return status.Errorf(codes.Internal, "failed to open file: %v", err)
	}
	defer file.Close()

	if info.IsDir() {
		return status.Errorf(codes.InvalidArgument, "source path is a directory: %s", req.Path)
	}

	if err := stream.Send(&vmrpc.StreamSandboxFileChunk{
		Payload: &vmrpc.StreamSandboxFileChunk_Info{
			Info: &vmrpc.FileStreamInfo{
				FileName:  filepath.Base(req.Path),
				MimeType:  mime.TypeByExtension(filepath.Ext(req.Path)),
				TotalSize: info.Size(),
				Mode:      uint32(info.Mode().Perm()),
			},
		},
	}); err != nil {
		return status.Errorf(codes.Internal, "failed to send file metadata: %v", err)
	}

	const chunkSize = 256 * 1024
	buffer := make([]byte, chunkSize)
	hasher := sha256.New()
	var bytesSent int64

	for {
		n, readErr := file.Read(buffer)
		if n > 0 {
			chunk := make([]byte, n)
			copy(chunk, buffer[:n])
			if _, err := hasher.Write(chunk); err != nil {
				return status.Errorf(codes.Internal, "failed to hash file chunk: %v", err)
			}
			bytesSent += int64(n)
			if err := stream.Send(&vmrpc.StreamSandboxFileChunk{
				Payload: &vmrpc.StreamSandboxFileChunk_Data{
					Data: chunk,
				},
			}); err != nil {
				return status.Errorf(codes.Internal, "failed to stream file chunk: %v", err)
			}
		}
		if readErr == io.EOF {
			break
		}
		if readErr != nil {
			return status.Errorf(codes.Internal, "failed to read file: %v", readErr)
		}
	}

	if err := stream.Send(&vmrpc.StreamSandboxFileChunk{
		Payload: &vmrpc.StreamSandboxFileChunk_Done{
			Done: &vmrpc.FileStreamDone{
				BytesSent: bytesSent,
				Sha256:    hex.EncodeToString(hasher.Sum(nil)),
			},
		},
	}); err != nil {
		return status.Errorf(codes.Internal, "failed to send file completion: %v", err)
	}
	return nil
}

func (s *vmDaemonServer) DeleteSandboxFile(ctx context.Context, req *vmrpc.DeleteSandboxFileRequest) (*vmrpc.DeleteSandboxFileResponse, error) {
	if req.Path == "" {
		return nil, status.Error(codes.InvalidArgument, "path is required")
	}

	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return nil, status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}

	if err := sandbox.DeleteFile(req.Path); err != nil {
		if os.IsNotExist(err) {
			return nil, status.Errorf(codes.NotFound, "file not found: %s", req.Path)
		}
		return nil, status.Errorf(codes.Internal, "failed to delete file: %v", err)
	}

	return &vmrpc.DeleteSandboxFileResponse{}, nil
}

func (s *vmDaemonServer) SandboxFileExists(ctx context.Context, req *vmrpc.SandboxFileExistsRequest) (*vmrpc.SandboxFileExistsResponse, error) {
	if req.Path == "" {
		return nil, status.Error(codes.InvalidArgument, "path is required")
	}

	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return nil, status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}

	return &vmrpc.SandboxFileExistsResponse{Exists: sandbox.FileExists(req.Path)}, nil
}

func (s *vmDaemonServer) GetSandboxState(ctx context.Context, req *vmrpc.GetSandboxStateRequest) (*vmrpc.GetSandboxStateResponse, error) {
	log.Printf("[gRPC] GetSandboxState called: sandboxID=%s", req.SandboxId)
	if err := validateID(req.SandboxId); err != nil {
		return nil, err
	}

	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return nil, status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}

	diffs, err := sandbox.GetFileDiff()
	if err != nil {
		log.Printf("[gRPC] GetSandboxState failed: %v", err)
		return nil, status.Errorf(codes.Internal, "failed to get sandbox state: %v", err)
	}

	// Convert overlay.FileDiff to vmrpc.FileDiff
	changes := make([]*vmrpc.FileDiff, len(diffs))
	for i, diff := range diffs {
		changes[i] = &vmrpc.FileDiff{
			Path:      diff.Path,
			Mode:      diff.Mode,
			IsDir:     diff.IsDir,
			IsUpdated: diff.IsUpdated,
			IsDeleted: diff.IsDeleted,
			MovedFrom: diff.MovedFrom,
			Timestamp: diff.Timestamp.Unix(),
			Size:      diff.Size,
		}
	}

	// Count each type of change
	var movedCount, deletedCount, updatedCount int
	for _, diff := range diffs {
		if diff.MovedFrom != "" {
			movedCount++
		}
		if diff.IsDeleted {
			deletedCount++
		}
		if diff.IsUpdated {
			updatedCount++
		}
	}
	log.Printf("[gRPC] GetSandboxState succeeded: %d changes (updated=%d, deleted=%d, moved=%d)", len(changes), updatedCount, deletedCount, movedCount)
	return &vmrpc.GetSandboxStateResponse{
		SandboxId: req.SandboxId,
		Changes:   changes,
	}, nil
}

func (s *vmDaemonServer) ExportSandboxDiff(req *vmrpc.ExportSandboxDiffRequest, stream vmrpc.VMService_ExportSandboxDiffServer) error {
	overallStart := time.Now()
	log.Printf("[gRPC] ExportSandboxDiff called: sandboxID=%s", req.SandboxId)
	if err := validateID(req.SandboxId); err != nil {
		return err
	}

	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}

	var streamErr error
	var totalTarBytes int64

	err = sandbox.ExportDiff(req.Paths,
		// onMetadata callback
		func(result *ops.ExportDiffResult) {
			if streamErr != nil {
				return
			}

			// Convert overlay.FileDiff to vmrpc.FileDiff
			changes := make([]*vmrpc.FileDiff, len(result.Changes))
			for i, diff := range result.Changes {
				changes[i] = &vmrpc.FileDiff{
					Path:      diff.Path,
					Mode:      diff.Mode,
					IsDir:     diff.IsDir,
					IsUpdated: diff.IsUpdated,
					IsDeleted: diff.IsDeleted,
					MovedFrom: diff.MovedFrom,
					Timestamp: diff.Timestamp.Unix(),
					Size:      diff.Size,
				}
			}

			// Send metadata as first message
			resp := &vmrpc.ExportSandboxDiffResponse{
				Payload: &vmrpc.ExportSandboxDiffResponse_Metadata{
					Metadata: &vmrpc.DiffMetadata{
						Changes: changes,
						Paths:   result.Paths,
					},
				},
			}
			if err := stream.Send(resp); err != nil {
				streamErr = err
			}
		},
		// onTarData callback
		func(data []byte) {
			if streamErr != nil {
				return
			}
			totalTarBytes += int64(len(data))
			resp := &vmrpc.ExportSandboxDiffResponse{
				Payload: &vmrpc.ExportSandboxDiffResponse_Data{
					Data: &vmrpc.DataChunk{Data: data},
				},
			}
			if err := stream.Send(resp); err != nil {
				streamErr = err
			}
		},
	)

	if err != nil {
		log.Printf("[gRPC] ExportSandboxDiff failed: %v", err)
		return status.Errorf(codes.Internal, "export sandbox diff failed: %v", err)
	}
	if streamErr != nil {
		return streamErr
	}

	log.Printf("[gRPC] ExportSandboxDiff succeeded: %d bytes tar data, took %v", totalTarBytes, time.Since(overallStart))
	return nil
}

func (s *vmDaemonServer) DiscardSandboxAllChanges(ctx context.Context, req *vmrpc.DiscardSandboxAllChangesRequest) (*vmrpc.DiscardSandboxAllChangesResponse, error) {
	log.Printf("[gRPC] DiscardSandboxAllChanges called: sandboxID=%s", req.SandboxId)
	if err := validateID(req.SandboxId); err != nil {
		return nil, err
	}

	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return nil, status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}

	if err := sandbox.DiscardAllChanges(); err != nil {
		log.Printf("[gRPC] DiscardSandboxAllChanges failed: %v", err)
		return nil, status.Errorf(codes.Internal, "discard sandbox changes failed: %v", err)
	}

	log.Printf("[gRPC] DiscardSandboxAllChanges succeeded")
	return &vmrpc.DiscardSandboxAllChangesResponse{}, nil
}

func (s *vmDaemonServer) ExecutePython(ctx context.Context, req *vmrpc.ExecutePythonRequest) (*vmrpc.ExecutePythonResponse, error) {
	log.Printf("[gRPC] ExecutePython called: sandboxID=%s, workingDir=%s", req.SandboxId, req.WorkingDir)

	if err := validateID(req.SandboxId); err != nil {
		return nil, err
	}

	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return nil, status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}

	stdout, stderr, exitCode, err := sandbox.ExecutePython(ctx, req.Code, req.WorkingDir, req.Env)
	if err != nil {
		log.Printf("[gRPC] ExecutePython failed: %v", err)
		return nil, status.Errorf(codes.Internal, "execute python failed: %v", err)
	}

	return &vmrpc.ExecutePythonResponse{
		Stdout:   stdout,
		Stderr:   stderr,
		ExitCode: int32(exitCode),
	}, nil
}

func (s *vmDaemonServer) ExecutePythonStream(req *vmrpc.ExecutePythonRequest, stream vmrpc.VMService_ExecutePythonStreamServer) error {
	log.Printf("[gRPC] ExecutePythonStream called: sandboxID=%s, workingDir=%s", req.SandboxId, req.WorkingDir)

	if err := validateID(req.SandboxId); err != nil {
		return err
	}

	sandbox, err := s.getOrRestoreSandbox(req.SandboxId)
	if err != nil {
		return status.Errorf(codes.NotFound, "sandbox not found: %v", err)
	}

	sender := &execOutputSender{stream: stream}
	err = sandbox.ExecutePythonStream(
		stream.Context(),
		req.Code,
		req.WorkingDir,
		req.Env,
		func(data []byte) {
			if len(data) == 0 {
				return
			}
			_ = sender.send(&vmrpc.ExecOutput{Type: vmrpc.ExecOutput_STDOUT, Data: data})
		},
		func(data []byte) {
			if len(data) == 0 {
				return
			}
			_ = sender.send(&vmrpc.ExecOutput{Type: vmrpc.ExecOutput_STDERR, Data: data})
		},
		func(exitCode int) {
			if sender.Err() != nil {
				return
			}
			_ = sender.send(&vmrpc.ExecOutput{Type: vmrpc.ExecOutput_EXIT, ExitCode: int32(exitCode)})
		},
	)
	if err != nil {
		log.Printf("[gRPC] ExecutePythonStream failed: %v", err)
		return status.Errorf(codes.Internal, "execute python stream failed: %v", err)
	}
	if err := sender.Err(); err != nil {
		return err
	}
	return nil
}

// SyncWorkspace receives a gzip-compressed tar stream and extracts it to the target path.
func (s *vmDaemonServer) SyncWorkspace(stream grpc.ClientStreamingServer[vmrpc.SyncWorkspaceChunk, vmrpc.SyncWorkspaceResponse]) error {
	start := time.Now()
	log.Printf("[gRPC] SyncWorkspace called")

	firstChunk, err := stream.Recv()
	if err == io.EOF {
		return stream.SendAndClose(&vmrpc.SyncWorkspaceResponse{})
	}
	if err != nil {
		return err
	}

	targetPath := firstChunk.GetTargetPath()
	if targetPath == "" {
		return stream.SendAndClose(&vmrpc.SyncWorkspaceResponse{Error: "target path is required"})
	}
	if err := resetSyncTarget(targetPath); err != nil {
		return stream.SendAndClose(&vmrpc.SyncWorkspaceResponse{Error: err.Error()})
	}

	pr, pw := io.Pipe()

	// Receive chunks in a goroutine, writing to the pipe.
	errCh := make(chan error, 1)
	go func() {
		defer pw.Close()
		if len(firstChunk.GetData()) > 0 {
			if _, err := pw.Write(firstChunk.GetData()); err != nil {
				errCh <- err
				return
			}
		}
		for {
			chunk, err := stream.Recv()
			if err == io.EOF {
				return
			}
			if err != nil {
				errCh <- err
				return
			}
			if len(chunk.GetData()) > 0 {
				if _, err := pw.Write(chunk.GetData()); err != nil {
					errCh <- err
					return
				}
			}
		}
	}()

	// Extract the tar stream.
	filesExtracted, extractErr := extractSyncTar(pr, &targetPath)

	// Check for receive errors.
	select {
	case err := <-errCh:
		if extractErr == nil {
			extractErr = err
		}
	default:
	}

	resp := &vmrpc.SyncWorkspaceResponse{FilesExtracted: filesExtracted}
	if extractErr != nil {
		resp.Error = extractErr.Error()
		log.Printf("[gRPC] SyncWorkspace failed: %v", extractErr)
	} else {
		log.Printf("[gRPC] SyncWorkspace succeeded: %d files to %s, took %v", filesExtracted, targetPath, time.Since(start))
	}

	return stream.SendAndClose(resp)
}

func resetSyncTarget(targetPath string) error {
	if targetPath == "" {
		return fmt.Errorf("target path is required")
	}
	if err := os.RemoveAll(targetPath); err != nil {
		return fmt.Errorf("clear target %s: %w", targetPath, err)
	}
	if err := os.MkdirAll(targetPath, 0o755); err != nil {
		return fmt.Errorf("create target %s: %w", targetPath, err)
	}
	return nil
}

// extractSyncTar reads a gzip-compressed tar from r and extracts it under targetPath.
func extractSyncTar(r io.Reader, targetPath *string) (int64, error) {
	gz, err := gzip.NewReader(r)
	if err != nil {
		// If gzip fails, try as plain tar.
		return extractPlainTar(io.MultiReader(strings.NewReader(""), r), targetPath)
	}
	defer gz.Close()
	return extractPlainTar(gz, targetPath)
}

func extractPlainTar(r io.Reader, targetPath *string) (int64, error) {
	tr := tar.NewReader(r)
	var count int64

	for {
		header, err := tr.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			return count, fmt.Errorf("tar read: %w", err)
		}

		if *targetPath == "" {
			return count, fmt.Errorf("target path not set")
		}

		// Sanitize path to prevent directory traversal.
		cleanName := filepath.Clean(header.Name)
		if strings.Contains(cleanName, "..") {
			continue
		}
		dest := filepath.Join(*targetPath, cleanName)
		if !strings.HasPrefix(dest, filepath.Clean(*targetPath)+string(os.PathSeparator)) && dest != filepath.Clean(*targetPath) {
			continue
		}

		switch header.Typeflag {
		case tar.TypeDir:
			if err := os.MkdirAll(dest, os.FileMode(header.Mode)); err != nil {
				return count, fmt.Errorf("mkdir %s: %w", dest, err)
			}
		case tar.TypeReg:
			if err := os.MkdirAll(filepath.Dir(dest), 0o755); err != nil {
				return count, fmt.Errorf("mkdir parent %s: %w", dest, err)
			}
			f, err := os.OpenFile(dest, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, os.FileMode(header.Mode))
			if err != nil {
				return count, fmt.Errorf("create %s: %w", dest, err)
			}
			if _, err := io.Copy(f, tr); err != nil {
				f.Close()
				return count, fmt.Errorf("write %s: %w", dest, err)
			}
			f.Close()
			count++
		case tar.TypeSymlink:
			os.Remove(dest)
			if err := os.Symlink(header.Linkname, dest); err != nil {
				return count, fmt.Errorf("symlink %s: %w", dest, err)
			}
			count++
		}
	}
	return count, nil
}
