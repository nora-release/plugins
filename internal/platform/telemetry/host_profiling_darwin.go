//go:build darwin

package telemetry

import (
	"bytes"
	"compress/gzip"
	"context"
	"encoding/json"
	"sort"
	"sync"
	"sync/atomic"
	"time"
)

type hostProfilingConfig struct {
	PollInterval            time.Duration
	HighCPUThreadPercent    float64
	HighCPUSustain          time.Duration
	Cooldown                time.Duration
	CaptureInterval         time.Duration
	CaptureWindow           time.Duration
	ThreadTopN              int
	PprofCPUProfileDuration time.Duration
	IncludePprofHeap        bool
	IncludePprofMutex       bool
	IncludePprofBlock       bool
	IncludePprofGoroutine   bool
}

type threadSampler interface {
	Sample() ([]hostThreadSample, error)
}

func defaultHostProfilingConfig() hostProfilingConfig {
	return hostProfilingConfig{
		PollInterval:            5 * time.Second,
		HighCPUThreadPercent:    90,
		HighCPUSustain:          10 * time.Second,
		Cooldown:                5 * time.Minute,
		CaptureInterval:         time.Second,
		CaptureWindow:           10 * time.Second,
		ThreadTopN:              20,
		PprofCPUProfileDuration: 10 * time.Second,
		IncludePprofHeap:        true,
		IncludePprofMutex:       true,
		IncludePprofBlock:       true,
		IncludePprofGoroutine:   true,
	}
}

type hostProfiler struct {
	collector *Collector
	uploader  *ArtifactUploader
	cfg       hostProfilingConfig
	sampler   threadSampler

	mu          sync.Mutex
	stopCh      chan struct{}
	doneCh      chan struct{}
	highSince   time.Time
	lastTrigger time.Time
	running     bool
	capturing   atomic.Bool
}

func newHostProfiler(collector *Collector) HostProfiler {
	return &hostProfiler{
		collector: collector,
		uploader:  NewArtifactUploader(collector),
		cfg:       defaultHostProfilingConfig(),
		sampler:   hostThreadSampler{},
	}
}

func (p *hostProfiler) Start() {
	if p == nil {
		return
	}

	p.mu.Lock()
	if p.running {
		p.mu.Unlock()
		return
	}
	p.running = true
	p.stopCh = make(chan struct{})
	p.doneCh = make(chan struct{})
	p.mu.Unlock()

	go p.loop()
}

func (p *hostProfiler) Stop() {
	if p == nil {
		return
	}

	p.mu.Lock()
	if !p.running {
		p.mu.Unlock()
		return
	}
	close(p.stopCh)
	done := p.doneCh
	p.running = false
	p.mu.Unlock()

	select {
	case <-done:
	case <-time.After(2 * time.Second):
	}
}

func (p *hostProfiler) loop() {
	defer func() {
		p.mu.Lock()
		if p.doneCh != nil {
			close(p.doneCh)
		}
		p.mu.Unlock()
	}()

	ticker := time.NewTicker(p.cfg.PollInterval)
	defer ticker.Stop()

	for {
		select {
		case <-p.stopCh:
			return
		case <-ticker.C:
			p.poll()
		}
	}
}

func (p *hostProfiler) poll() {
	if p == nil || p.collector == nil {
		return
	}

	if p.collector.IngestionBaseURL() == "" || p.collector.AuthToken() == "" {
		p.mu.Lock()
		p.highSince = time.Time{}
		p.mu.Unlock()
		return
	}

	samples, err := p.sampler.Sample()
	if err != nil {
		return
	}

	maxThreadCPU := 0.0
	for _, s := range samples {
		cpu := cpuPercentFromScaledUsage(s.CPUUsageScaled)
		if cpu > maxThreadCPU {
			maxThreadCPU = cpu
		}
	}

	now := time.Now()

	p.mu.Lock()
	defer p.mu.Unlock()

	if maxThreadCPU < p.cfg.HighCPUThreadPercent {
		p.highSince = time.Time{}
		return
	}

	if p.highSince.IsZero() {
		p.highSince = now
		return
	}

	if now.Sub(p.lastTrigger) < p.cfg.Cooldown {
		return
	}

	if now.Sub(p.highSince) < p.cfg.HighCPUSustain {
		return
	}

	if !p.capturing.CompareAndSwap(false, true) {
		return
	}

	p.lastTrigger = now
	p.highSince = time.Time{}

	go func() {
		defer p.capturing.Store(false)
		ctx, cancel := context.WithTimeout(context.Background(), p.cfg.CaptureWindow+30*time.Second)
		defer cancel()
		_ = p.captureAndUpload(ctx)
	}()
}

