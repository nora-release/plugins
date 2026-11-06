package telemetry

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

type telemetryPolicy struct {
	Enabled          bool                `json:"enabled"`
	ExpiresInSeconds int                 `json:"expires_in_seconds"`
	TailSampling     *tailSamplingPolicy `json:"tail_sampling"`
}

type tailSamplingPolicy struct {
	DecisionWaitSeconds        int `json:"decision_wait_seconds"`
	NumTraces                  int `json:"num_traces"`
	ExpectedNewTracesPerSecond int `json:"expected_new_traces_per_second"`
}

func (c *Collector) ensurePolicyLoopStarted() {
	if c == nil {
		return
	}

	c.policyMu.Lock()
	if c.policyRefreshCh != nil {
		c.policyMu.Unlock()
		return
	}

	refreshCh := make(chan struct{}, 1)
	stopCh := make(chan struct{})
	doneCh := make(chan struct{})

	c.policyRefreshCh = refreshCh
	c.policyStopCh = stopCh
	c.policyDoneCh = doneCh
	c.policyMu.Unlock()

	go c.policyLoop(refreshCh, stopCh, doneCh)
}

func (c *Collector) triggerPolicyRefresh() {
	if c == nil {
		return
	}

	c.ensurePolicyLoopStarted()

	c.policyMu.Lock()
	refreshCh := c.policyRefreshCh
	c.policyMu.Unlock()
	if refreshCh == nil {
		return
	}

	select {
	case refreshCh <- struct{}{}:
	default:
	}
}

func (c *Collector) stopPolicyLoop(ctx context.Context) {
	if c == nil {
		return
	}

	if ctx == nil {
		ctx = context.Background()
	}

	c.policyMu.Lock()
	stopCh := c.policyStopCh
	doneCh := c.policyDoneCh
	c.policyStopCh = nil
	c.policyRefreshCh = nil
	c.policyDoneCh = nil
	c.policyMu.Unlock()

	if stopCh == nil || doneCh == nil {
		return
	}

	close(stopCh)

	select {
	case <-doneCh:
	case <-ctx.Done():
	}
}

func (c *Collector) policyLoop(refreshCh <-chan struct{}, stopCh <-chan struct{}, doneCh chan struct{}) {
	defer close(doneCh)

	var timer *time.Timer
	var timerCh <-chan time.Time

	schedule := func(d time.Duration) {
		if d <= 0 {
			d = time.Minute
		}
		if d < 5*time.Second {
			d = 5 * time.Second
		}

		if timer == nil {
			timer = time.NewTimer(d)
			timerCh = timer.C
			return
		}
		if !timer.Stop() {
			select {
			case <-timer.C:
			default:
			}
		}
		timer.Reset(d)
		timerCh = timer.C
	}

	refresh := func(etag string) string {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()

		ttl, nextETag, err := c.fetchAndApplyPolicy(ctx, etag)
		if err != nil {
			schedule(30 * time.Second)
			return etag
		}

		if ttl > 0 {
			schedule(ttl / 2)
		}
		if nextETag != "" {
			return nextETag
		}
		return etag
	}

	var etag string
	for {
		select {
		case <-stopCh:
			if timer != nil {
				timer.Stop()
			}
			return
		case <-refreshCh:
			etag = refresh(etag)
		case <-timerCh:
			etag = refresh(etag)
		}
	}
}

func (c *Collector) fetchAndApplyPolicy(ctx context.Context, etag string) (time.Duration, string, error) {
	if c == nil {
		return 0, "", fmt.Errorf("otlp collector is nil")
	}

	baseURL := c.IngestionBaseURL()
	token := c.AuthToken()
	if baseURL == "" || token == "" {
		return 0, "", nil
	}

	telemetryBaseURL, err := deriveTelemetryBaseURL(baseURL)
	if err != nil {
		return 0, "", nil
	}

	policyURL := strings.TrimRight(telemetryBaseURL, "/") + "/policy"
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, policyURL, nil)
	if err != nil {
		return 0, "", fmt.Errorf("create policy request: %w", err)
	}
	req.Header.Set("Accept", "application/json")
	req.Header.Set("Authorization", "Bearer "+token)
	if strings.TrimSpace(etag) != "" {
		req.Header.Set("If-None-Match", strings.TrimSpace(etag))
	}

	resp, err := c.client.Do(req)
	if err != nil {
		return 0, "", fmt.Errorf("fetch policy: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusNotModified {
		return 0, etag, nil
	}
	if resp.StatusCode < 200 || resp.StatusCode >= 300 {
		return 0, "", fmt.Errorf("fetch policy: status=%d", resp.StatusCode)
	}

	body, err := io.ReadAll(io.LimitReader(resp.Body, 256*1024))
	if err != nil {
		return 0, "", fmt.Errorf("read policy response: %w", err)
	}

	var policy telemetryPolicy
	if err := json.Unmarshal(body, &policy); err != nil {
		return 0, "", fmt.Errorf("decode policy response: %w", err)
	}

	c.applyTailSamplingPolicy(&policy)

	ttl := time.Duration(policy.ExpiresInSeconds) * time.Second
	nextETag := strings.TrimSpace(resp.Header.Get("ETag"))
	return ttl, nextETag, nil
}

func (c *Collector) applyTailSamplingPolicy(policy *telemetryPolicy) {
	if c == nil {
		return
	}

	if policy == nil || !policy.Enabled || policy.TailSampling == nil {
		c.disableTailSampling()
		return
	}

	decisionWait := time.Duration(policy.TailSampling.DecisionWaitSeconds) * time.Second
	numTraces := policy.TailSampling.NumTraces
	if decisionWait <= 0 || numTraces <= 0 {
		c.disableTailSampling()
		return
	}

	c.tailDecisionWait.Store(int64(decisionWait))
	c.tailNumTraces.Store(int64(numTraces))
	c.tailExpectedNewTracesPerSec.Store(int64(policy.TailSampling.ExpectedNewTracesPerSecond))

	_ = c.tailSamplerForCurrentConfig()
}

func (c *Collector) disableTailSampling() {
	if c == nil {
		return
	}

	c.tailDecisionWait.Store(0)
	c.tailNumTraces.Store(0)
	c.tailExpectedNewTracesPerSec.Store(0)

	c.tailMu.Lock()
	sampler := c.tailSampler
	c.tailSampler = nil
	c.tailMu.Unlock()

	if sampler == nil {
		return
	}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
	defer cancel()
	sampler.StopAndFlush(ctx)
}
