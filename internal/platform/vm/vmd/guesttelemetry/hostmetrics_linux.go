//go:build linux

package guesttelemetry

import (
	"context"
	"log"
	"os"
	"sync/atomic"
	"time"

	hosttelemetry "github.com/nora/sandbox-vm/internal/platform/telemetry"
	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
)

type hostMetricsSnapshot struct {
	timestamp time.Time

	cpuTotal uint64
	cpuIdle  uint64

	memTotalBytes     uint64
	memAvailableBytes uint64

	diskReadBytes  uint64
	diskWriteBytes uint64
	netRxBytes     uint64
	netTxBytes     uint64
}

func CollectHostMetrics() (hostMetricsSnapshot, error) {
	now := time.Now()

	cpuData, err := os.ReadFile("/proc/stat")
	if err != nil {
		return hostMetricsSnapshot{}, err
	}
	cpuTotal, cpuIdle, err := parseCPUStat(string(cpuData))
	if err != nil {
		return hostMetricsSnapshot{}, err
	}

	memData, err := os.ReadFile("/proc/meminfo")
	if err != nil {
		return hostMetricsSnapshot{}, err
	}
	memTotal, memAvailable, err := parseMemInfo(string(memData))
	if err != nil {
		return hostMetricsSnapshot{}, err
	}

	netData, err := os.ReadFile("/proc/net/dev")
	if err != nil {
		return hostMetricsSnapshot{}, err
	}
	netRx, netTx, err := parseNetDev(string(netData))
	if err != nil {
		return hostMetricsSnapshot{}, err
	}

	diskData, err := os.ReadFile("/proc/diskstats")
	if err != nil {
		return hostMetricsSnapshot{}, err
	}
	diskRead, diskWrite, err := parseDiskStats(string(diskData))
	if err != nil {
		return hostMetricsSnapshot{}, err
	}

	return hostMetricsSnapshot{
		timestamp: now,

		cpuTotal: cpuTotal,
		cpuIdle:  cpuIdle,

		memTotalBytes:     memTotal,
		memAvailableBytes: memAvailable,

		diskReadBytes:  diskRead,
		diskWriteBytes: diskWrite,
		netRxBytes:     netRx,
		netTxBytes:     netTx,
	}, nil
}

func (p hostMetricsSnapshot) ComputeValues(next hostMetricsSnapshot) HostMetricsValues {
	dt := next.timestamp.Sub(p.timestamp).Seconds()
	if dt <= 0 {
		dt = 1
	}

	cpuUtilization := 0.0
	if next.cpuTotal > p.cpuTotal {
		deltaTotal := float64(next.cpuTotal - p.cpuTotal)
		deltaIdle := float64(next.cpuIdle - p.cpuIdle)
		if deltaTotal > 0 {
			cpuUtilization = (deltaTotal - deltaIdle) / deltaTotal
		}
	}

	memUsed := uint64(0)
	if next.memTotalBytes > next.memAvailableBytes {
		memUsed = next.memTotalBytes - next.memAvailableBytes
	}

	diskReadRate := float64(next.diskReadBytes-p.diskReadBytes) / dt
	diskWriteRate := float64(next.diskWriteBytes-p.diskWriteBytes) / dt
	netRxRate := float64(next.netRxBytes-p.netRxBytes) / dt
	netTxRate := float64(next.netTxBytes-p.netTxBytes) / dt

	return HostMetricsValues{
		Timestamp: next.timestamp,

		CPUUtilization: cpuUtilization,

		MemTotalBytes: next.memTotalBytes,
		MemUsedBytes:  memUsed,

		DiskReadBytesPerSec:  diskReadRate,
		DiskWriteBytesPerSec: diskWriteRate,
		NetRxBytesPerSec:     netRxRate,
		NetTxBytesPerSec:     netTxRate,
	}
}

