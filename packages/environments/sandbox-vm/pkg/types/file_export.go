package types

// ExportFileResult describes a completed file export from a workspace to the local filesystem.
type ExportFileResult struct {
	FileName     string `json:"file_name,omitempty"`
	MimeType     string `json:"mime_type,omitempty"`
	BytesWritten int64  `json:"bytes_written,omitempty"`
	SHA256       string `json:"sha256,omitempty"`
}

type CopyFileResult struct {
	SourceEnvironmentID      string `json:"source_environment_id,omitempty"`
	SourcePath               string `json:"source_path,omitempty"`
	DestinationEnvironmentID string `json:"destination_environment_id,omitempty"`
	DestinationPath          string `json:"destination_path,omitempty"`
	FileName                 string `json:"file_name,omitempty"`
	MimeType                 string `json:"mime_type,omitempty"`
	BytesWritten             int64  `json:"bytes_written,omitempty"`
	SHA256                   string `json:"sha256,omitempty"`
	Created                  bool   `json:"created,omitempty"`
	Overwritten              bool   `json:"overwritten,omitempty"`
}
