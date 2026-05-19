package telemetry

import (
	"bytes"
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"path"
	"runtime"
	"strings"
	"time"

	"github.com/nora/sandbox-vm/internal/platform/otlp"
	collogsv1 "go.opentelemetry.io/proto/otlp/collector/logs/v1"
	coltracev1 "go.opentelemetry.io/proto/otlp/collector/trace/v1"
	commonv1 "go.opentelemetry.io/proto/otlp/common/v1"
	logsv1 "go.opentelemetry.io/proto/otlp/logs/v1"
	resourcev1 "go.opentelemetry.io/proto/otlp/resource/v1"
	tracev1 "go.opentelemetry.io/proto/otlp/trace/v1"
	"google.golang.org/protobuf/proto"
)

type TriggerReason string

const (
	TriggerReasonHighCPU     TriggerReason = "high_cpu"
	TriggerReasonSlowRequest TriggerReason = "slow_request"
	TriggerReasonErrorBurst  TriggerReason = "error_burst"
)

type ArtifactType string

const (
	ArtifactTypeHostThreadTop   ArtifactType = "host_thread_top"
	ArtifactTypeGuestProcessTop ArtifactType = "guest_process_top"
	ArtifactTypePprofCPU        ArtifactType = "pprof_cpu"
	ArtifactTypePprofHeap       ArtifactType = "pprof_heap"
	ArtifactTypePprofMutex      ArtifactType = "pprof_mutex"
	ArtifactTypePprofBlock      ArtifactType = "pprof_block"
	ArtifactTypePprofGoroutine  ArtifactType = "pprof_goroutine"
	ArtifactTypePerf            ArtifactType = "perf"
)

type ArtifactUploadRequest struct {
	Type          ArtifactType
	Filename      string
	ContentType   string
	Payload       []byte
	StartTime     time.Time
	EndTime       time.Time
	TriggerReason TriggerReason
}

type ArtifactUploadResult struct {
	Type          ArtifactType
	TriggerReason TriggerReason

	ObjectID string
	Key      string

	StartTime time.Time
	EndTime   time.Time

	SHA256Hex   string
	SizeBytes   int64
	ViewURL     string
	DownloadURL string
}

type artifactUploadURLRequest struct {
	Filename    string `json:"filename"`
	ContentType string `json:"content_type"`
	SizeBytes   int64  `json:"size_bytes"`
	FileHash    string `json:"file_hash,omitempty"`
}

type artifactUploadURLResponse struct {
	ID          string `json:"id"`
	UploadURL   string `json:"upload_url,omitempty"`
	Method      string `json:"method,omitempty"`
	Key         string `json:"key"`
	PublicURL   string `json:"public_url,omitempty"`
	DownloadURL string `json:"download_url,omitempty"`
}

type ArtifactUploader struct {
	collector *Collector
}

func NewArtifactUploader(collector *Collector) *ArtifactUploader {
	return &ArtifactUploader{collector: collector}
}

