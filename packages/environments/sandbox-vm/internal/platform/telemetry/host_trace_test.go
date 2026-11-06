package telemetry

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"sync"
	"testing"
	"time"

	"go.opentelemetry.io/otel"
	coltracev1 "go.opentelemetry.io/proto/otlp/collector/trace/v1"
	"google.golang.org/protobuf/proto"
)

func TestStartHostTracingExportsSpans(t *testing.T) {
	t.Cleanup(func() {
		_ = StopHostTracing(context.Background())
	})

	var (
		mu      sync.Mutex
		payload [][]byte
	)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/v1/traces" {
			t.Fatalf("unexpected path %q", r.URL.Path)
		}
		body, err := io.ReadAll(r.Body)
		if err != nil {
			t.Fatalf("read body: %v", err)
		}
		mu.Lock()
		payload = append(payload, body)
		mu.Unlock()
		w.WriteHeader(http.StatusOK)
	}))
	defer server.Close()

	if err := StartHostTracing(server.URL); err != nil {
		t.Fatalf("StartHostTracing: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	ctx, span := otel.Tracer("nora/test").Start(ctx, "host.trace.test")
	span.End()

	if err := StopHostTracing(ctx); err != nil {
		t.Fatalf("StopHostTracing: %v", err)
	}

	mu.Lock()
	defer mu.Unlock()
	if len(payload) == 0 {
		t.Fatal("expected trace export payload")
	}

	var req coltracev1.ExportTraceServiceRequest
	if err := proto.Unmarshal(payload[0], &req); err != nil {
		t.Fatalf("unmarshal trace payload: %v", err)
	}
	if len(req.ResourceSpans) == 0 {
		t.Fatal("expected resource spans")
	}
}
