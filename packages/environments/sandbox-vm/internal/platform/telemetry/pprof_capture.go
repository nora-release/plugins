package telemetry

import (
	"bytes"
	"compress/gzip"
	"context"
	"errors"
	"fmt"
	"io"
	"runtime"
	"runtime/pprof"
	"time"
)

func CapturePprofCPU(ctx context.Context, duration time.Duration) ([]byte, time.Time, time.Time, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	if duration <= 0 {
		duration = 10 * time.Second
	}

	start := time.Now()
	var buf bytes.Buffer
	if err := pprof.StartCPUProfile(&buf); err != nil {
		return nil, time.Time{}, time.Time{}, err
	}

	timer := time.NewTimer(duration)
	select {
	case <-ctx.Done():
		timer.Stop()
	case <-timer.C:
	}
	pprof.StopCPUProfile()
	end := time.Now()

	data, err := ensureGzip(buf.Bytes())
	if err != nil {
		return nil, time.Time{}, time.Time{}, err
	}
	return data, start, end, nil
}

func CapturePprofSnapshot(profile string) ([]byte, time.Time, time.Time, error) {
	profile = normalizePprofProfile(profile)
	if profile == "" {
		return nil, time.Time{}, time.Time{}, errors.New("missing profile name")
	}

	start := time.Now()
	var buf bytes.Buffer

	var restore func()
	if profile == "mutex" {
		prev := runtime.SetMutexProfileFraction(1)
		restore = func() { runtime.SetMutexProfileFraction(prev) }
	}
	if profile == "block" {
		runtime.SetBlockProfileRate(1)
		restore = func() { runtime.SetBlockProfileRate(0) }
	}
	if restore != nil {
		defer restore()
	}

	p := pprof.Lookup(profile)
	if p == nil {
		return nil, time.Time{}, time.Time{}, fmt.Errorf("unknown profile: %s", profile)
	}
	if err := p.WriteTo(&buf, 0); err != nil {
		return nil, time.Time{}, time.Time{}, err
	}
	end := time.Now()

	data, err := ensureGzip(buf.Bytes())
	if err != nil {
		return nil, time.Time{}, time.Time{}, err
	}
	return data, start, end, nil
}

func normalizePprofProfile(profile string) string {
	switch profile {
	case "heap", "goroutine", "mutex", "block":
		return profile
	default:
		return profile
	}
}

func ensureGzip(data []byte) ([]byte, error) {
	if len(data) >= 2 && data[0] == 0x1f && data[1] == 0x8b {
		return data, nil
	}

	var buf bytes.Buffer
	zw := gzip.NewWriter(&buf)
	if _, err := io.Copy(zw, bytes.NewReader(data)); err != nil {
		_ = zw.Close()
		return nil, err
	}
	if err := zw.Close(); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}
