package telemetry

import (
	"context"
	"fmt"
	"net/url"
	"strings"
	"sync"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracehttp"
	"go.opentelemetry.io/otel/propagation"
	"go.opentelemetry.io/otel/sdk/resource"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	"go.opentelemetry.io/otel/trace/noop"
)

var (
	hostTraceMu       sync.Mutex
	hostTraceProvider *sdktrace.TracerProvider
	hostTraceEndpoint string
)

func StartHostTracing(collectorEndpoint string) error {
	endpoint := strings.TrimSpace(collectorEndpoint)
	if endpoint == "" {
		return fmt.Errorf("collector endpoint is required")
	}

	parsed, err := url.Parse(endpoint)
	if err != nil {
		return fmt.Errorf("parse collector endpoint: %w", err)
	}
	if parsed.Host == "" {
		return fmt.Errorf("collector endpoint host is required")
	}

	opts := []otlptracehttp.Option{
		otlptracehttp.WithEndpoint(parsed.Host),
		otlptracehttp.WithURLPath(traceExportPath(parsed.Path)),
	}
	switch parsed.Scheme {
	case "http":
		opts = append(opts, otlptracehttp.WithInsecure())
	case "https":
	default:
		return fmt.Errorf("unsupported collector endpoint scheme %q", parsed.Scheme)
	}

	exporter, err := otlptracehttp.New(context.Background(), opts...)
	if err != nil {
		return fmt.Errorf("create trace exporter: %w", err)
	}

	provider := sdktrace.NewTracerProvider(
		sdktrace.WithBatcher(exporter),
		sdktrace.WithResource(resource.NewSchemaless(
			attribute.String("service.name", "local-vm-host"),
			attribute.String("nora.runtime", "local-vm"),
		)),
	)

	hostTraceMu.Lock()
	defer hostTraceMu.Unlock()

	if hostTraceProvider != nil && hostTraceEndpoint == endpoint {
		_ = provider.Shutdown(context.Background())
		return nil
	}
	if hostTraceProvider != nil {
		_ = hostTraceProvider.Shutdown(context.Background())
	}

	otel.SetTextMapPropagator(propagation.TraceContext{})
	otel.SetTracerProvider(provider)
	hostTraceProvider = provider
	hostTraceEndpoint = endpoint
	return nil
}

func StopHostTracing(ctx context.Context) error {
	hostTraceMu.Lock()
	provider := hostTraceProvider
	hostTraceProvider = nil
	hostTraceEndpoint = ""
	hostTraceMu.Unlock()

	otel.SetTracerProvider(noop.NewTracerProvider())
	otel.SetTextMapPropagator(propagation.TraceContext{})

	if provider == nil {
		return nil
	}
	if ctx == nil {
		ctx = context.Background()
	}
	return provider.Shutdown(ctx)
}

func traceExportPath(basePath string) string {
	trimmed := strings.TrimRight(strings.TrimSpace(basePath), "/")
	switch {
	case trimmed == "":
		return "/v1/traces"
	case strings.HasSuffix(trimmed, "/v1/traces"):
		return trimmed
	case strings.HasSuffix(trimmed, "/v1"):
		return trimmed + "/traces"
	default:
		return trimmed + "/v1/traces"
	}
}
