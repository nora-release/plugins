package telemetry

import (
	"bytes"
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

func TestCollectorForwardsOTLPWithAuth(t *testing.T) {
	var got struct {
		url   string
		token string
		body  []byte
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
			got.token = r.Header.Get("Authorization")
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

	f := NewCollector(client)
	f.UpdateConfig(Config{
		IngestionBaseURL: "https://api.example.com/v1/telemetry/otlp",
		AuthToken:        "tkn",
		MaxRequestBytes:  1024,
	})

	payload := []byte{0x01, 0x02, 0x03}
	req := httptest.NewRequest(http.MethodPost, "http://collector/v1/traces", bytes.NewReader(payload))
	req.Header.Set("Content-Type", "application/x-protobuf")

	w := httptest.NewRecorder()
	f.Handler().ServeHTTP(w, req)

	if w.Code != http.StatusOK {
		t.Fatalf("unexpected status: %d", w.Code)
	}
	if got.url != "https://api.example.com/v1/telemetry/otlp/traces" {
		t.Fatalf("unexpected forwarded url: %s", got.url)
	}
	if got.token != "Bearer tkn" {
		t.Fatalf("unexpected authorization: %q", got.token)
	}
	if !bytes.Equal(got.body, payload) {
		t.Fatalf("unexpected forwarded body: %v", got.body)
	}
}

func TestCollectorReturns503WhenNotConfigured(t *testing.T) {
	f := NewCollector(&http.Client{Transport: roundTripperFunc(func(_ *http.Request) (*http.Response, error) {
		t.Fatal("unexpected request")
		return nil, nil
	})})

	w := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodPost, "http://collector/v1/traces", bytes.NewReader([]byte{0x01}))
	f.Handler().ServeHTTP(w, req)

	if w.Code != http.StatusServiceUnavailable {
		t.Fatalf("expected 503, got %d", w.Code)
	}
}

func TestCollectorEnforcesRequestSize(t *testing.T) {
	f := NewCollector(&http.Client{Transport: roundTripperFunc(func(_ *http.Request) (*http.Response, error) {
		t.Fatal("unexpected request")
		return nil, nil
	})})
	f.UpdateConfig(Config{
		IngestionBaseURL: "https://api.example.com/v1/telemetry/otlp",
		MaxRequestBytes:  1,
	})

	w := httptest.NewRecorder()
	req := httptest.NewRequest(http.MethodPost, "http://collector/v1/traces", bytes.NewReader([]byte{0x01, 0x02}))
	f.Handler().ServeHTTP(w, req)

	if w.Code != http.StatusRequestEntityTooLarge {
		t.Fatalf("expected 413, got %d", w.Code)
	}
}

func TestCollectorStartExposesEndpoint(t *testing.T) {
	f := NewCollector(http.DefaultClient)
	endpoint, err := f.Start()
	if err != nil {
		t.Skipf("start failed: %v", err)
	}
	if endpoint == "" {
		t.Fatalf("expected endpoint to be set")
	}

	ctx, cancel := context.WithTimeout(context.Background(), time.Second)
	defer cancel()
	_ = f.Shutdown(ctx)
}

type roundTripperFunc func(*http.Request) (*http.Response, error)

func (rt roundTripperFunc) RoundTrip(r *http.Request) (*http.Response, error) {
	if r.Body == nil {
		r.Body = io.NopCloser(bytes.NewReader(nil))
	}
	return rt(r)
}
