package guesttelemetry

import (
	"crypto/rand"
	"encoding/hex"
	"fmt"
	"strings"
	"time"

	"github.com/nora/sandbox-vm/internal/platform/otlp"
	coltracev1 "go.opentelemetry.io/proto/otlp/collector/trace/v1"
	commonv1 "go.opentelemetry.io/proto/otlp/common/v1"
	resourcev1 "go.opentelemetry.io/proto/otlp/resource/v1"
	tracev1 "go.opentelemetry.io/proto/otlp/trace/v1"
	"google.golang.org/protobuf/proto"
)

func MarshalClientSpan(name string, start time.Time, end time.Time, attributes map[string]string, ok bool, traceparent, tracestate string) ([]byte, error) {
	if name == "" {
		return nil, fmt.Errorf("missing span name")
	}

	traceID, parentSpanID, hasParent := parseTraceParent(traceparent)
	if traceID == nil {
		traceID = make([]byte, 16)
		if _, err := rand.Read(traceID); err != nil {
			return nil, fmt.Errorf("trace id: %w", err)
		}
	}

	spanID := make([]byte, 8)
	if _, err := rand.Read(spanID); err != nil {
		return nil, fmt.Errorf("span id: %w", err)
	}

	statusCode := tracev1.Status_STATUS_CODE_OK
	if !ok {
		statusCode = tracev1.Status_STATUS_CODE_ERROR
	}

	spanAttrs := []*commonv1.KeyValue{
		otlp.KVString("rpc.system", "grpc"),
	}
	for k, v := range attributes {
		spanAttrs = append(spanAttrs, otlp.KVString(k, v))
	}

	span := &tracev1.Span{
		TraceId:           traceID,
		SpanId:            spanID,
		Name:              name,
		Kind:              tracev1.Span_SPAN_KIND_CLIENT,
		StartTimeUnixNano: uint64(start.UnixNano()),
		EndTimeUnixNano:   uint64(end.UnixNano()),
		Attributes:        spanAttrs,
		Status:            &tracev1.Status{Code: statusCode},
	}
	if hasParent {
		span.ParentSpanId = parentSpanID
	}
	if state := strings.TrimSpace(tracestate); state != "" {
		span.TraceState = state
	}

	req := &coltracev1.ExportTraceServiceRequest{
		ResourceSpans: []*tracev1.ResourceSpans{{
			Resource: &resourcev1.Resource{
				Attributes: []*commonv1.KeyValue{
					otlp.KVString("service.name", "guest-daemon"),
					otlp.KVString("os.type", "linux"),
				},
			},
			ScopeSpans: []*tracev1.ScopeSpans{{
				Scope: &commonv1.InstrumentationScope{
					Name:    "nora.guesttelemetry",
					Version: "phase1",
				},
				Spans: []*tracev1.Span{span},
			}},
		}},
	}

	return proto.Marshal(req)
}

func parseTraceParent(traceparent string) (traceID []byte, parentSpanID []byte, ok bool) {
	parent := strings.TrimSpace(traceparent)
	if parent == "" {
		return nil, nil, false
	}

	parts := strings.Split(parent, "-")
	if len(parts) != 4 {
		return nil, nil, false
	}
	if len(parts[1]) != 32 || len(parts[2]) != 16 {
		return nil, nil, false
	}

	traceID, err := hex.DecodeString(parts[1])
	if err != nil || len(traceID) != 16 || isAllZero(traceID) {
		return nil, nil, false
	}

	parentSpanID, err = hex.DecodeString(parts[2])
	if err != nil || len(parentSpanID) != 8 || isAllZero(parentSpanID) {
		return nil, nil, false
	}

	return traceID, parentSpanID, true
}

func isAllZero(b []byte) bool {
	for _, v := range b {
		if v != 0 {
			return false
		}
	}
	return true
}
