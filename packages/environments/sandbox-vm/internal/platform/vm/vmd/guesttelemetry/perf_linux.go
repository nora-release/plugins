//go:build linux

package guesttelemetry

import (
	"bytes"
	"compress/gzip"
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"time"
)

func CapturePerfRecord(ctx context.Context, duration time.Duration) ([]byte, time.Time, time.Time, error) {
	if os.Getenv("CUE_ENABLE_PERF_PROFILE") != "1" {
		return nil, time.Time{}, time.Time{}, fmt.Errorf("perf profiling disabled")
	}

	if ctx == nil {
		ctx = context.Background()
	}
	if duration <= 0 {
		duration = 15 * time.Second
	}

	perfPath, err := exec.LookPath("perf")
	if err != nil {
		return nil, time.Time{}, time.Time{}, err
	}

	tmp, err := os.CreateTemp("", "perf-*.data")
	if err != nil {
		return nil, time.Time{}, time.Time{}, err
	}
	tmpPath := tmp.Name()
	_ = tmp.Close()
	defer os.Remove(tmpPath)

	seconds := strconv.FormatInt(int64(duration.Seconds()), 10)
	if seconds == "0" {
		seconds = "1"
	}

	start := time.Now()

	cmd := exec.CommandContext(ctx, perfPath, "record", "-F", "99", "-g", "--output", tmpPath, "--", "sleep", seconds)
	var stderr bytes.Buffer
	cmd.Stdout = io.Discard
	cmd.Stderr = &stderr

	if err := cmd.Run(); err != nil {
		return nil, time.Time{}, time.Time{}, fmt.Errorf("perf record: %w (%s)", err, strings.TrimSpace(stderr.String()))
	}

	raw, err := os.ReadFile(tmpPath)
	if err != nil {
		return nil, time.Time{}, time.Time{}, err
	}
	end := time.Now()

	var buf bytes.Buffer
	zw := gzip.NewWriter(&buf)
	if _, err := zw.Write(raw); err != nil {
		_ = zw.Close()
		return nil, time.Time{}, time.Time{}, err
	}
	if err := zw.Close(); err != nil {
		return nil, time.Time{}, time.Time{}, err
	}

	return buf.Bytes(), start, end, nil
}
