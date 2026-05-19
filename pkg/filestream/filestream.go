// Package filestream provides streaming file I/O types and local implementations.
// It is intentionally dependency-free (stdlib only) so that both host-side code
// (envhost, environments) and guest-side code (vmd) can import it.
package filestream

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"mime"
	"os"
	"path/filepath"
)

type FileStreamInfo struct {
	FileName  string
	MimeType  string
	TotalSize int64
	Mode      uint32
}

type FileStreamDone struct {
	BytesSent int64
	SHA256    string
}

type FileWriteOptions struct {
	Overwrite     bool
	Mode          uint32
	MimeType      string
	TotalSizeHint int64
}

type FileWriteResult struct {
	BytesWritten int64
	SHA256       string
	Created      bool
	Overwritten  bool
}

type FileReadStream interface {
	io.ReadCloser
	Info() FileStreamInfo
}

type FileWriteStream interface {
	io.Writer
	Commit(done FileStreamDone) (FileWriteResult, error)
	Abort() error
}

type localFileReadStream struct {
	file *os.File
	info FileStreamInfo
}

func (s *localFileReadStream) Read(p []byte) (int, error) {
	return s.file.Read(p)
}

func (s *localFileReadStream) Close() error {
	return s.file.Close()
}

func (s *localFileReadStream) Info() FileStreamInfo {
	return s.info
}

func OpenLocalFileReadStream(path string) (FileReadStream, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}

	stat, err := file.Stat()
	if err != nil {
		_ = file.Close()
		return nil, err
	}
	if stat.IsDir() {
		_ = file.Close()
		return nil, fmt.Errorf("source path is a directory: %s", path)
	}

	return &localFileReadStream{
		file: file,
		info: FileStreamInfo{
			FileName:  filepath.Base(path),
			MimeType:  mime.TypeByExtension(filepath.Ext(path)),
			TotalSize: stat.Size(),
			Mode:      uint32(stat.Mode().Perm()),
		},
	}, nil
}

type localFileWriteStream struct {
	file        *os.File
	tempPath    string
	finalPath   string
	created     bool
	overwritten bool
	hasher      hashWriter
	bytes       int64
	closed      bool
}

type hashWriter interface {
	Write([]byte) (int, error)
	Sum([]byte) []byte
}

func OpenLocalFileWriteStream(path string, opts FileWriteOptions) (FileWriteStream, error) {
	finalPath := path
	if err := os.MkdirAll(filepath.Dir(finalPath), 0o755); err != nil {
		return nil, fmt.Errorf("create destination directory: %w", err)
	}

	_, statErr := os.Stat(finalPath)
	exists := statErr == nil
	if statErr != nil && !os.IsNotExist(statErr) {
		return nil, fmt.Errorf("stat destination: %w", statErr)
	}
	if exists && !opts.Overwrite {
		return nil, fmt.Errorf("destination file already exists: %s", finalPath)
	}

	mode := os.FileMode(opts.Mode)
	if mode == 0 {
		mode = 0o644
	}

	tempFile, err := os.CreateTemp(filepath.Dir(finalPath), "."+filepath.Base(finalPath)+".*.part")
	if err != nil {
		return nil, fmt.Errorf("create temporary destination: %w", err)
	}
	if err := tempFile.Chmod(mode); err != nil {
		_ = tempFile.Close()
		_ = os.Remove(tempFile.Name())
		return nil, fmt.Errorf("set temporary file mode: %w", err)
	}

	return &localFileWriteStream{
		file:        tempFile,
		tempPath:    tempFile.Name(),
		finalPath:   finalPath,
		created:     !exists,
		overwritten: exists,
		hasher:      sha256.New(),
	}, nil
}

func (s *localFileWriteStream) Write(p []byte) (int, error) {
	if s.closed {
		return 0, fmt.Errorf("write stream is closed")
	}
	if len(p) == 0 {
		return 0, nil
	}
	if _, err := s.hasher.Write(p); err != nil {
		return 0, fmt.Errorf("hash destination chunk: %w", err)
	}
	n, err := s.file.Write(p)
	s.bytes += int64(n)
	return n, err
}

func (s *localFileWriteStream) Commit(done FileStreamDone) (FileWriteResult, error) {
	if s.closed {
		return FileWriteResult{}, fmt.Errorf("write stream is closed")
	}
	s.closed = true

	localSHA := hex.EncodeToString(s.hasher.Sum(nil))
	if done.BytesSent != s.bytes {
		_ = s.file.Close()
		_ = os.Remove(s.tempPath)
		return FileWriteResult{}, fmt.Errorf("file stream size mismatch: sent=%d wrote=%d", done.BytesSent, s.bytes)
	}
	if done.SHA256 != "" && done.SHA256 != localSHA {
		_ = s.file.Close()
		_ = os.Remove(s.tempPath)
		return FileWriteResult{}, fmt.Errorf("file stream checksum mismatch")
	}
	if err := s.file.Sync(); err != nil {
		_ = s.file.Close()
		_ = os.Remove(s.tempPath)
		return FileWriteResult{}, fmt.Errorf("sync temporary destination: %w", err)
	}
	if err := s.file.Close(); err != nil {
		_ = os.Remove(s.tempPath)
		return FileWriteResult{}, fmt.Errorf("close temporary destination: %w", err)
	}
	if err := os.Rename(s.tempPath, s.finalPath); err != nil {
		_ = os.Remove(s.tempPath)
		return FileWriteResult{}, fmt.Errorf("rename temporary destination: %w", err)
	}
	return FileWriteResult{
		BytesWritten: s.bytes,
		SHA256:       localSHA,
		Created:      s.created,
		Overwritten:  s.overwritten,
	}, nil
}

func (s *localFileWriteStream) Abort() error {
	if s.closed {
		return nil
	}
	s.closed = true
	closeErr := s.file.Close()
	removeErr := os.Remove(s.tempPath)
	if closeErr != nil {
		return closeErr
	}
	if removeErr != nil && !os.IsNotExist(removeErr) {
		return removeErr
	}
	return nil
}