func RunHostMetricsExporter(peer *vmrpc.Peer, interval time.Duration) {
	if peer == nil {
		return
	}

	ticker := time.NewTicker(interval)
	defer ticker.Stop()

	var previous *hostMetricsSnapshot
	var lastLog time.Time

	var highSince time.Time
	var lastTrigger time.Time
	var capturing atomic.Bool

	for {
		snapshot, err := CollectHostMetrics()
		if err != nil {
			log.Printf("[telemetry] hostmetrics collect failed: %v", err)
			<-ticker.C
			continue
		}

		if previous == nil {
			previous = &snapshot
			<-ticker.C
			continue
		}

		values := previous.ComputeValues(snapshot)
		previous = &snapshot

		payload, err := MarshalHostMetrics(values)
		if err != nil {
			log.Printf("[telemetry] hostmetrics marshal failed: %v", err)
			continue
		}

		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		ack, err := hosttelemetry.NewVMRPCExporter(peer).Export(ctx, &vmrpc.OtlpEnvelope{
			Payload:     payload,
			Signal:      vmrpc.TelemetrySignal_TELEMETRY_SIGNAL_METRICS,
			Compression: vmrpc.TelemetryCompression_TELEMETRY_COMPRESSION_NONE,
			Attributes:  map[string]string{"telemetry.source": "guest"},
		})
		cancel()

		if err != nil {
			if time.Since(lastLog) > time.Minute {
				lastLog = time.Now()
				log.Printf("[telemetry] hostmetrics export failed: %v", err)
			}
			continue
		}

		if ack != nil && (ack.StatusCode < 200 || ack.StatusCode >= 300) && time.Since(lastLog) > time.Minute {
			lastLog = time.Now()
			log.Printf("[telemetry] hostmetrics export non-2xx: status=%d err=%s", ack.StatusCode, ack.Error)
		}

		// Trigger profiling artifacts when guest CPU stays high. Upload is best-effort and routed via Host.
		const highCPUThreshold = 0.90
		const cooldown = 5 * time.Minute
		sustain := 2 * interval
		if sustain < 10*time.Second {
			sustain = 10 * time.Second
		}

		now := values.Timestamp
		if values.CPUUtilization >= highCPUThreshold {
			if highSince.IsZero() {
				highSince = now
			}
		} else {
			highSince = time.Time{}
		}

		if !highSince.IsZero() && now.Sub(highSince) >= sustain && now.Sub(lastTrigger) >= cooldown {
			if capturing.CompareAndSwap(false, true) {
				lastTrigger = now
				highSince = time.Time{}
				go func() {
					defer capturing.Store(false)
					captureAndExportProfileArtifacts(peer)
				}()
			}
		}

		<-ticker.C
	}
}

func captureAndExportProfileArtifacts(peer *vmrpc.Peer) {
	if peer == nil {
		return
	}

	ctx, cancel := context.WithTimeout(context.Background(), 2*time.Minute)
	defer cancel()

	captureAndSend := func(artifactType string, filename string, payload []byte, start time.Time, end time.Time) {
		if len(payload) == 0 {
			return
		}

		attrs := map[string]string{
			"artifact.type":       artifactType,
			"artifact.filename":   filename,
			"artifact.start_time": start.UTC().Format(time.RFC3339Nano),
			"artifact.end_time":   end.UTC().Format(time.RFC3339Nano),
			"trigger.reason":      string(hosttelemetry.TriggerReasonHighCPU),
			"telemetry.source":    "guest",
		}

		ack, err := hosttelemetry.NewVMRPCExporter(peer).Export(ctx, &vmrpc.OtlpEnvelope{
			Payload:     payload,
			Signal:      vmrpc.TelemetrySignal_TELEMETRY_SIGNAL_PROFILE_ARTIFACT,
			Compression: vmrpc.TelemetryCompression_TELEMETRY_COMPRESSION_NONE,
			Attributes:  attrs,
		})
		if err != nil {
			log.Printf("[telemetry] profile artifact export failed: %v", err)
			return
		}
		if ack != nil && (ack.StatusCode < 200 || ack.StatusCode >= 300) {
			log.Printf("[telemetry] profile artifact export non-2xx: status=%d err=%s", ack.StatusCode, ack.Error)
		}
	}

	procPayload, procStart, procEnd, err := CaptureProcessTopN(ctx, 10*time.Second, time.Second, 20)
	if err == nil {
		captureAndSend(string(hosttelemetry.ArtifactTypeGuestProcessTop), "guest_process_top.jsonl.gz", procPayload, procStart, procEnd)
	}

	pprofCPU, pprofStart, pprofEnd, err := hosttelemetry.CapturePprofCPU(ctx, 10*time.Second)
	if err == nil {
		captureAndSend(string(hosttelemetry.ArtifactTypePprofCPU), "pprof_cpu.pprof.gz", pprofCPU, pprofStart, pprofEnd)
	}

	if heap, start, end, err := hosttelemetry.CapturePprofSnapshot("heap"); err == nil {
		captureAndSend(string(hosttelemetry.ArtifactTypePprofHeap), "pprof_heap.pprof.gz", heap, start, end)
	}
	if mutex, start, end, err := hosttelemetry.CapturePprofSnapshot("mutex"); err == nil {
		captureAndSend(string(hosttelemetry.ArtifactTypePprofMutex), "pprof_mutex.pprof.gz", mutex, start, end)
	}
	if block, start, end, err := hosttelemetry.CapturePprofSnapshot("block"); err == nil {
		captureAndSend(string(hosttelemetry.ArtifactTypePprofBlock), "pprof_block.pprof.gz", block, start, end)
	}
	if goroutine, start, end, err := hosttelemetry.CapturePprofSnapshot("goroutine"); err == nil {
		captureAndSend(string(hosttelemetry.ArtifactTypePprofGoroutine), "pprof_goroutine.pprof.gz", goroutine, start, end)
	}

	// Optional: perf record (requires perf in image and kernel support).
	if perfPayload, perfStart, perfEnd, err := CapturePerfRecord(ctx, 15*time.Second); err == nil {
		captureAndSend(string(hosttelemetry.ArtifactTypePerf), "perf.data.gz", perfPayload, perfStart, perfEnd)
	}
}
