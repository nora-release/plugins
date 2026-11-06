package telemetry

import (
	"bytes"
	"compress/gzip"
	"context"
	"encoding/binary"
	"fmt"
	"io"
	"strings"
	"sync"
	"time"

	coltracev1 "go.opentelemetry.io/proto/otlp/collector/trace/v1"
	commonv1 "go.opentelemetry.io/proto/otlp/common/v1"
	resourcev1 "go.opentelemetry.io/proto/otlp/resource/v1"
	tracev1 "go.opentelemetry.io/proto/otlp/trace/v1"
	"google.golang.org/protobuf/proto"
)

const (
	maxInt64  = int64(^uint64(0) >> 1)
	maxUint64 = ^uint64(0)
)

type tailSamplingConfig struct {
	DecisionWait time.Duration
	MaxTraces    int

	SampleRate    float64
	SlowThreshold time.Duration
}

type spanRecord struct {
	resource *resourcev1.Resource
	scope    *commonv1.InstrumentationScope
	span     *tracev1.Span
}

type traceBuffer struct {
	firstSeen   time.Time
	lastSeen    time.Time
	hasError    bool
	maxDuration time.Duration
	spans       []spanRecord
}

type otlpTraceCollector interface {
	ForwardTraces(context.Context, *coltracev1.ExportTraceServiceRequest)
}

type tailSampler struct {
	collector otlpTraceCollector

	mu      sync.Mutex
	cfg     tailSamplingConfig
	traces  map[[16]byte]*traceBuffer
	stopCtx context.Context

	stopOnce sync.Once
	stopCh   chan struct{}
	doneCh   chan struct{}
}

func newTailSampler(collector otlpTraceCollector, cfg tailSamplingConfig) *tailSampler {
	s := &tailSampler{
		collector: collector,
		cfg:       cfg,
		traces:    make(map[[16]byte]*traceBuffer),
		stopCh:    make(chan struct{}),
		doneCh:    make(chan struct{}),
	}
	go s.loop()
	return s
}

func (s *tailSampler) StopAndFlush(ctx context.Context) {
	if s == nil {
		return
	}

	if ctx == nil {
		ctx = context.Background()
	}
	s.mu.Lock()
	s.stopCtx = ctx
	s.mu.Unlock()

	s.stopOnce.Do(func() {
		close(s.stopCh)
	})

	select {
	case <-s.doneCh:
	case <-ctx.Done():
	}
}

func (s *tailSampler) UpdateConfig(cfg tailSamplingConfig) {
	if s == nil {
		return
	}

	s.mu.Lock()
	s.cfg = cfg
	s.mu.Unlock()
}

func (s *tailSampler) AddOTLPTraces(payload []byte, contentEncoding string, maxDecompressedBytes int64) error {
	if s == nil {
		return fmt.Errorf("tail sampler is nil")
	}

	decoded, err := decodeOTLPBody(payload, contentEncoding, maxDecompressedBytes)
	if err != nil {
		return err
	}

	var req coltracev1.ExportTraceServiceRequest
	if err := proto.Unmarshal(decoded, &req); err != nil {
		return fmt.Errorf("unmarshal traces: %w", err)
	}

	s.addRequest(&req)
	return nil
}

func (s *tailSampler) addRequest(req *coltracev1.ExportTraceServiceRequest) {
	if s == nil || req == nil {
		return
	}

	now := time.Now()

	s.mu.Lock()
	defer s.mu.Unlock()

	cfg := s.cfg
	if cfg.MaxTraces <= 0 {
		return
	}

	for _, rs := range req.ResourceSpans {
		if rs == nil {
			continue
		}
		resource := rs.Resource
		for _, ss := range rs.ScopeSpans {
			if ss == nil {
				continue
			}
			scope := ss.Scope
			for _, span := range ss.Spans {
				if span == nil || len(span.TraceId) != 16 {
					continue
				}

				var traceID [16]byte
				copy(traceID[:], span.TraceId)

				buf := s.traces[traceID]
				if buf == nil {
					if len(s.traces) >= cfg.MaxTraces {
						s.evictOldestLocked()
					}
					if len(s.traces) >= cfg.MaxTraces {
						continue
					}
					buf = &traceBuffer{
						firstSeen: now,
						lastSeen:  now,
					}
					s.traces[traceID] = buf
				}

				buf.lastSeen = now
				buf.spans = append(buf.spans, spanRecord{
					resource: resource,
					scope:    scope,
					span:     span,
				})

				if span.Status != nil && span.Status.Code == tracev1.Status_STATUS_CODE_ERROR {
					buf.hasError = true
				}

				if span.EndTimeUnixNano > span.StartTimeUnixNano {
					delta := span.EndTimeUnixNano - span.StartTimeUnixNano
					if delta > uint64(maxInt64) {
						delta = uint64(maxInt64)
					}
					d := time.Duration(delta)
					if d > buf.maxDuration {
						buf.maxDuration = d
					}
				}
			}
		}
	}
}

