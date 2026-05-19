package telemetry

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/propagation"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	tracetest "go.opentelemetry.io/otel/sdk/trace/tracetest"
)

func TestRoundTripperInjectsTraceparentFromTraceContext(t *testing.T) {
	recorder := tracetest.NewSpanRecorder()
	provider := sdktrace.NewTracerProvider(sdktrace.WithSpanProcessor(recorder))
	prevProvider := otel.GetTracerProvider()
	prevPropagator := otel.GetTextMapPropagator()
	otel.SetTracerProvider(provider)
	otel.SetTextMapPropagator(propagation.TraceContext{})
	defer func() {
		otel.SetTracerProvider(prevProvider)
		otel.SetTextMapPropagator(prevPropagator)
		_ = provider.Shutdown(context.Background())
	}()

	headersCh := make(chan http.Header, 1)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		headersCh <- r.Header.Clone()
		w.WriteHeader(http.StatusAccepted)
	}))
	defer server.Close()

	req, err := http.NewRequestWithContext(
		WithTraceContext(context.Background(), TraceContextFromStrings(
			"00-4bf92f3577b34da6a3ce929d0e0e4736-1111111111111111-01",
			"vendor=value",
		)),
		http.MethodPost,
		server.URL+"/responses",
		nil,
	)
	if err != nil {
		t.Fatalf("NewRequestWithContext returned error: %v", err)
	}

	client := NewHTTPClient("nora/http-test", nil)
	resp, err := client.Do(req)
	if err != nil {
		t.Fatalf("Do returned error: %v", err)
	}
	_ = resp.Body.Close()

	headers := <-headersCh
	traceparent := headers.Get("traceparent")
	if traceparent == "" {
		t.Fatal("expected traceparent header")
	}
	if got := headers.Get("tracestate"); got != "vendor=value" {
		t.Fatalf("expected tracestate to round-trip, got %q", got)
	}
	if !strings.HasPrefix(traceparent, "00-4bf92f3577b34da6a3ce929d0e0e4736-") {
		t.Fatalf("expected trace id to be preserved, got %q", traceparent)
	}

	spans := recorder.Ended()
	if len(spans) != 1 {
		t.Fatalf("expected 1 client span, got %d", len(spans))
	}
	if got := spans[0].Name(); got != "POST "+req.URL.Host {
		t.Fatalf("unexpected span name %q", got)
	}
	if got := spans[0].Parent().TraceID().String(); got != "4bf92f3577b34da6a3ce929d0e0e4736" {
		t.Fatalf("expected parent trace id to match propagated trace, got %s", got)
	}
}
