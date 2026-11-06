package telemetry

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	collogsv1 "go.opentelemetry.io/proto/otlp/collector/logs/v1"
	"google.golang.org/protobuf/proto"
)

func TestArtifactUploaderUploadLifecycle(t *testing.T) {
	var got struct {
		uploadReq artifactUploadURLRequest
		putBody   []byte
	}

	var server *httptest.Server
	server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/v1/telemetry/policy":
			w.WriteHeader(http.StatusNotModified)
			return

		case r.Method == http.MethodPost && r.URL.Path == "/v1/telemetry/artifacts/upload_url":
			if r.Header.Get("Authorization") != "Bearer tkn" {
				t.Fatalf("unexpected auth header: %q", r.Header.Get("Authorization"))
			}

			if err := json.NewDecoder(r.Body).Decode(&got.uploadReq); err != nil {
				t.Fatalf("decode upload_url request: %v", err)
			}

			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"id":"obj1","upload_url":"` + server.URL + `/upload","method":"PUT","key":"k","public_url":"` + server.URL + `/v1/admin/telemetry/artifacts/obj1/view","download_url":"` + server.URL + `/v1/admin/telemetry/artifacts/obj1/download"}`))
			return

		case r.Method == http.MethodPut && r.URL.Path == "/upload":
			body, _ := io.ReadAll(r.Body)
			got.putBody = body
			w.WriteHeader(http.StatusOK)
			return

		case r.Method == http.MethodPost && r.URL.Path == "/v1/telemetry/artifacts/obj1/complete":
			if r.Header.Get("Authorization") != "Bearer tkn" {
				t.Fatalf("unexpected auth header: %q", r.Header.Get("Authorization"))
			}
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"status":"ok"}`))
			return

		default:
			t.Fatalf("unexpected request: %s %s", r.Method, r.URL.Path)
		}
	}))
	defer server.Close()

	collector := NewCollector(server.Client())
	collector.UpdateConfig(Config{
		IngestionBaseURL: server.URL + "/v1/telemetry/otlp",
		AuthToken:        "tkn",
	})

	uploader := NewArtifactUploader(collector)
	payload := []byte{0x01, 0x02, 0x03}
	start := time.Unix(1, 0)
	end := time.Unix(2, 0)
	result, err := uploader.Upload(context.Background(), ArtifactUploadRequest{
		Type:          ArtifactTypeHostThreadTop,
		Filename:      "host_thread_top.jsonl.gz",
		ContentType:   "application/gzip",
		Payload:       payload,
		StartTime:     start,
		EndTime:       end,
		TriggerReason: TriggerReasonHighCPU,
	})
	if err != nil {
		t.Fatalf("Upload: %v", err)
	}

	if result.ObjectID != "obj1" {
		t.Fatalf("unexpected object id: %s", result.ObjectID)
	}
	if result.ViewURL != server.URL+"/v1/admin/telemetry/artifacts/obj1/view" {
		t.Fatalf("unexpected view url: %s", result.ViewURL)
	}
	if result.DownloadURL != server.URL+"/v1/admin/telemetry/artifacts/obj1/download" {
		t.Fatalf("unexpected download url: %s", result.DownloadURL)
	}
	if result.SHA256Hex == "" || got.uploadReq.FileHash == "" {
		t.Fatalf("expected sha256 to be computed")
	}
	if result.SHA256Hex != got.uploadReq.FileHash {
		t.Fatalf("sha mismatch: %s != %s", result.SHA256Hex, got.uploadReq.FileHash)
	}
	if !bytes.Equal(got.putBody, payload) {
		t.Fatalf("unexpected uploaded payload: %v", got.putBody)
	}
}

func TestCollectorReportArtifactUploadedEmitsOTLPLog(t *testing.T) {
	var got struct {
		path   string
		body   []byte
		auth   string
		ct     string
		accept string
	}

	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/v1/telemetry/policy":
			w.WriteHeader(http.StatusNotModified)
			return
		case r.Method == http.MethodPost && r.URL.Path == "/v1/telemetry/otlp/traces":
			w.WriteHeader(http.StatusOK)
			return
		case r.Method == http.MethodPost && r.URL.Path == "/v1/telemetry/otlp/logs":
			got.path = r.URL.Path
			got.auth = r.Header.Get("Authorization")
			got.ct = r.Header.Get("Content-Type")
			got.accept = r.Header.Get("Accept")
			body, _ := io.ReadAll(r.Body)
			got.body = body
			w.WriteHeader(http.StatusOK)
			return
		default:
			t.Fatalf("unexpected request: %s %s", r.Method, r.URL.Path)
		}
	}))
	defer server.Close()

	collector := NewCollector(server.Client())
	collector.UpdateConfig(Config{
		IngestionBaseURL: server.URL + "/v1/telemetry/otlp",
		AuthToken:        "tkn",
	})

	err := collector.ReportArtifactUploaded(context.Background(), &ArtifactUploadResult{
		Type:          ArtifactTypePprofCPU,
		TriggerReason: TriggerReasonHighCPU,
		StartTime:     time.Unix(10, 0),
		EndTime:       time.Unix(20, 0),
		SHA256Hex:     "deadbeef",
		SizeBytes:     123,
		ViewURL:       "https://view",
		DownloadURL:   "https://download",
	})
	if err != nil {
		t.Fatalf("ReportArtifactUploaded: %v", err)
	}

	if got.path != "/v1/telemetry/otlp/logs" {
		t.Fatalf("unexpected path: %s", got.path)
	}
	if got.auth != "Bearer tkn" {
		t.Fatalf("unexpected auth: %q", got.auth)
	}
	if got.ct != "application/x-protobuf" || got.accept != "application/x-protobuf" {
		t.Fatalf("unexpected content negotiation: ct=%q accept=%q", got.ct, got.accept)
	}

	var req collogsv1.ExportLogsServiceRequest
	if err := proto.Unmarshal(got.body, &req); err != nil {
		t.Fatalf("unmarshal logs: %v", err)
	}
	if len(req.ResourceLogs) != 1 || len(req.ResourceLogs[0].ScopeLogs) != 1 || len(req.ResourceLogs[0].ScopeLogs[0].LogRecords) != 1 {
		t.Fatalf("unexpected log shape")
	}

	record := req.ResourceLogs[0].ScopeLogs[0].LogRecords[0]
	if len(record.TraceId) != 16 || len(record.SpanId) != 8 {
		t.Fatalf("expected trace/span ids to be set, trace_id=%d span_id=%d", len(record.TraceId), len(record.SpanId))
	}

	var attrs = map[string]string{}
	for _, kv := range record.Attributes {
		if kv == nil || kv.Value == nil {
			continue
		}
		if s := kv.Value.GetStringValue(); s != "" {
			attrs[kv.Key] = s
		}
	}
	if attrs["artifact.type"] != string(ArtifactTypePprofCPU) {
		t.Fatalf("missing artifact.type, got %q", attrs["artifact.type"])
	}
	if attrs["trigger.reason"] != string(TriggerReasonHighCPU) {
		t.Fatalf("missing trigger.reason, got %q", attrs["trigger.reason"])
	}
	if attrs["artifact.sha256"] != "deadbeef" {
		t.Fatalf("missing sha256, got %q", attrs["artifact.sha256"])
	}
	if attrs["artifact.view_url"] != "https://view" || attrs["artifact.download_url"] != "https://download" {
		t.Fatalf("missing urls: %v", attrs)
	}
}
