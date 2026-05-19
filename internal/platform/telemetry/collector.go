package telemetry

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

type Config struct {
	IngestionBaseURL string
	AuthToken        string
	MaxRequestBytes  int64

	TailDecisionWait            time.Duration
	TailNumTraces               int
	TailExpectedNewTracesPerSec int
}

// Collector runs a local OTLP HTTP receiver (v1/{traces,metrics,logs}) and forwards
// requests to a configured upstream ingestion endpoint.
type Collector struct {
	client *http.Client

	ingestionBaseURL atomic.Value // string
	authToken        atomic.Value // string
	maxRequestBytes  atomic.Int64

	mux *http.ServeMux

	tailDecisionWait            atomic.Int64
	tailNumTraces               atomic.Int64
	tailExpectedNewTracesPerSec atomic.Int64

	tailMu      sync.Mutex
	tailSampler *tailSampler

	policyMu        sync.Mutex
	policyRefreshCh chan struct{}
	policyStopCh    chan struct{}
	policyDoneCh    chan struct{}

	mu       sync.Mutex
	listener net.Listener
	server   *http.Server
	endpoint string
}

func (c *Collector) IngestionBaseURL() string {
	if c == nil {
		return ""
	}
	baseURL, _ := c.ingestionBaseURL.Load().(string)
	return strings.TrimSpace(baseURL)
}

func (c *Collector) AuthToken() string {
	if c == nil {
		return ""
	}
	token, _ := c.authToken.Load().(string)
	return strings.TrimSpace(token)
}

func (c *Collector) ForwardOTLP(ctx context.Context, signal string, payload []byte, contentType, accept, contentEncoding string) (int, error) {
	if c == nil {
		return http.StatusServiceUnavailable, errors.New("otlp forwarder is nil")
	}
	if ctx == nil {
		ctx = context.Background()
	}

	baseURL := c.IngestionBaseURL()
	if baseURL == "" {
		return http.StatusServiceUnavailable, errors.New("telemetry ingestion is not configured")
	}

	limit := c.maxRequestBytes.Load()
	if limit > 0 && int64(len(payload)) > limit {
		return http.StatusRequestEntityTooLarge, fmt.Errorf("request body too large")
	}

	if strings.TrimSpace(signal) == "traces" {
		if sampler := c.tailSamplerForCurrentConfig(); sampler != nil {
			if err := sampler.AddOTLPTraces(payload, contentEncoding, limit); err != nil {
				return http.StatusBadRequest, err
			}
			return http.StatusOK, nil
		}
	}

	return c.forwardUpstream(ctx, baseURL, signal, payload, contentType, accept, contentEncoding)
}

func (c *Collector) forwardUpstream(ctx context.Context, baseURL string, signal string, payload []byte, contentType, accept, contentEncoding string) (int, error) {
	targetURL := baseURL + "/" + signal
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, targetURL, bytes.NewReader(payload))
	if err != nil {
		return http.StatusInternalServerError, fmt.Errorf("create request: %w", err)
	}

	if strings.TrimSpace(contentType) != "" {
		req.Header.Set("Content-Type", contentType)
	}
	if strings.TrimSpace(accept) != "" {
		req.Header.Set("Accept", accept)
	}
	if enc := strings.TrimSpace(contentEncoding); enc != "" {
		req.Header.Set("Content-Encoding", enc)
	}

	if token := c.AuthToken(); token != "" {
		req.Header.Set("Authorization", "Bearer "+token)
	}

	resp, err := c.client.Do(req)
	if err != nil {
		return http.StatusBadGateway, fmt.Errorf("forward otlp: %w", err)
	}
	_ = resp.Body.Close()

	return resp.StatusCode, nil
}

func NewCollector(client *http.Client) *Collector {
	if client == nil {
		client = http.DefaultClient
	}

	c := &Collector{
		client: client,
		mux:    http.NewServeMux(),
	}

	c.maxRequestBytes.Store(4 << 20) // 4MiB
	c.mux.HandleFunc("/v1/traces", c.handleTraces)
	c.mux.HandleFunc("/v1/metrics", c.handleMetrics)
	c.mux.HandleFunc("/v1/logs", c.handleLogs)

	return c
}

func (c *Collector) Handler() http.Handler {
	if c == nil {
		return http.NewServeMux()
	}
	return c.mux
}

