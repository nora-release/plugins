package guesttelemetry

import (
	"testing"
	"time"

	colmetricv1 "go.opentelemetry.io/proto/otlp/collector/metrics/v1"
	metricsv1 "go.opentelemetry.io/proto/otlp/metrics/v1"
	"google.golang.org/protobuf/proto"
)

func TestMarshalHostMetricsBuildsExpectedMetrics(t *testing.T) {
	values := HostMetricsValues{
		Timestamp:            time.Unix(123, 456),
		CPUUtilization:       0.25,
		MemTotalBytes:        1024,
		MemUsedBytes:         512,
		DiskReadBytesPerSec:  10,
		DiskWriteBytesPerSec: 11,
		NetRxBytesPerSec:     12,
		NetTxBytesPerSec:     13,
	}

	data, err := MarshalHostMetrics(values)
	if err != nil {
		t.Fatalf("MarshalHostMetrics: %v", err)
	}

	var req colmetricv1.ExportMetricsServiceRequest
	if err := proto.Unmarshal(data, &req); err != nil {
		t.Fatalf("Unmarshal: %v", err)
	}

	if len(req.ResourceMetrics) != 1 {
		t.Fatalf("expected 1 resource metrics, got %d", len(req.ResourceMetrics))
	}

	resource := req.ResourceMetrics[0].Resource
	if resource == nil {
		t.Fatalf("expected resource")
	}

	attrs := map[string]string{}
	for _, kv := range resource.Attributes {
		if kv == nil || kv.Value == nil {
			continue
		}
		attrs[kv.Key] = kv.Value.GetStringValue()
	}
	if attrs["service.name"] != "guest-daemon" {
		t.Fatalf("unexpected service.name: %q", attrs["service.name"])
	}
	if attrs["os.type"] != "linux" {
		t.Fatalf("unexpected os.type: %q", attrs["os.type"])
	}

	gotCPU := requireGaugeDouble(t, req.ResourceMetrics[0], "guest.cpu.utilization")
	if gotCPU != values.CPUUtilization {
		t.Fatalf("unexpected cpu util: %v", gotCPU)
	}

	gotMemTotal := requireGaugeInt(t, req.ResourceMetrics[0], "guest.memory.total_bytes")
	if gotMemTotal != int64(values.MemTotalBytes) {
		t.Fatalf("unexpected mem total: %d", gotMemTotal)
	}

	gotMemUsed := requireGaugeInt(t, req.ResourceMetrics[0], "guest.memory.used_bytes")
	if gotMemUsed != int64(values.MemUsedBytes) {
		t.Fatalf("unexpected mem used: %d", gotMemUsed)
	}
}

func requireGaugeDouble(t *testing.T, rm *metricsv1.ResourceMetrics, name string) float64 {
	t.Helper()
	metric := findMetric(rm, name)
	if metric == nil {
		t.Fatalf("missing metric %s", name)
	}
	gauge := metric.GetGauge()
	if gauge == nil || len(gauge.DataPoints) != 1 {
		t.Fatalf("expected gauge datapoint for %s", name)
	}
	return gauge.DataPoints[0].GetAsDouble()
}

func requireGaugeInt(t *testing.T, rm *metricsv1.ResourceMetrics, name string) int64 {
	t.Helper()
	metric := findMetric(rm, name)
	if metric == nil {
		t.Fatalf("missing metric %s", name)
	}
	gauge := metric.GetGauge()
	if gauge == nil || len(gauge.DataPoints) != 1 {
		t.Fatalf("expected gauge datapoint for %s", name)
	}
	return gauge.DataPoints[0].GetAsInt()
}

func findMetric(rm *metricsv1.ResourceMetrics, name string) *metricsv1.Metric {
	for _, sm := range rm.ScopeMetrics {
		for _, m := range sm.Metrics {
			if m != nil && m.Name == name {
				return m
			}
		}
	}
	return nil
}