func (u *ArtifactUploader) Upload(ctx context.Context, req ArtifactUploadRequest) (*ArtifactUploadResult, error) {
	if u == nil || u.collector == nil {
		return nil, errors.New("uploader is not configured")
	}
	if ctx == nil {
		ctx = context.Background()
	}

	req.Filename = strings.TrimSpace(req.Filename)
	req.ContentType = strings.TrimSpace(req.ContentType)
	if strings.TrimSpace(string(req.Type)) == "" {
		return nil, errors.New("missing artifact type")
	}
	if req.Filename == "" {
		return nil, errors.New("missing filename")
	}
	if req.ContentType == "" {
		req.ContentType = "application/gzip"
	}
	if len(req.Payload) == 0 {
		return nil, errors.New("missing artifact payload")
	}

	telemetryBaseURL, err := deriveTelemetryBaseURL(u.collector.IngestionBaseURL())
	if err != nil {
		return nil, err
	}
	token := u.collector.AuthToken()
	if token == "" {
		return nil, errors.New("telemetry auth token is not configured")
	}

	digest := sha256.Sum256(req.Payload)
	sha256Hex := hex.EncodeToString(digest[:])

	uploadURLInfo, err := u.getUploadURL(ctx, telemetryBaseURL, token, artifactUploadURLRequest{
		Filename:    req.Filename,
		ContentType: req.ContentType,
		SizeBytes:   int64(len(req.Payload)),
		FileHash:    sha256Hex,
	})
	if err != nil {
		return nil, err
	}

	if uploadURLInfo.UploadURL != "" {
		if err := u.putObject(ctx, uploadURLInfo.UploadURL, uploadURLInfo.Method, req.ContentType, req.Payload); err != nil {
			return nil, err
		}

		if err := u.completeUpload(ctx, telemetryBaseURL, token, uploadURLInfo.ID); err != nil {
			return nil, err
		}
	}

	return &ArtifactUploadResult{
		Type:          req.Type,
		TriggerReason: req.TriggerReason,
		ObjectID:      uploadURLInfo.ID,
		Key:           uploadURLInfo.Key,
		StartTime:     req.StartTime,
		EndTime:       req.EndTime,
		SHA256Hex:     sha256Hex,
		SizeBytes:     int64(len(req.Payload)),
		ViewURL:       uploadURLInfo.PublicURL,
		DownloadURL:   uploadURLInfo.DownloadURL,
	}, nil
}

func deriveTelemetryBaseURL(ingestionBaseURL string) (string, error) {
	base := strings.TrimRight(strings.TrimSpace(ingestionBaseURL), "/")
	if base == "" {
		return "", errors.New("telemetry ingestion is not configured")
	}

	u, err := url.Parse(base)
	if err != nil {
		return "", fmt.Errorf("parse telemetry base url: %w", err)
	}

	cleanPath := path.Clean(u.Path)
	if cleanPath == "." || cleanPath == "/" {
		return "", errors.New("telemetry ingestion base url is missing path")
	}

	parts := strings.Split(strings.Trim(cleanPath, "/"), "/")
	if len(parts) == 0 || parts[len(parts)-1] != "otlp" {
		return "", errors.New("telemetry ingestion base url must end with /otlp")
	}
	u.Path = "/" + strings.Join(parts[:len(parts)-1], "/")
	return strings.TrimRight(u.String(), "/"), nil
}

func (u *ArtifactUploader) getUploadURL(ctx context.Context, telemetryBaseURL string, token string, req artifactUploadURLRequest) (*artifactUploadURLResponse, error) {
	body, err := json.Marshal(req)
	if err != nil {
		return nil, fmt.Errorf("encode upload url request: %w", err)
	}

	endpoint := strings.TrimRight(telemetryBaseURL, "/") + "/artifacts/upload_url"
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("create upload url request: %w", err)
	}
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("Accept", "application/json")
	httpReq.Header.Set("Authorization", "Bearer "+token)

	resp, err := u.collector.client.Do(httpReq)
	if err != nil {
		return nil, fmt.Errorf("request upload url: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		data, _ := io.ReadAll(io.LimitReader(resp.Body, 2048))
		return nil, fmt.Errorf("request upload url: status=%d body=%s", resp.StatusCode, strings.TrimSpace(string(data)))
	}

	var decoded artifactUploadURLResponse
	if err := json.NewDecoder(resp.Body).Decode(&decoded); err != nil {
		return nil, fmt.Errorf("decode upload url response: %w", err)
	}
	if strings.TrimSpace(decoded.ID) == "" {
		return nil, errors.New("upload url response is missing id")
	}
	return &decoded, nil
}

