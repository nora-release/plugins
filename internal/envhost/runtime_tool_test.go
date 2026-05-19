package envhost

import (
	"context"
	"encoding/json"
	"testing"
)

func TestDispatchRuntimeToolRequest(t *testing.T) {
	handler := &stubRuntimeToolHandler{
		result: &RuntimeToolResult{
			Result: json.RawMessage(`{"ok":true}`),
		},
	}

	payload := mustMarshalRuntimePayload(t, RuntimeToolRequest{
		CapabilityToken: "cap",
		Tool:            "subagent",
		Input:           json.RawMessage(`{"message":"hello"}`),
	})

	result, err := DispatchRuntimeToolRequest(handler, context.Background(), RuntimeToolCallType, payload)
	if err != nil {
		t.Fatalf("DispatchRuntimeToolRequest returned error: %v", err)
	}

	resp, ok := result.(*RuntimeToolResult)
	if !ok {
		t.Fatalf("expected *RuntimeToolResult, got %T", result)
	}
	if handler.lastRequest.Tool != "subagent" {
		t.Fatalf("expected tool subagent, got %q", handler.lastRequest.Tool)
	}
	if string(resp.Result) != `{"ok":true}` {
		t.Fatalf("unexpected result payload: %s", string(resp.Result))
	}
}

type stubRuntimeToolHandler struct {
	result      *RuntimeToolResult
	lastRequest RuntimeToolRequest
}

func (h *stubRuntimeToolHandler) CallTool(ctx context.Context, req *RuntimeToolRequest) (*RuntimeToolResult, error) {
	_ = ctx
	if req != nil {
		h.lastRequest = *req
	}
	return h.result, nil
}

func mustMarshalRuntimePayload(t *testing.T, value any) json.RawMessage {
	t.Helper()
	data, err := json.Marshal(value)
	if err != nil {
		t.Fatalf("marshal payload: %v", err)
	}
	return data
}
