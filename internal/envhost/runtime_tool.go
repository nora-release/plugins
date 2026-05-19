package envhost

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
)

const RuntimeToolCallType = "runtime.tool.call"

type RuntimeToolRequest struct {
	CapabilityToken string              `json:"capability_token"`
	Tool            string              `json:"tool"`
	Input           json.RawMessage     `json:"input,omitempty"`
	Headers         map[string][]string `json:"headers,omitempty"`
}

type RuntimeToolResult struct {
	Result json.RawMessage `json:"result,omitempty"`
}

type RuntimeToolHandler interface {
	CallTool(ctx context.Context, req *RuntimeToolRequest) (*RuntimeToolResult, error)
}

type RuntimeBridge interface {
	CallTool(ctx context.Context, req *RuntimeToolRequest) (*RuntimeToolResult, error)
	DoHTTP(ctx context.Context, req *RuntimeHTTPRequest) (*RuntimeHTTPResult, error)
}

type directRuntimeBridge struct {
	toolHandler RuntimeToolHandler
	httpHandler RuntimeHTTPHandler
}

func NewDirectRuntimeBridge(toolHandler RuntimeToolHandler, httpHandler RuntimeHTTPHandler) RuntimeBridge {
	return &directRuntimeBridge{
		toolHandler: toolHandler,
		httpHandler: httpHandler,
	}
}

func (b *directRuntimeBridge) CallTool(ctx context.Context, req *RuntimeToolRequest) (*RuntimeToolResult, error) {
	if b == nil || b.toolHandler == nil {
		return nil, fmt.Errorf("runtime tool handler is unavailable")
	}
	return b.toolHandler.CallTool(ctx, req)
}

func (b *directRuntimeBridge) DoHTTP(ctx context.Context, req *RuntimeHTTPRequest) (*RuntimeHTTPResult, error) {
	if b == nil || b.httpHandler == nil {
		return nil, fmt.Errorf("runtime http handler is unavailable")
	}
	return b.httpHandler.DoHTTP(ctx, req)
}

func DispatchRuntimeToolRequest(handler RuntimeToolHandler, ctx context.Context, msgType string, payload json.RawMessage) (any, error) {
	if handler == nil {
		return nil, NewProtocolError(ErrCodeToolNotSupported, "runtime tool handler is unavailable")
	}
	if strings.TrimSpace(msgType) != RuntimeToolCallType {
		return nil, NewProtocolError(ErrCodeInvalidRequest, fmt.Sprintf("unknown runtime tool message type: %s", msgType))
	}

	var req RuntimeToolRequest
	if err := json.Unmarshal(payload, &req); err != nil {
		return nil, NewProtocolError(ErrCodeInvalidRequest, err.Error())
	}
	return handler.CallTool(ctx, &req)
}
