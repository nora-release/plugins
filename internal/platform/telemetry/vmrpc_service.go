package telemetry

import (
	"bytes"
	"compress/gzip"
	"context"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
)

type VMRPCTelemetryService struct {
	vmrpc.UnimplementedTelemetryServiceServer

	collector *Collector
	uploader  *ArtifactUploader
}

// noOpTelemetryService accepts and discards all telemetry data.
type noOpTelemetryService struct {
	vmrpc.UnimplementedTelemetryServiceServer
}

func (s *noOpTelemetryService) ExportOTLP(_ context.Context, _ *vmrpc.OtlpEnvelope) (*vmrpc.ExportAck, error) {
	return &vmrpc.ExportAck{StatusCode: 200}, nil
}

// NewNoOpTelemetryService returns a TelemetryService that accepts and discards
// all telemetry data. Use this when no collector is configured to avoid
// Unimplemented gRPC errors from the guest.
func NewNoOpTelemetryService() vmrpc.TelemetryServiceServer {
	return &noOpTelemetryService{}
}

func NewVMRPCTelemetryService(collector *Collector) *VMRPCTelemetryService {
	return &VMRPCTelemetryService{
		collector: collector,
		uploader:  NewArtifactUploader(collector),
	}
}

func (s *VMRPCTelemetryService) ExportOTLP(ctx context.Context, envelope *vmrpc.OtlpEnvelope) (*vmrpc.ExportAck, error) {
	status, forwardErr := s.handleEnvelope(ctx, envelope)
	ack := &vmrpc.ExportAck{StatusCode: uint32(status)}
	if forwardErr != nil {
		ack.Error = forwardErr.Error()
	}
	return ack, nil
}

func (s *VMRPCTelemetryService) handleEnvelope(ctx context.Context, envelope *vmrpc.OtlpEnvelope) (int, error) {
	if envelope == nil {
		return http.StatusBadRequest, fmt.Errorf("missing envelope")
	}

	if envelope.Signal == vmrpc.TelemetrySignal_TELEMETRY_SIGNAL_PROFILE_ARTIFACT {
		return s.handleProfileArtifact(ctx, envelope)
	}

	signal, err := normalizeOTLPSignal(envelope.Signal)
	if err != nil {
		return http.StatusBadRequest, err
	}

	payload, err := s.decodePayload(envelope.Payload, envelope.Compression)
	if err != nil {
		return http.StatusBadRequest, err
	}

	if s.collector == nil {
		return http.StatusServiceUnavailable, fmt.Errorf("collector is not configured")
	}

	return s.collector.ForwardOTLP(ctx, signal, payload, "application/x-protobuf", "application/x-protobuf", "")
}

func normalizeOTLPSignal(signal vmrpc.TelemetrySignal) (string, error) {
	switch signal {
	case vmrpc.TelemetrySignal_TELEMETRY_SIGNAL_TRACES:
		return "traces", nil
	case vmrpc.TelemetrySignal_TELEMETRY_SIGNAL_METRICS:
		return "metrics", nil
	case vmrpc.TelemetrySignal_TELEMETRY_SIGNAL_LOGS:
		return "logs", nil
	default:
		return "", fmt.Errorf("unsupported signal: %s", signal.String())
	}
}

func (s *VMRPCTelemetryService) handleProfileArtifact(ctx context.Context, envelope *vmrpc.OtlpEnvelope) (int, error) {
	if envelope == nil {
		return http.StatusBadRequest, fmt.Errorf("missing envelope")
	}
	if len(envelope.Payload) == 0 {
		return http.StatusBadRequest, fmt.Errorf("missing payload")
	}
	if s.collector == nil || s.uploader == nil {
		return http.StatusServiceUnavailable, fmt.Errorf("collector is not configured")
	}

	artifactType := strings.TrimSpace(envelope.Attributes["artifact.type"])
	if artifactType == "" {
		return http.StatusBadRequest, fmt.Errorf("missing artifact.type")
	}
	filename := strings.TrimSpace(envelope.Attributes["artifact.filename"])
	if filename == "" {
		filename = artifactType + ".bin.gz"
	}

	startTime, _ := time.Parse(time.RFC3339Nano, strings.TrimSpace(envelope.Attributes["artifact.start_time"]))
	endTime, _ := time.Parse(time.RFC3339Nano, strings.TrimSpace(envelope.Attributes["artifact.end_time"]))
	if startTime.IsZero() {
		startTime = time.Now()
	}
	if endTime.IsZero() {
		endTime = startTime
	}

	triggerReason := TriggerReason(strings.TrimSpace(envelope.Attributes["trigger.reason"]))
	uploaded, err := s.uploader.Upload(ctx, ArtifactUploadRequest{
		Type:          ArtifactType(artifactType),
		Filename:      filename,
		ContentType:   "application/gzip",
		Payload:       envelope.Payload,
		StartTime:     startTime,
		EndTime:       endTime,
		TriggerReason: triggerReason,
	})
	if err != nil {
		return http.StatusBadGateway, err
	}
	_ = s.collector.ReportArtifactUploaded(ctx, uploaded)
	return http.StatusOK, nil
}

func (s *VMRPCTelemetryService) decodePayload(payload []byte, compression vmrpc.TelemetryCompression) ([]byte, error) {
	switch compression {
	case vmrpc.TelemetryCompression_TELEMETRY_COMPRESSION_UNSPECIFIED, vmrpc.TelemetryCompression_TELEMETRY_COMPRESSION_NONE:
		return payload, nil
	case vmrpc.TelemetryCompression_TELEMETRY_COMPRESSION_GZIP:
		return s.gunzip(payload)
	default:
		return nil, fmt.Errorf("unsupported compression: %s", compression.String())
	}
}

func (s *VMRPCTelemetryService) gunzip(payload []byte) ([]byte, error) {
	if len(payload) == 0 {
		return payload, nil
	}

	r, err := gzip.NewReader(bytes.NewReader(payload))
	if err != nil {
		return nil, fmt.Errorf("gzip reader: %w", err)
	}
	defer r.Close()

	limit := int64(4 << 20)
	if s.collector != nil {
		limit = s.collector.maxRequestBytes.Load()
	}
	if limit <= 0 {
		limit = 4 << 20
	}

	data, err := io.ReadAll(io.LimitReader(r, limit+1))
	if err != nil {
		return nil, fmt.Errorf("gzip read: %w", err)
	}
	if int64(len(data)) > limit {
		return nil, fmt.Errorf("decompressed payload too large")
	}
	return data, nil
}
