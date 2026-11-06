package envhost

import (
	"os"

	"github.com/nora/sandbox-vm/pkg/filestream"
)

// Re-export file stream types from pkg/filestream so existing consumers
// of the envhost package continue to compile without changes.

type FileStreamInfo = filestream.FileStreamInfo
type FileStreamDone = filestream.FileStreamDone
type FileWriteOptions = filestream.FileWriteOptions
type FileWriteResult = filestream.FileWriteResult
type FileReadStream = filestream.FileReadStream
type FileWriteStream = filestream.FileWriteStream

// OpenLocalFileReadStream opens a local file for streaming reads.
// It wraps the pkg/filestream implementation to translate file-not-found
// errors into envhost protocol errors.
func OpenLocalFileReadStream(path string) (FileReadStream, error) {
	stream, err := filestream.OpenLocalFileReadStream(path)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, NewProtocolError(ErrCodeFileNotFound, err.Error())
		}
		return nil, err
	}
	return stream, nil
}

// OpenLocalFileWriteStream opens a local file for streaming writes
// with atomic rename on commit.
func OpenLocalFileWriteStream(path string, opts FileWriteOptions) (FileWriteStream, error) {
	return filestream.OpenLocalFileWriteStream(path, opts)
}
