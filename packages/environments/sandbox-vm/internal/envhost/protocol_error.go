package envhost

import "fmt"

// Error codes matching the envhost protocol specification.
const (
	ErrCodeInvalidRequest         = "invalid_request"
	ErrCodeEnvironmentNotFound    = "environment_not_found"
	ErrCodeCapabilityNotSupported = "capability_not_supported"
	ErrCodeEnvironmentNotReady    = "environment_not_ready"
	ErrCodeExecutionFailed        = "execution_failed"
	ErrCodeFileNotFound           = "file_not_found"
	ErrCodePermissionDenied       = "permission_denied"
	ErrCodeUnauthorized           = "unauthorized"
	ErrCodeCapabilityInvalid      = "capability_invalid"
	ErrCodeSessionNotFound        = "session_not_found"
	ErrCodeToolNotSupported       = "tool_not_supported"
	ErrCodeHTTPUpstreamFailed     = "http_upstream_failed"
	ErrCodeTimeout                = "timeout"
	ErrCodeInternalError          = "internal_error"
)

// ProtocolError is an error with a protocol error code.
type ProtocolError struct {
	Code    string
	Message string
}

func (e *ProtocolError) Error() string {
	return fmt.Sprintf("%s: %s", e.Code, e.Message)
}

// NewProtocolError creates a ProtocolError with the given code and message.
func NewProtocolError(code, message string) *ProtocolError {
	return &ProtocolError{Code: code, Message: message}
}