func (u *ArtifactUploader) putObject(ctx context.Context, uploadURL string, method string, contentType string, payload []byte) error {
	method = strings.TrimSpace(method)
	if method == "" {
		method = http.MethodPut
	}
	if method != http.MethodPut {
		return fmt.Errorf("unsupported upload method: %s", method)
	}

	httpReq, err := http.NewRequestWithContext(ctx, method, uploadURL, bytes.NewReader(payload))
	if err != nil {
		return fmt.Errorf("create upload request: %w", err)
	}
	httpReq.Header.Set("Content-Type", contentType)

	resp, err := u.collector.client.Do(httpReq)
	if err != nil {
		return fmt.Errorf("upload object: %w", err)
	}
	_ = resp.Body.Close()

	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("upload object: status=%d", resp.StatusCode)
	}
	return nil
}

func (u *ArtifactUploader) completeUpload(ctx context.Context, telemetryBaseURL string, token string, objectID string) error {
	endpoint := strings.TrimRight(telemetryBaseURL, "/") + "/artifacts/" + objectID + "/complete"
	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, nil)
	if err != nil {
		return fmt.Errorf("create complete upload request: %w", err)
	}
	httpReq.Header.Set("Authorization", "Bearer "+token)

	resp, err := u.collector.client.Do(httpReq)
	if err != nil {
		return fmt.Errorf("complete upload: %w", err)
	}
	_ = resp.Body.Close()
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return fmt.Errorf("complete upload: status=%d", resp.StatusCode)
	}
	return nil
}

func (c *Collector) ReportArtifactUploaded(ctx context.Context, result *ArtifactUploadResult) error {
	if c == nil {
		return fmt.Errorf("otlp collector is nil")
	}
	if result == nil {
		return fmt.Errorf("missing artifact result")
	}

	if ctx == nil {
		ctx = context.Background()
	}

	traceID, parentSpanID, hasParent := parseTraceParent(TraceContextFromContext(ctx).Traceparent)
	if traceID == nil {
		traceID = make([]byte, 16)
		if _, err := rand.Read(traceID); err != nil {
			return fmt.Errorf("trace id: %w", err)
		}
	}

	spanID := make([]byte, 8)
	if _, err := rand.Read(spanID); err != nil {
		return fmt.Errorf("span id: %w", err)
	}

	start := result.StartTime
	if start.IsZero() {
		start = time.Now()
	}
	end := time.Now()
	if end.Before(start) {
		start = end
	}

	c.reportArtifactUploadedSpan(ctx, traceID, spanID, parentSpanID, hasParent, TraceContextFromContext(ctx).Tracestate, *result, start, end)

	payload, err := marshalArtifactLogEvent(ArtifactLogEvent{
		TraceID: traceID,
		SpanID:  spanID,

		Type:          result.Type,
		TriggerReason: result.TriggerReason,
		StartTime:     result.StartTime,
		EndTime:       result.EndTime,
		SHA256Hex:     result.SHA256Hex,
		SizeBytes:     result.SizeBytes,
		ViewURL:       result.ViewURL,
		DownloadURL:   result.DownloadURL,
	})
	if err != nil {
		return err
	}

	status, err := c.ForwardOTLP(ctx, "logs", payload, "application/x-protobuf", "application/x-protobuf", "")
	if err != nil {
		return err
	}
	if status < 200 || status >= 300 {
		return fmt.Errorf("artifact log export returned status %d", status)
	}
	return nil
}

type ArtifactLogEvent struct {
	TraceID []byte
	SpanID  []byte

	Type          ArtifactType
	TriggerReason TriggerReason
	StartTime     time.Time
	EndTime       time.Time
	SHA256Hex     string
	SizeBytes     int64
	ViewURL       string
	DownloadURL   string
}

