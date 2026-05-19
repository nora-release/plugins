package telemetry

import (
	"bytes"
	"compress/gzip"
	"context"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
	"google.golang.org/grpc/test/bufconn"
)

func TestVMRPCTelemetryServiceForwardsOTLP(t *testing.T) {
	var got struct {
		url           string
		authorization string
		contentType   string
		accept        string
		body          []byte
	}

	client := &http.Client{
		Transport: roundTripperFunc(func(r *http.Request) (*http.Response, error) {
			if r.Method == http.MethodGet && strings.HasSuffix(r.URL.Path, "/policy") {
				return &http.Response{
					StatusCode: http.StatusNotModified,
					Body:       io.NopCloser(bytes.NewReader(nil)),
					Header:     make(http.Header),
					Request:    r,
				}, nil
			}

			got.url = r.URL.String()
			got.authorization = r.Header.Get("Authorization")
			got.contentType = r.Header.Get("Content-Type")
			got.accept = r.Header.Get("Accept")

			body, _ := io.ReadAll(r.Body)
			_ = r.Body.Close()
			got.body = body

			return &http.Response{
				StatusCode: http.StatusOK,
				Body:       io.NopCloser(bytes.NewReader(nil)),
				Header:     make(http.Header),
				Request:    r,
			}, nil
		}),
	}

	forwarder := NewCollector(client)
	forwarder.UpdateConfig(Config{
		IngestionBaseURL: "https://api.example.com/v1/telemetry/otlp",
		AuthToken:        "tkn",
		MaxRequestBytes:  1024,
	})

	svc := NewVMRPCTelemetryService(forwarder)

	conn := dialBufconn(t, func(server *grpc.Server) {
		vmrpc.RegisterTelemetryServiceServer(server, svc)
	})
	defer conn.Close()

	payload := []byte{0x01, 0x02, 0x03}
	ack, err := vmrpc.NewTelemetryServiceClient(conn).ExportOTLP(context.Background(), &vmrpc.OtlpEnvelope{
		Payload:     payload,
		Signal:      vmrpc.TelemetrySignal_TELEMETRY_SIGNAL_TRACES,
		Compression: vmrpc.TelemetryCompression_TELEMETRY_COMPRESSION_NONE,
	})
	if err != nil {
		t.Fatalf("ExportOTLP: %v", err)
	}
	if ack.StatusCode != http.StatusOK {
		t.Fatalf("unexpected status: %d (%s)", ack.StatusCode, ack.Error)
	}

	if got.url != "https://api.example.com/v1/telemetry/otlp/traces" {
		t.Fatalf("unexpected url: %s", got.url)
	}
	if got.authorization != "Bearer tkn" {
		t.Fatalf("unexpected authorization: %q", got.authorization)
	}
	if got.contentType != "application/x-protobuf" {
		t.Fatalf("unexpected content-type: %q", got.contentType)
	}
	if got.accept != "application/x-protobuf" {
		t.Fatalf("unexpected accept: %q", got.accept)
	}
	if !bytes.Equal(got.body, payload) {
		t.Fatalf("unexpected body: %v", got.body)
	}
}

func TestVMRPCTelemetryServiceGunzip(t *testing.T) {
	var gotBody []byte
	client := &http.Client{
		Transport: roundTripperFunc(func(r *http.Request) (*http.Response, error) {
			body, _ := io.ReadAll(r.Body)
			_ = r.Body.Close()
			gotBody = body

			return &http.Response{
				StatusCode: http.StatusOK,
				Body:       io.NopCloser(bytes.NewReader(nil)),
				Header:     make(http.Header),
				Request:    r,
			}, nil
		}),
	}

	forwarder := NewCollector(client)
	forwarder.UpdateConfig(Config{
		IngestionBaseURL: "https://api.example.com/v1/telemetry/otlp",
		MaxRequestBytes:  1024,
	})

	svc := NewVMRPCTelemetryService(forwarder)

	conn := dialBufconn(t, func(server *grpc.Server) {
		vmrpc.RegisterTelemetryServiceServer(server, svc)
	})
	defer conn.Close()

	var compressed bytes.Buffer
	zw := gzip.NewWriter(&compressed)
	_, _ = zw.Write([]byte("hello"))
	_ = zw.Close()

	ack, err := vmrpc.NewTelemetryServiceClient(conn).ExportOTLP(context.Background(), &vmrpc.OtlpEnvelope{
		Payload:     compressed.Bytes(),
		Signal:      vmrpc.TelemetrySignal_TELEMETRY_SIGNAL_LOGS,
		Compression: vmrpc.TelemetryCompression_TELEMETRY_COMPRESSION_GZIP,
	})
	if err != nil {
		t.Fatalf("ExportOTLP: %v", err)
	}
	if ack.StatusCode != http.StatusOK {
		t.Fatalf("unexpected status: %d (%s)", ack.StatusCode, ack.Error)
	}

	if string(gotBody) != "hello" {
		t.Fatalf("unexpected forwarded body: %q", string(gotBody))
	}
}

