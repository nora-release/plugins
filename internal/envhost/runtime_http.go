package envhost

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
)

const RuntimeHTTPRequestType = "runtime.http.request"

type RuntimeHTTPRequest struct {
	CapabilityToken string              `json:"capability_token"`
	Method          string              `json:"method"`
	Path            string              `json:"path"`
	Query           string              `json:"query,omitempty"`
	Headers         map[string][]string `json:"headers,omitempty"`
	Body            string              `json:"body,omitempty"`
	BodyEncoding    string              `json:"body_encoding,omitempty"`
}

type RuntimeHTTPResult struct {
	StatusCode   int                 `json:"status_code"`
	Headers      map[string][]string `json:"headers,omitempty"`
	Body         string              `json:"body,omitempty"`
	BodyEncoding string              `json:"body_encoding,omitempty"`
}

type RuntimeHTTPHandler interface {
	DoHTTP(ctx context.Context, req *RuntimeHTTPRequest) (*RuntimeHTTPResult, error)
}

func DispatchRuntimeHTTPRequest(handler RuntimeHTTPHandler, ctx context.Context, msgType string, payload json.RawMessage) (any, error) {
	if handler == nil {
		return nil, NewProtocolError(ErrCodeCapabilityNotSupported, "runtime http handler is unavailable")
	}
	if strings.TrimSpace(msgType) != RuntimeHTTPRequestType {
		return nil, NewProtocolError(ErrCodeInvalidRequest, fmt.Sprintf("unknown runtime http message type: %s", msgType))
	}

	var req RuntimeHTTPRequest
	if err := json.Unmarshal(payload, &req); err != nil {
		return nil, NewProtocolError(ErrCodeInvalidRequest, err.Error())
	}
	return handler.DoHTTP(ctx, &req)
}