func (s *tailSampler) evictOldestLocked() {
	var oldestID [16]byte
	var oldestAt time.Time
	found := false
	for traceID, buf := range s.traces {
		if buf == nil {
			continue
		}
		if !found || buf.firstSeen.Before(oldestAt) {
			oldestID = traceID
			oldestAt = buf.firstSeen
			found = true
		}
	}
	if found {
		delete(s.traces, oldestID)
	}
}

func (s *tailSampler) loop() {
	ticker := time.NewTicker(250 * time.Millisecond)
	defer ticker.Stop()
	defer close(s.doneCh)

	for {
		select {
		case <-s.stopCh:
			ctx := context.Background()
			s.mu.Lock()
			if s.stopCtx != nil {
				ctx = s.stopCtx
			}
			s.mu.Unlock()
			s.flush(ctx, true)
			return
		case <-ticker.C:
			s.flush(context.Background(), false)
		}
	}
}

func (s *tailSampler) flush(ctx context.Context, flushAll bool) {
	if s == nil {
		return
	}

	now := time.Now()

	s.mu.Lock()
	cfg := s.cfg
	if cfg.MaxTraces <= 0 || cfg.DecisionWait <= 0 || s.collector == nil {
		s.traces = make(map[[16]byte]*traceBuffer)
		s.mu.Unlock()
		return
	}

	var due []*traceBuffer
	var dueIDs [][16]byte
	for traceID, buf := range s.traces {
		if buf == nil {
			delete(s.traces, traceID)
			continue
		}
		if flushAll || now.Sub(buf.firstSeen) >= cfg.DecisionWait {
			due = append(due, buf)
			dueIDs = append(dueIDs, traceID)
			delete(s.traces, traceID)
		}
	}
	s.mu.Unlock()

	for i, buf := range due {
		traceID := dueIDs[i]
		if !shouldKeepTrace(traceID, buf, cfg) {
			continue
		}

		req := buildTraceExportRequest(buf.spans)
		if req == nil {
			continue
		}
		s.collector.ForwardTraces(ctx, req)
	}
}

func decodeOTLPBody(payload []byte, contentEncoding string, maxDecompressedBytes int64) ([]byte, error) {
	enc := strings.TrimSpace(contentEncoding)
	if enc == "" || strings.EqualFold(enc, "identity") {
		return payload, nil
	}
	if !strings.EqualFold(enc, "gzip") {
		return nil, fmt.Errorf("unsupported content encoding: %s", enc)
	}

	if len(payload) == 0 {
		return payload, nil
	}

	r, err := gzip.NewReader(bytes.NewReader(payload))
	if err != nil {
		return nil, fmt.Errorf("gzip reader: %w", err)
	}
	defer r.Close()

	limit := maxDecompressedBytes
	if limit <= 0 {
		limit = 4 << 20
	}

	decoded, err := io.ReadAll(io.LimitReader(r, limit+1))
	if err != nil {
		return nil, fmt.Errorf("gzip read: %w", err)
	}
	if int64(len(decoded)) > limit {
		return nil, fmt.Errorf("decompressed payload too large")
	}
	return decoded, nil
}

func shouldKeepTrace(traceID [16]byte, buf *traceBuffer, cfg tailSamplingConfig) bool {
	if buf == nil {
		return false
	}
	if buf.hasError {
		return true
	}
	if cfg.SlowThreshold > 0 && buf.maxDuration >= cfg.SlowThreshold {
		return true
	}

	return sampleByTraceID(traceID, cfg.SampleRate)
}

func sampleByTraceID(traceID [16]byte, rate float64) bool {
	if rate >= 1 {
		return true
	}
	if rate <= 0 {
		return false
	}

	n := binary.BigEndian.Uint64(traceID[:8])
	threshold := uint64(rate * float64(maxUint64))
	return n <= threshold
}

func buildTraceExportRequest(spans []spanRecord) *coltracev1.ExportTraceServiceRequest {
	if len(spans) == 0 {
		return nil
	}

	req := &coltracev1.ExportTraceServiceRequest{}

	resourceMap := make(map[*resourcev1.Resource]*tracev1.ResourceSpans)
	scopeMap := make(map[*tracev1.ResourceSpans]map[*commonv1.InstrumentationScope]*tracev1.ScopeSpans)

	for _, rec := range spans {
		if rec.span == nil {
			continue
		}

		rs := resourceMap[rec.resource]
		if rs == nil {
			rs = &tracev1.ResourceSpans{Resource: rec.resource}
			resourceMap[rec.resource] = rs
			req.ResourceSpans = append(req.ResourceSpans, rs)
		}

		if scopeMap[rs] == nil {
			scopeMap[rs] = make(map[*commonv1.InstrumentationScope]*tracev1.ScopeSpans)
		}
		ss := scopeMap[rs][rec.scope]
		if ss == nil {
			ss = &tracev1.ScopeSpans{Scope: rec.scope}
			scopeMap[rs][rec.scope] = ss
			rs.ScopeSpans = append(rs.ScopeSpans, ss)
		}

		ss.Spans = append(ss.Spans, rec.span)
	}

	if len(req.ResourceSpans) == 0 {
		return nil
	}
	return req
}