func TestVMRPCTelemetryServiceUploadsProfileArtifact(t *testing.T) {
	var (
		uploaded atomic.Bool
		logged   atomic.Bool
	)

	var server *httptest.Server
	server = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case r.Method == http.MethodGet && r.URL.Path == "/v1/telemetry/policy":
			w.WriteHeader(http.StatusNotModified)
			return

		case r.Method == http.MethodPost && r.URL.Path == "/v1/telemetry/artifacts/upload_url":
			if r.Header.Get("Authorization") != "Bearer tkn" {
				t.Fatalf("unexpected auth: %q", r.Header.Get("Authorization"))
			}
			var body map[string]any
			_ = json.NewDecoder(r.Body).Decode(&body)
			w.Header().Set("Content-Type", "application/json")
			_, _ = w.Write([]byte(`{"id":"obj1","upload_url":"` + server.URL + `/upload","method":"PUT","key":"k","public_url":"` + server.URL + `/v1/admin/telemetry/artifacts/obj1/view","download_url":"` + server.URL + `/v1/admin/telemetry/artifacts/obj1/download"}`))
			return

		case r.Method == http.MethodPut && r.URL.Path == "/upload":
			uploaded.Store(true)
			w.WriteHeader(http.StatusOK)
			return

		case r.Method == http.MethodPost && r.URL.Path == "/v1/telemetry/artifacts/obj1/complete":
			if r.Header.Get("Authorization") != "Bearer tkn" {
				t.Fatalf("unexpected auth: %q", r.Header.Get("Authorization"))
			}
			w.WriteHeader(http.StatusOK)
			return

		case r.Method == http.MethodPost && r.URL.Path == "/v1/telemetry/otlp/logs":
			if r.Header.Get("Authorization") != "Bearer tkn" {
				t.Fatalf("unexpected auth: %q", r.Header.Get("Authorization"))
			}
			logged.Store(true)
			w.WriteHeader(http.StatusOK)
			return

		case r.Method == http.MethodPost && r.URL.Path == "/v1/telemetry/otlp/traces":
			if r.Header.Get("Authorization") != "Bearer tkn" {
				t.Fatalf("unexpected auth: %q", r.Header.Get("Authorization"))
			}
			w.WriteHeader(http.StatusOK)
			return

		default:
			t.Fatalf("unexpected request: %s %s", r.Method, r.URL.Path)
		}
	}))
	defer server.Close()

	forwarder := NewCollector(server.Client())
	forwarder.UpdateConfig(Config{
		IngestionBaseURL: server.URL + "/v1/telemetry/otlp",
		AuthToken:        "tkn",
	})
	svc := NewVMRPCTelemetryService(forwarder)

	conn := dialBufconn(t, func(server *grpc.Server) {
		vmrpc.RegisterTelemetryServiceServer(server, svc)
	})
	defer conn.Close()

	start := time.Now().Add(-time.Second).UTC().Format(time.RFC3339Nano)
	end := time.Now().UTC().Format(time.RFC3339Nano)
	ack, err := vmrpc.NewTelemetryServiceClient(conn).ExportOTLP(context.Background(), &vmrpc.OtlpEnvelope{
		Payload:     []byte{0x01, 0x02},
		Signal:      vmrpc.TelemetrySignal_TELEMETRY_SIGNAL_PROFILE_ARTIFACT,
		Compression: vmrpc.TelemetryCompression_TELEMETRY_COMPRESSION_NONE,
		Attributes: map[string]string{
			"artifact.type":       "guest_process_top",
			"artifact.filename":   "guest_process_top.jsonl.gz",
			"artifact.start_time": start,
			"artifact.end_time":   end,
			"trigger.reason":      "high_cpu",
		},
	})
	if err != nil {
		t.Fatalf("ExportOTLP: %v", err)
	}
	if ack.StatusCode != http.StatusOK {
		t.Fatalf("unexpected status: %d (%s)", ack.StatusCode, ack.Error)
	}
	if !uploaded.Load() {
		t.Fatalf("expected artifact to be uploaded")
	}
	if !logged.Load() {
		t.Fatalf("expected artifact log to be exported")
	}
}

func dialBufconn(t *testing.T, register func(*grpc.Server)) *grpc.ClientConn {
	t.Helper()

	listener := bufconn.Listen(1024 * 1024)
	server := grpc.NewServer()
	register(server)
	go func() {
		_ = server.Serve(listener)
	}()
	t.Cleanup(func() {
		server.Stop()
		_ = listener.Close()
	})

	dialer := func(ctx context.Context, _ string) (net.Conn, error) {
		return listener.DialContext(ctx)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	t.Cleanup(cancel)

	conn, err := grpc.DialContext(
		ctx,
		"bufnet",
		grpc.WithContextDialer(dialer),
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	)
	if err != nil {
		t.Fatalf("DialContext: %v", err)
	}
	return conn
}
