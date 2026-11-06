package vm

import (
	"fmt"
	"io"

	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
	"github.com/nora/sandbox-vm/pkg/filestream"
)

type grpcSandboxFileReadStream interface {
	Recv() (*vmrpc.StreamSandboxFileChunk, error)
}

type vmReadStream struct {
	stream grpcSandboxFileReadStream
	info   filestream.FileStreamInfo
	buf    []byte
	done   bool
}

func newVMReadStream(stream grpcSandboxFileReadStream) (*vmReadStream, error) {
	first, err := stream.Recv()
	if err != nil {
		return nil, fmt.Errorf("receive file metadata: %w", err)
	}
	info := first.GetInfo()
	if info == nil {
		return nil, fmt.Errorf("file stream did not start with metadata")
	}
	return &vmReadStream{
		stream: stream,
		info: filestream.FileStreamInfo{
			FileName:  info.GetFileName(),
			MimeType:  info.GetMimeType(),
			TotalSize: info.GetTotalSize(),
			Mode:      info.GetMode(),
		},
	}, nil
}

func (s *vmReadStream) Info() filestream.FileStreamInfo {
	return s.info
}

func (s *vmReadStream) Read(p []byte) (int, error) {
	if len(s.buf) > 0 {
		n := copy(p, s.buf)
		s.buf = s.buf[n:]
		return n, nil
	}
	if s.done {
		return 0, io.EOF
	}

	for {
		chunk, err := s.stream.Recv()
		if err != nil {
			return 0, err
		}
		switch payload := chunk.GetPayload().(type) {
		case *vmrpc.StreamSandboxFileChunk_Data:
			if len(payload.Data) == 0 {
				continue
			}
			n := copy(p, payload.Data)
			if n < len(payload.Data) {
				s.buf = append(s.buf[:0], payload.Data[n:]...)
			}
			return n, nil
		case *vmrpc.StreamSandboxFileChunk_Done:
			s.done = true
			return 0, io.EOF
		}
	}
}

func (s *vmReadStream) Close() error {
	s.done = true
	s.buf = nil
	return nil
}

type grpcSandboxFileUploadClient interface {
	Send(*vmrpc.UploadSandboxFileChunk) error
	CloseAndRecv() (*vmrpc.UploadSandboxFileResponse, error)
}

type vmWriteStream struct {
	stream grpcSandboxFileUploadClient
	closed bool
}

func newVMWriteStream(stream grpcSandboxFileUploadClient, sandboxID, path string, opts filestream.FileWriteOptions) (*vmWriteStream, error) {
	if err := stream.Send(&vmrpc.UploadSandboxFileChunk{
		Payload: &vmrpc.UploadSandboxFileChunk_Info{
			Info: &vmrpc.UploadSandboxFileInfo{
				SandboxId:     sandboxID,
				Path:          path,
				Overwrite:     opts.Overwrite,
				Mode:          opts.Mode,
				MimeType:      opts.MimeType,
				TotalSizeHint: opts.TotalSizeHint,
			},
		},
	}); err != nil {
		return nil, fmt.Errorf("send upload metadata: %w", err)
	}
	return &vmWriteStream{stream: stream}, nil
}

func (s *vmWriteStream) Write(p []byte) (int, error) {
	if s.closed {
		return 0, fmt.Errorf("write stream is closed")
	}
	if len(p) == 0 {
		return 0, nil
	}
	chunk := make([]byte, len(p))
	copy(chunk, p)
	if err := s.stream.Send(&vmrpc.UploadSandboxFileChunk{
		Payload: &vmrpc.UploadSandboxFileChunk_Data{Data: chunk},
	}); err != nil {
		return 0, err
	}
	return len(p), nil
}

func (s *vmWriteStream) Commit(done filestream.FileStreamDone) (filestream.FileWriteResult, error) {
	if s.closed {
		return filestream.FileWriteResult{}, fmt.Errorf("write stream is closed")
	}
	s.closed = true
	if err := s.stream.Send(&vmrpc.UploadSandboxFileChunk{
		Payload: &vmrpc.UploadSandboxFileChunk_Done{
			Done: &vmrpc.FileStreamDone{
				BytesSent: done.BytesSent,
				Sha256:    done.SHA256,
			},
		},
	}); err != nil {
		return filestream.FileWriteResult{}, err
	}
	resp, err := s.stream.CloseAndRecv()
	if err != nil {
		return filestream.FileWriteResult{}, err
	}
	return filestream.FileWriteResult{
		BytesWritten: resp.GetBytesWritten(),
		SHA256:       resp.GetSha256(),
		Created:      resp.GetCreated(),
		Overwritten:  resp.GetOverwritten(),
	}, nil
}

func (s *vmWriteStream) Abort() error {
	if s.closed {
		return nil
	}
	s.closed = true
	// Close the gRPC client stream so the server stops waiting on Recv.
	// The server will see an EOF or cancellation and can clean up.
	_, err := s.stream.CloseAndRecv()
	return err
}
