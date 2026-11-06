//go:build linux

package guesttelemetry

import (
	"bytes"
	"compress/gzip"
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"
)

type processSnapshot struct {
	timestamp time.Time
	totalCPU  uint64
	procs     map[int]processStat
}

type processTopEntry struct {
	PID        int     `json:"pid"`
	Comm       string  `json:"comm"`
	CPUPercent float64 `json:"cpu_percent"`
	DeltaTicks uint64  `json:"delta_ticks"`
}

type processTopSnapshot struct {
	Timestamp string            `json:"timestamp"`
	Processes []processTopEntry `json:"processes"`
}

func CaptureProcessTopN(ctx context.Context, window time.Duration, interval time.Duration, topN int) ([]byte, time.Time, time.Time, error) {
	if ctx == nil {
		ctx = context.Background()
	}
	if window <= 0 {
		window = 10 * time.Second
	}
	if interval <= 0 {
		interval = time.Second
	}
	if topN <= 0 {
		topN = 20
	}

	start := time.Now()
	deadline := start.Add(window)

	prev, err := collectProcessSnapshot()
	if err != nil {
		return nil, time.Time{}, time.Time{}, err
	}

	var buf bytes.Buffer
	gz := gzip.NewWriter(&buf)

	for {
		now := time.Now()
		if now.After(deadline) {
			break
		}

		timer := time.NewTimer(interval)
		select {
		case <-ctx.Done():
			timer.Stop()
			_ = gz.Close()
			return nil, time.Time{}, time.Time{}, ctx.Err()
		case <-timer.C:
		}

		next, err := collectProcessSnapshot()
		if err != nil {
			_ = gz.Close()
			return nil, time.Time{}, time.Time{}, err
		}

		entries := computeTopProcessDeltas(prev, next, topN)
		line, err := json.Marshal(processTopSnapshot{
			Timestamp: next.timestamp.UTC().Format(time.RFC3339Nano),
			Processes: entries,
		})
		if err != nil {
			_ = gz.Close()
			return nil, time.Time{}, time.Time{}, err
		}
		if _, err := gz.Write(line); err != nil {
			_ = gz.Close()
			return nil, time.Time{}, time.Time{}, err
		}
		if _, err := gz.Write([]byte("\n")); err != nil {
			_ = gz.Close()
			return nil, time.Time{}, time.Time{}, err
		}

		prev = next
	}

	if err := gz.Close(); err != nil {
		return nil, time.Time{}, time.Time{}, err
	}

	end := time.Now()
	return buf.Bytes(), start, end, nil
}

func collectProcessSnapshot() (processSnapshot, error) {
	now := time.Now()

	cpuData, err := os.ReadFile("/proc/stat")
	if err != nil {
		return processSnapshot{}, err
	}
	total, _, err := parseCPUStat(string(cpuData))
	if err != nil {
		return processSnapshot{}, err
	}

	entries, err := os.ReadDir("/proc")
	if err != nil {
		return processSnapshot{}, err
	}

	procs := make(map[int]processStat)
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		pid, err := strconv.Atoi(entry.Name())
		if err != nil || pid <= 0 {
			continue
		}

		statPath := filepath.Join("/proc", entry.Name(), "stat")
		line, err := os.ReadFile(statPath)
		if err != nil {
			continue
		}

		stat, err := parseProcStatLine(string(line))
		if err != nil {
			continue
		}
		procs[pid] = stat
	}

	return processSnapshot{
		timestamp: now,
		totalCPU:  total,
		procs:     procs,
	}, nil
}

func computeTopProcessDeltas(prev processSnapshot, next processSnapshot, topN int) []processTopEntry {
	if topN <= 0 {
		topN = 20
	}

	deltaTotal := uint64(0)
	if next.totalCPU > prev.totalCPU {
		deltaTotal = next.totalCPU - prev.totalCPU
	}
	if deltaTotal == 0 {
		return nil
	}

	type candidate struct {
		pid        int
		comm       string
		deltaTicks uint64
		cpuPercent float64
	}

	candidates := make([]candidate, 0, len(next.procs))
	for pid, nextStat := range next.procs {
		prevStat, ok := prev.procs[pid]
		if !ok {
			continue
		}
		prevTicks := prevStat.UTime + prevStat.STime
		nextTicks := nextStat.UTime + nextStat.STime
		if nextTicks <= prevTicks {
			continue
		}
		delta := nextTicks - prevTicks
		candidates = append(candidates, candidate{
			pid:        pid,
			comm:       strings.TrimSpace(nextStat.Comm),
			deltaTicks: delta,
			cpuPercent: float64(delta) * 100.0 / float64(deltaTotal),
		})
	}

	sort.Slice(candidates, func(i, j int) bool {
		return candidates[i].cpuPercent > candidates[j].cpuPercent
	})

	if len(candidates) > topN {
		candidates = candidates[:topN]
	}

	out := make([]processTopEntry, 0, len(candidates))
	for _, c := range candidates {
		out = append(out, processTopEntry{
			PID:        c.pid,
			Comm:       c.comm,
			CPUPercent: c.cpuPercent,
			DeltaTicks: c.deltaTicks,
		})
	}
	return out
}