func (c *Collector) UpdateConfig(cfg Config) {
	if c == nil {
		return
	}

	if strings.TrimSpace(cfg.IngestionBaseURL) != "" {
		c.ingestionBaseURL.Store(strings.TrimRight(strings.TrimSpace(cfg.IngestionBaseURL), "/"))
	}
	if strings.TrimSpace(cfg.AuthToken) != "" {
		c.authToken.Store(strings.TrimSpace(cfg.AuthToken))
	}

	if cfg.MaxRequestBytes > 0 {
		c.maxRequestBytes.Store(cfg.MaxRequestBytes)
	}

	if cfg.TailDecisionWait > 0 {
		c.tailDecisionWait.Store(int64(cfg.TailDecisionWait))
	}
	if cfg.TailNumTraces > 0 {
		c.tailNumTraces.Store(int64(cfg.TailNumTraces))
	}
	if cfg.TailExpectedNewTracesPerSec > 0 {
		c.tailExpectedNewTracesPerSec.Store(int64(cfg.TailExpectedNewTracesPerSec))
	}

	c.triggerPolicyRefresh()
}

func (c *Collector) tailSamplerForCurrentConfig() *tailSampler {
	if c == nil {
		return nil
	}

	cfg := tailSamplingConfig{
		DecisionWait: time.Duration(c.tailDecisionWait.Load()),
		MaxTraces:    int(c.tailNumTraces.Load()),

		SampleRate:    0.05,
		SlowThreshold: 2 * time.Second,
	}
	if cfg.DecisionWait <= 0 || cfg.MaxTraces <= 0 {
		return nil
	}

	c.tailMu.Lock()
	defer c.tailMu.Unlock()

	if c.tailSampler == nil {
		c.tailSampler = newTailSampler(c, cfg)
		return c.tailSampler
	}

	c.tailSampler.UpdateConfig(cfg)
	return c.tailSampler
}

func (c *Collector) Endpoint() string {
	if c == nil {
		return ""
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.endpoint
}

func (c *Collector) Start() (string, error) {
	if c == nil {
		return "", errors.New("otlp collector is nil")
	}

	c.mu.Lock()
	defer c.mu.Unlock()

	if c.server != nil {
		return c.endpoint, nil
	}

	var lastErr error
	for range 5 {
		listener, err := net.Listen("tcp", "127.0.0.1:0")
		if err != nil {
			lastErr = err
			time.Sleep(50 * time.Millisecond)
			continue
		}

		addr := listener.Addr().(*net.TCPAddr)
		c.listener = listener
		c.endpoint = fmt.Sprintf("http://127.0.0.1:%d", addr.Port)

		c.server = &http.Server{
			Handler:           c.mux,
			ReadHeaderTimeout: 5 * time.Second,
		}
		server := c.server

		go func() {
			_ = server.Serve(listener)
		}()

		return c.endpoint, nil
	}

	if lastErr == nil {
		lastErr = errors.New("failed to bind telemetry receiver")
	}
	return "", lastErr
}

func (c *Collector) Shutdown(ctx context.Context) error {
	if c == nil {
		return nil
	}

	c.stopPolicyLoop(ctx)

	c.tailMu.Lock()
	sampler := c.tailSampler
	c.tailSampler = nil
	c.tailMu.Unlock()
	if sampler != nil {
		if ctx == nil {
			ctx = context.Background()
		}
		sampler.StopAndFlush(ctx)
	}

	c.mu.Lock()
	server := c.server
	listener := c.listener
	c.server = nil
	c.listener = nil
	c.endpoint = ""
	c.mu.Unlock()

	var err error
	if server != nil {
		err = server.Shutdown(ctx)
	}
	if listener != nil {
		_ = listener.Close()
	}
	return err
}

func (c *Collector) handleTraces(w http.ResponseWriter, r *http.Request) {
	c.handleOTLP(w, r, "traces")
}

func (c *Collector) handleMetrics(w http.ResponseWriter, r *http.Request) {
	c.handleOTLP(w, r, "metrics")
}

func (c *Collector) handleLogs(w http.ResponseWriter, r *http.Request) {
	c.handleOTLP(w, r, "logs")
}

func (c *Collector) handleOTLP(w http.ResponseWriter, r *http.Request, signal string) {
	if r.Method != http.MethodPost {
		w.WriteHeader(http.StatusMethodNotAllowed)
		return
	}

	limit := c.maxRequestBytes.Load()
	reader := io.LimitReader(r.Body, limit+1)
	body, err := io.ReadAll(reader)
	if err != nil {
		w.WriteHeader(http.StatusBadRequest)
		return
	}
	if int64(len(body)) > limit {
		w.WriteHeader(http.StatusRequestEntityTooLarge)
		return
	}

	status, _ := c.ForwardOTLP(
		r.Context(),
		signal,
		body,
		r.Header.Get("Content-Type"),
		r.Header.Get("Accept"),
		r.Header.Get("Content-Encoding"),
	)
	w.WriteHeader(status)
}
