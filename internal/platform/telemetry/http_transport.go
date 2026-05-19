package telemetry

import (
	"context"
	"fmt"
	"net/http"
	"strings"

	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/codes"
	"go.opentelemetry.io/otel/propagation"
	oteltrace "go.opentelemetry.io/otel/trace"
)

const defaultHTTPScope = "nora/httpclient"

type RoundTripper struct {
	base       http.RoundTripper
	propagator propagation.TextMapPropagator
	tracer     oteltrace.Tracer
}

func NewRoundTripper(scope string, base http.RoundTripper) http.RoundTripper {
	scope = strings.TrimSpace(scope)
	if scope == "" {
		scope = defaultHTTPScope
	}
	if base == nil {
		base = http.DefaultTransport
	}

	propagator := otel.GetTextMapPropagator()
	if propagator == nil {
		propagator = propagation.TraceContext{}
	}

	return &RoundTripper{
		base:       base,
		propagator: propagator,
		tracer:     otel.Tracer(scope),
	}
}

func NewHTTPClient(scope string, base http.RoundTripper) *http.Client {
	return &http.Client{Transport: NewRoundTripper(scope, base)}
}

func ContextWithPropagation(ctx context.Context) context.Context {
	if ctx == nil {
		ctx = context.Background()
	}
	traceContext := TraceContextFromContext(ctx)
	if traceContext.IsZero() {
		return ctx
	}
	propagator := otel.GetTextMapPropagator()
	if propagator == nil {
		propagator = propagation.TraceContext{}
	}
	return propagator.Extract(ctx, propagation.MapCarrier(traceContext.headerCarrier()))
}

func (rt *RoundTripper) RoundTrip(req *http.Request) (*http.Response, error) {
	if req == nil {
		return nil, fmt.Errorf("http request is required")
	}

	ctx := req.Context()
	if ctx == nil {
		ctx = context.Background()
	}

	traceContext := TraceContextFromContext(ctx)
	ctx = ContextWithPropagation(ctx)

	spanName := strings.ToUpper(strings.TrimSpace(req.Method))
	if spanName == "" {
		spanName = http.MethodGet
	}
	if req.URL != nil && req.URL.Host != "" {
		spanName += " " + req.URL.Host
	}

	attrs := []attribute.KeyValue{
		attribute.String("http.method", req.Method),
	}
	if req.URL != nil {
		attrs = append(attrs,
			attribute.String("url.scheme", req.URL.Scheme),
			attribute.String("server.address", req.URL.Host),
		)
	}

	ctx, span := rt.tracer.Start(ctx, spanName,
		oteltrace.WithSpanKind(oteltrace.SpanKindClient),
		oteltrace.WithAttributes(attrs...),
	)
	defer span.End()

	cloned := req.Clone(ctx)
	if cloned.Header == nil {
		cloned.Header = make(http.Header)
	}
	rt.propagator.Inject(ctx, propagation.HeaderCarrier(cloned.Header))
	if cloned.Header.Get("traceparent") == "" && traceContext.Traceparent != "" {
		cloned.Header.Set("traceparent", traceContext.Traceparent)
	}
	if cloned.Header.Get("tracestate") == "" && traceContext.Tracestate != "" {
		cloned.Header.Set("tracestate", traceContext.Tracestate)
	}

	resp, err := rt.base.RoundTrip(cloned)
	if err != nil {
		span.RecordError(err)
		span.SetStatus(codes.Error, err.Error())
		return nil, err
	}

	span.SetAttributes(attribute.Int("http.status_code", resp.StatusCode))
	if resp.StatusCode >= http.StatusInternalServerError {
		span.SetStatus(codes.Error, http.StatusText(resp.StatusCode))
	} else {
		span.SetStatus(codes.Ok, "")
	}
	return resp, nil
}

func (tc TraceContext) headerCarrier() map[string]string {
	headers := make(map[string]string, 2)
	if tc.Traceparent != "" {
		headers["traceparent"] = tc.Traceparent
	}
	if tc.Tracestate != "" {
		headers["tracestate"] = tc.Tracestate
	}
	return headers
}