func (p *hostProfiler) captureAndUpload(ctx context.Context) error {
	if p == nil || p.collector == nil {
		return nil
	}

	var results []captureResult
	var mu sync.Mutex
	var wg sync.WaitGroup

	wg.Add(1)
	go func() {
		defer wg.Done()
		payload, start, end, err := captureHostThreadTopN(ctx, p.sampler, p.cfg.CaptureInterval, p.cfg.CaptureWindow, p.cfg.ThreadTopN)
		mu.Lock()
		results = append(results, captureResult{
			artifactType: ArtifactTypeHostThreadTop,
			filename:     "host_thread_top.jsonl.gz",
			payload:      payload,
			start:        start,
			end:          end,
			err:          err,
		})
		mu.Unlock()
	}()

	wg.Add(1)
	go func() {
		defer wg.Done()
		payload, start, end, err := CapturePprofCPU(ctx, p.cfg.PprofCPUProfileDuration)
		mu.Lock()
		results = append(results, captureResult{
			artifactType: ArtifactTypePprofCPU,
			filename:     "pprof_cpu.pprof.gz",
			payload:      payload,
			start:        start,
			end:          end,
			err:          err,
		})
		mu.Unlock()
	}()

	if p.cfg.IncludePprofHeap {
		wg.Add(1)
		go func() {
			defer wg.Done()
			payload, start, end, err := CapturePprofSnapshot("heap")
			mu.Lock()
			results = append(results, captureResult{
				artifactType: ArtifactTypePprofHeap,
				filename:     "pprof_heap.pprof.gz",
				payload:      payload,
				start:        start,
				end:          end,
				err:          err,
			})
			mu.Unlock()
		}()
	}

	if p.cfg.IncludePprofMutex {
		wg.Add(1)
		go func() {
			defer wg.Done()
			payload, start, end, err := CapturePprofSnapshot("mutex")
			mu.Lock()
			results = append(results, captureResult{
				artifactType: ArtifactTypePprofMutex,
				filename:     "pprof_mutex.pprof.gz",
				payload:      payload,
				start:        start,
				end:          end,
				err:          err,
			})
			mu.Unlock()
		}()
	}

	if p.cfg.IncludePprofBlock {
		wg.Add(1)
		go func() {
			defer wg.Done()
			payload, start, end, err := CapturePprofSnapshot("block")
			mu.Lock()
			results = append(results, captureResult{
				artifactType: ArtifactTypePprofBlock,
				filename:     "pprof_block.pprof.gz",
				payload:      payload,
				start:        start,
				end:          end,
				err:          err,
			})
			mu.Unlock()
		}()
	}

	if p.cfg.IncludePprofGoroutine {
		wg.Add(1)
		go func() {
			defer wg.Done()
			payload, start, end, err := CapturePprofSnapshot("goroutine")
			mu.Lock()
			results = append(results, captureResult{
				artifactType: ArtifactTypePprofGoroutine,
				filename:     "pprof_goroutine.pprof.gz",
				payload:      payload,
				start:        start,
				end:          end,
				err:          err,
			})
			mu.Unlock()
		}()
	}

	wg.Wait()

	var firstErr error
	for _, r := range results {
		if r.err != nil {
			if firstErr == nil {
				firstErr = r.err
			}
			continue
		}
		if len(r.payload) == 0 {
			continue
		}

		uploaded, err := p.uploader.Upload(ctx, ArtifactUploadRequest{
			Type:          r.artifactType,
			Filename:      r.filename,
			ContentType:   "application/gzip",
			Payload:       r.payload,
			StartTime:     r.start,
			EndTime:       r.end,
			TriggerReason: TriggerReasonHighCPU,
		})
		if err != nil {
			if firstErr == nil {
				firstErr = err
			}
			continue
		}
		_ = p.collector.ReportArtifactUploaded(ctx, uploaded)
	}

	return firstErr
}

type captureResult struct {
	artifactType ArtifactType
	filename     string
	payload      []byte
	start        time.Time
	end          time.Time
	err          error
}

type hostThreadTopEntry struct {
	ThreadID   uint64  `json:"thread_id"`
	Name       string  `json:"name,omitempty"`
	CPUPercent float64 `json:"cpu_percent"`
	UserUsec   uint64  `json:"user_time_usec"`
	SystemUsec uint64  `json:"system_time_usec"`
	RunState   int     `json:"run_state"`
}

type hostThreadTopSnapshot struct {
	Timestamp string               `json:"timestamp"`
	Threads   []hostThreadTopEntry `json:"threads"`
}

func captureHostThreadTopN(ctx context.Context, sampler threadSampler, interval time.Duration, window time.Duration, topN int) ([]byte, time.Time, time.Time, error) {
	if topN <= 0 {
		topN = 20
	}
	if interval <= 0 {
		interval = time.Second
	}
	if window <= 0 {
		window = 10 * time.Second
	}

	start := time.Now()
	deadline := start.Add(window)

	var buf bytes.Buffer
	gz := gzip.NewWriter(&buf)

	writeSnapshot := func(ts time.Time) error {
		samples, err := sampler.Sample()
		if err != nil {
			return err
		}

		sort.Slice(samples, func(i, j int) bool {
			return samples[i].CPUUsageScaled > samples[j].CPUUsageScaled
		})

		n := topN
		if len(samples) < n {
			n = len(samples)
		}

		entries := make([]hostThreadTopEntry, 0, n)
		for _, s := range samples[:n] {
			entries = append(entries, hostThreadTopEntry{
				ThreadID:   s.ThreadID,
				Name:       s.Name,
				CPUPercent: cpuPercentFromScaledUsage(s.CPUUsageScaled),
				UserUsec:   s.UserTimeUsec,
				SystemUsec: s.SystemTimeUsec,
				RunState:   s.RunState,
			})
		}

		snapshot := hostThreadTopSnapshot{
			Timestamp: ts.UTC().Format(time.RFC3339Nano),
			Threads:   entries,
		}

		line, err := json.Marshal(snapshot)
		if err != nil {
			return err
		}
		if _, err := gz.Write(line); err != nil {
			return err
		}
		if _, err := gz.Write([]byte("\n")); err != nil {
			return err
		}
		return nil
	}

	for {
		now := time.Now()
		if now.After(deadline) {
			break
		}

		if err := writeSnapshot(now); err != nil {
			return nil, time.Time{}, time.Time{}, err
		}

		sleep := time.Until(now.Add(interval))
		if sleep <= 0 {
			continue
		}

		timer := time.NewTimer(sleep)
		select {
		case <-ctx.Done():
			timer.Stop()
			return nil, time.Time{}, time.Time{}, ctx.Err()
		case <-timer.C:
		}
	}

	if err := gz.Close(); err != nil {
		return nil, time.Time{}, time.Time{}, err
	}

	end := time.Now()
	return buf.Bytes(), start, end, nil
}
