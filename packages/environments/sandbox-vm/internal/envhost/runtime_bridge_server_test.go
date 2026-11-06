package envhost

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"io"
	"net/http"
	"strings"
	"testing"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
	oteltrace "go.opentelemetry.io/otel/trace"
)

func TestRuntimeBridgeHTTPServerRoutesToolRequests(t *testing.T) {
	server, err := NewRuntimeBridgeHTTPServer(
		NewDirectRuntimeBridge(&stubRuntimeToolHandler{
			result: &RuntimeToolResult{
				Result: json.RawMessage(`{"ok":true}`),
			},
		}, nil),
	)
	if err != nil {
		t.Fatalf("NewRuntimeBridgeHTTPServer returned error: %v", err)
	}
	defer server.Close()

	req, err := http.NewRequestWithContext(context.Background(), http.MethodPost, server.BaseURL()+"/cap/tool/subagent", strings.NewReader(`{"message":"hello"}`))
	if err != nil {
		t.Fatalf("NewRequest returned error: %v", err)
	}
	req.Header.Set("Content-Type", "application/json")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("Do returned error: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("expected status 200, got %d", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("ReadAll returned error: %v", err)
	}
	if !strings.Contains(string(body), `"ok":true`) {
		t.Fatalf("expected tool response body, got %s", string(body))
	}
}

func TestRuntimeBridgeHTTPServerExtractsToolTraceHeaders(t *testing.T) {
	prevPropagator := otel.GetTextMapPropagator()
	otel.SetTextMapPropagator(propagation.TraceContext{})
	defer otel.SetTextMapPropagator(prevPropagator)

	handler := &traceCapturingRuntimeToolHandler{
		result: &RuntimeToolResult{
			Result: json.RawMessage(`{"ok":true}`),
		},
	}
	server, err := NewRuntimeBridgeHTTPServer(NewDirectRuntimeBridge(handler, nil))
	if err != nil {
		t.Fatalf("NewRuntimeBridgeHTTPServer returned error: %v", err)
	}
	defer server.Close()

	req, err := http.NewRequestWithContext(context.Background(), http.MethodPost, server.BaseURL()+"/cap/tool/subagent", strings.NewReader(`{"message":"hello"}`))
	if err != nil {
		t.Fatalf("NewRequest returned error: %v", err)
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("traceparent", "00-4bf92f3577b34da6a3ce929d0e0e4736-1111111111111111-01")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("Do returned error: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		t.Fatalf("expected status 200, got %d", resp.StatusCode)
	}
	if got := handler.lastSpanContext.TraceID().String(); got != "4bf92f3577b34da6a3ce929d0e0e4736" {
		t.Fatalf("unexpected extracted trace id %q", got)
	}
	if got := handler.lastSpanContext.SpanID().String(); got != "1111111111111111" {
		t.Fatalf("unexpected extracted parent span id %q", got)
	}
}

func TestRuntimeBridgeHTTPServerRoutesAPIRequests(t *testing.T) {
	server, err := NewRuntimeBridgeHTTPServer(NewDirectRuntimeBridge(nil, &stubRuntimeHTTPHandler{
		result: &RuntimeHTTPResult{
			StatusCode:   http.StatusAccepted,
			Headers:      map[string][]string{"Content-Type": {"application/json"}},
			Body:         base64.StdEncoding.EncodeToString([]byte(`{"path":"/v1/ping"}`)),
			BodyEncoding: "base64",
		},
	}))
	if err != nil {
		t.Fatalf("NewRuntimeBridgeHTTPServer returned error: %v", err)
	}
	defer server.Close()

	req, err := http.NewRequestWithContext(context.Background(), http.MethodGet, server.BaseURL()+"/cap/api/v1/ping?x=1", nil)
	if err != nil {
		t.Fatalf("NewRequest returned error: %v", err)
	}

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("Do returned error: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusAccepted {
		t.Fatalf("expected status 202, got %d", resp.StatusCode)
	}
	if got := resp.Header.Get("Content-Type"); got != "application/json" {
		t.Fatalf("expected application/json content type, got %q", got)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatalf("ReadAll returned error: %v", err)
	}
	if string(body) != `{"path":"/v1/ping"}` {
		t.Fatalf("unexpected API response body: %s", string(body))
	}
}

func TestRuntimeBridgeHTTPServerExtractsAPITraceHeaders(t *testing.T) {
	prevPropagator := otel.GetTextMapPropagator()
	otel.SetTextMapPropagator(propagation.TraceContext{})
	defer otel.SetTextMapPropagator(prevPropagator)

	handler := &traceCapturingRuntimeHTTPHandler{
		result: &RuntimeHTTPResult{
			StatusCode:   http.StatusNoContent,
			BodyEncoding: "base64",
		},
	}
	server, err := NewRuntimeBridgeHTTPServer(NewDirectRuntimeBridge(nil, handler))
	if err != nil {
		t.Fatalf("NewRuntimeBridgeHTTPServer returned error: %v", err)
	}
	defer server.Close()

	req, err := http.NewRequestWithContext(context.Background(), http.MethodGet, server.BaseURL()+"/cap/api/v1/ping", nil)
	if err != nil {
		t.Fatalf("NewRequest returned error: %v", err)
	}
	req.Header.Set("traceparent", "00-4bf92f3577b34da6a3ce929d0e0e4736-1111111111111111-01")

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("Do returned error: %v", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusNoContent {
		t.Fatalf("expected status 204, got %d", resp.StatusCode)
	}
	if got := handler.lastSpanContext.TraceID().String(); got != "4bf92f3577b34da6a3ce929d0e0e4736" {
		t.Fatalf("unexpected extracted trace id %q", got)
	}
	if got := handler.lastSpanContext.SpanID().String(); got != "1111111111111111" {
		t.Fatalf("unexpected extracted parent span id %q", got)
	}
}

type traceCapturingRuntimeToolHandler struct {
	result          *RuntimeToolResult
	lastSpanContext oteltrace.SpanContext
}

func (h *traceCapturingRuntimeToolHandler) CallTool(ctx context.Context, req *RuntimeToolRequest) (*RuntimeToolResult, error) {
	_ = req
	h.lastSpanContext = oteltrace.SpanContextFromContext(ctx)
	return h.result, nil
}

type traceCapturingRuntimeHTTPHandler struct {
	result          *RuntimeHTTPResult
	lastSpanContext oteltrace.SpanContext
}

func (h *traceCapturingRuntimeHTTPHandler) DoHTTP(ctx context.Context, req *RuntimeHTTPRequest) (*RuntimeHTTPResult, error) {
	_ = req
	h.lastSpanContext = oteltrace.SpanContextFromContext(ctx)
	return h.result, nil
}
