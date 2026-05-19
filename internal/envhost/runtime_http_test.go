package envhost

import (
	"context"
	"testing"
)

func TestDispatchRuntimeHTTPRequest(t *testing.T) {
	handler := &stubRuntimeHTTPHandler{
		result: &RuntimeHTTPResult{
			StatusCode: 200,
			Headers:    map[string][]string{"X-Test": {"ok"}},
		},
	}

	payload := mustMarshalRuntimePayload(t, RuntimeHTTPRequest{
		CapabilityToken: "cap",
		Method:          "GET",
		Path:            "/v1/search",
		Query:           "q=go",
	})

	result, err := DispatchRuntimeHTTPRequest(handler, context.Background(), RuntimeHTTPRequestType, payload)
	if err != nil {
		t.Fatalf("DispatchRuntimeHTTPRequest returned error: %v", err)
	}

	resp, ok := result.(*RuntimeHTTPResult)
	if !ok {
		t.Fatalf("expected *RuntimeHTTPResult, got %T", result)
	}
	if handler.lastRequest.Path != "/v1/search" {
		t.Fatalf("expected path /v1/search, got %q", handler.lastRequest.Path)
	}
	if resp.StatusCode != 200 {
		t.Fatalf("expected status 200, got %d", resp.StatusCode)
	}
}

type stubRuntimeHTTPHandler struct {
	result      *RuntimeHTTPResult
	lastRequest RuntimeHTTPRequest
}

func (h *stubRuntimeHTTPHandler) DoHTTP(ctx context.Context, req *RuntimeHTTPRequest) (*RuntimeHTTPResult, error) {
	_ = ctx
	if req != nil {
		h.lastRequest = *req
	}
	return h.result, nil
}
