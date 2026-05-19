package telemetry

import (
	"context"
	"time"

	coltracev1 "go.opentelemetry.io/proto/otlp/collector/trace/v1"
	"google.golang.org/protobuf/proto"
)

func (c *Collector) ForwardTraces(ctx context.Context, req *coltracev1.ExportTraceServiceRequest) {
	if c == nil || req == nil {
		return
	}

	baseURL := c.IngestionBaseURL()
	if baseURL == "" {
		return
	}

	payload, err := proto.Marshal(req)
	if err != nil {
		return
	}

	limit := c.maxRequestBytes.Load()
	if limit > 0 && int64(len(payload)) > limit {
		return
	}

	if ctx == nil {
		ctx = context.Background()
	}
	ctx, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()

	_, _ = c.forwardUpstream(ctx, baseURL, "traces", payload, "application/x-protobuf", "application/x-protobuf", "")
}
