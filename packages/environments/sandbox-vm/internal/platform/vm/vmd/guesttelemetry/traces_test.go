package guesttelemetry

import (
	"testing"
	"time"

	coltracev1 "go.opentelemetry.io/proto/otlp/collector/trace/v1"
	tracev1 "go.opentelemetry.io/proto/otlp/trace/v1"
	"google.golang.org/protobuf/proto"
)

func TestMarshalClientSpanBuildsSpan(t *testing.T) {
	start := time.Unix(10, 0)
	end := start.Add(250 * time.Millisecond)

	payload, err := MarshalClientSpan(
		"vmrpc.host.call_tool",
		start,
		end,
		map[string]string{"tool": "bash"},
		false,
		"",
		"",
	)
	if err != nil {
		t.Fatalf("MarshalClientSpan: %v", err)
	}

	var req coltracev1.ExportTraceServiceRequest
	if err := proto.Unmarshal(payload, &req); err != nil {
		t.Fatalf("Unmarshal: %v", err)
	}

	span := firstSpan(&req)
	if span == nil {
		t.Fatalf("missing span")
	}

	if span.Name != "vmrpc.host.call_tool" {
		t.Fatalf("unexpected name: %q", span.Name)
	}
	if span.Kind != tracev1.Span_SPAN_KIND_CLIENT {
		t.Fatalf("unexpected kind: %v", span.Kind)
	}
	if span.Status == nil || span.Status.Code != tracev1.Status_STATUS_CODE_ERROR {
		t.Fatalf("unexpected status: %+v", span.Status)
	}
	if len(span.TraceId) != 16 {
		t.Fatalf("unexpected trace id length: %d", len(span.TraceId))
	}
	if len(span.SpanId) != 8 {
		t.Fatalf("unexpected span id length: %d", len(span.SpanId))
	}
}

func firstSpan(req *coltracev1.ExportTraceServiceRequest) *tracev1.Span {
	if req == nil || len(req.ResourceSpans) == 0 || req.ResourceSpans[0] == nil {
		return nil
	}
	rs := req.ResourceSpans[0]
	if len(rs.ScopeSpans) == 0 || rs.ScopeSpans[0] == nil {
		return nil
	}
	ss := rs.ScopeSpans[0]
	if len(ss.Spans) == 0 {
		return nil
	}
	return ss.Spans[0]
}
