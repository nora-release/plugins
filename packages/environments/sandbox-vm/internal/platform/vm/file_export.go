package vm

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"os"
	"path/filepath"

	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
	"github.com/nora/sandbox-vm/pkg/types"
)

type sandboxFileStream interface {
	Recv() (*vmrpc.StreamSandboxFileChunk, error)
}

func exportSandboxFileStream(ctx context.Context, stream sandboxFileStream, dstPath string) (*types.ExportFileResult, error) {
	if err := os.MkdirAll(filepath.Dir(dstPath), 0755); err != nil {
		return nil, fmt.Errorf("create destination directory: %w", err)
	}

	tempPath := dstPath + ".part"
	_ = os.Remove(tempPath)

	file, err := os.OpenFile(tempPath, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, 0600)
	if err != nil {
		return nil, fmt.Errorf("open partial destination: %w", err)
	}

	cleanupTemp := true
	defer func() {
		_ = file.Close()
		if cleanupTemp {
			_ = os.Remove(tempPath)
		}
	}()

	hasher := sha256.New()
	result := &types.ExportFileResult{}
	var bytesWritten int64
	var done *vmrpc.FileStreamDone

	for {
		if ctx.Err() != nil {
			return nil, ctx.Err()
		}

		chunk, err := stream.Recv()
		if err != nil {
			if err == io.EOF {
				break
			}
			return nil, fmt.Errorf("receive file stream: %w", err)
		}

		switch payload := chunk.GetPayload().(type) {
		case *vmrpc.StreamSandboxFileChunk_Info:
			result.FileName = payload.Info.GetFileName()
			result.MimeType = payload.Info.GetMimeType()
		case *vmrpc.StreamSandboxFileChunk_Data:
			data := payload.Data
			if len(data) == 0 {
				continue
			}
			if _, err := hasher.Write(data); err != nil {
				return nil, fmt.Errorf("hash file chunk: %w", err)
			}
			n, err := file.Write(data)
			if err != nil {
				return nil, fmt.Errorf("write destination file: %w", err)
			}
			bytesWritten += int64(n)
		case *vmrpc.StreamSandboxFileChunk_Done:
			done = payload.Done
		}
	}

	if done == nil {
		return nil, fmt.Errorf("file stream ended without completion metadata")
	}
	if done.GetBytesSent() != bytesWritten {
		return nil, fmt.Errorf("file stream size mismatch: sent=%d wrote=%d", done.GetBytesSent(), bytesWritten)
	}

	localSHA := hex.EncodeToString(hasher.Sum(nil))
	if remoteSHA := done.GetSha256(); remoteSHA != "" && remoteSHA != localSHA {
		return nil, fmt.Errorf("file stream checksum mismatch")
	}
	result.SHA256 = localSHA
	result.BytesWritten = bytesWritten

	if err := file.Sync(); err != nil {
		return nil, fmt.Errorf("sync destination file: %w", err)
	}
	if err := file.Close(); err != nil {
		return nil, fmt.Errorf("close destination file: %w", err)
	}
	if err := os.Rename(tempPath, dstPath); err != nil {
		return nil, fmt.Errorf("rename destination file: %w", err)
	}
	cleanupTemp = false
	return result, nil
}