func marshalArtifactLogEvent(event ArtifactLogEvent) ([]byte, error) {
	if event.Type == "" {
		return nil, fmt.Errorf("missing artifact type")
	}

	now := time.Now()
	record := &logsv1.LogRecord{
		TimeUnixNano:   uint64(now.UnixNano()),
		SeverityNumber: logsv1.SeverityNumber_SEVERITY_NUMBER_INFO,
		SeverityText:   "INFO",
		TraceId:        event.TraceID,
		SpanId:         event.SpanID,
		Body: &commonv1.AnyValue{
			Value: &commonv1.AnyValue_StringValue{StringValue: "telemetry.artifact.uploaded"},
		},
		Attributes: []*commonv1.KeyValue{
			otlp.KVString("artifact.type", string(event.Type)),
			otlp.KVString("trigger.reason", string(event.TriggerReason)),
			otlp.KVString("artifact.start_time", event.StartTime.UTC().Format(time.RFC3339Nano)),
			otlp.KVString("artifact.end_time", event.EndTime.UTC().Format(time.RFC3339Nano)),
			otlp.KVString("artifact.sha256", event.SHA256Hex),
			otlp.KVInt("artifact.size_bytes", event.SizeBytes),
			otlp.KVString("artifact.view_url", event.ViewURL),
			otlp.KVString("artifact.download_url", event.DownloadURL),
		},
	}

	req := &collogsv1.ExportLogsServiceRequest{
		ResourceLogs: []*logsv1.ResourceLogs{{
			Resource: &resourcev1.Resource{
				Attributes: []*commonv1.KeyValue{
					otlp.KVString("service.name", "host-app"),
					otlp.KVString("os.type", runtime.GOOS),
				},
			},
			ScopeLogs: []*logsv1.ScopeLogs{{
				Scope: &commonv1.InstrumentationScope{
					Name:    "nora.telemetry",
					Version: "phase2",
				},
				LogRecords: []*logsv1.LogRecord{record},
			}},
		}},
	}

	return proto.Marshal(req)
}

func (c *Collector) reportArtifactUploadedSpan(ctx context.Context, traceID []byte, spanID []byte, parentSpanID []byte, hasParent bool, tracestate string, result ArtifactUploadResult, start time.Time, end time.Time) {
	if c == nil || len(traceID) != 16 || len(spanID) != 8 {
		return
	}

	baseURL := c.IngestionBaseURL()
	if baseURL == "" {
		return
	}

	attrs := []*commonv1.KeyValue{
		otlp.KVString("artifact.type", string(result.Type)),
		otlp.KVString("trigger.reason", string(result.TriggerReason)),
		otlp.KVString("artifact.start_time", result.StartTime.UTC().Format(time.RFC3339Nano)),
		otlp.KVString("artifact.end_time", result.EndTime.UTC().Format(time.RFC3339Nano)),
		otlp.KVString("artifact.sha256", result.SHA256Hex),
		otlp.KVInt("artifact.size_bytes", result.SizeBytes),
		otlp.KVString("artifact.view_url", result.ViewURL),
		otlp.KVString("artifact.download_url", result.DownloadURL),
	}

	span := &tracev1.Span{
		TraceId:           traceID,
		SpanId:            spanID,
		Name:              "telemetry.artifact.uploaded",
		Kind:              tracev1.Span_SPAN_KIND_INTERNAL,
		StartTimeUnixNano: uint64(start.UnixNano()),
		EndTimeUnixNano:   uint64(end.UnixNano()),
		Attributes:        attrs,
		Status:            &tracev1.Status{Code: tracev1.Status_STATUS_CODE_OK},
		Events: []*tracev1.Span_Event{{
			TimeUnixNano: uint64(time.Now().UnixNano()),
			Name:         "artifact.uploaded",
			Attributes:   attrs,
		}},
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
					otlp.KVString("service.name", "host-app"),
					otlp.KVString("os.type", runtime.GOOS),
				},
			},
			ScopeSpans: []*tracev1.ScopeSpans{{
				Scope: &commonv1.InstrumentationScope{
					Name:    "nora.telemetry",
					Version: "phase2",
				},
				Spans: []*tracev1.Span{span},
			}},
		}},
	}

	payload, err := proto.Marshal(req)
	if err != nil {
		return
	}

	_, _ = c.forwardUpstream(ctx, baseURL, "traces", payload, "application/x-protobuf", "application/x-protobuf", "")
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
