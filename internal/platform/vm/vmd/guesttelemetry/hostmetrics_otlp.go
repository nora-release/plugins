package guesttelemetry

import (
	"time"

	"github.com/nora/sandbox-vm/internal/platform/otlp"
	colmetricv1 "go.opentelemetry.io/proto/otlp/collector/metrics/v1"
	commonv1 "go.opentelemetry.io/proto/otlp/common/v1"
	metricsv1 "go.opentelemetry.io/proto/otlp/metrics/v1"
	resourcev1 "go.opentelemetry.io/proto/otlp/resource/v1"
	"google.golang.org/protobuf/proto"
)

type HostMetricsValues struct {
	Timestamp time.Time

	CPUUtilization float64

	MemTotalBytes uint64
	MemUsedBytes  uint64

	DiskReadBytesPerSec  float64
	DiskWriteBytesPerSec float64
	NetRxBytesPerSec     float64
	NetTxBytesPerSec     float64
}

func MarshalHostMetrics(values HostMetricsValues) ([]byte, error) {
	return proto.Marshal(buildHostMetricsRequest(values))
}

func buildHostMetricsRequest(values HostMetricsValues) *colmetricv1.ExportMetricsServiceRequest {
	now := uint64(values.Timestamp.UnixNano())
	resource := &resourcev1.Resource{
		Attributes: []*commonv1.KeyValue{
			otlp.KVString("service.name", "guest-daemon"),
			otlp.KVString("os.type", "linux"),
		},
	}

	scope := &commonv1.InstrumentationScope{
		Name:    "nora.guesttelemetry",
		Version: "phase1",
	}

	metrics := []*metricsv1.Metric{
		gaugeDouble("guest.cpu.utilization", "Guest CPU utilization", "1", values.CPUUtilization, now),
		gaugeInt("guest.memory.total_bytes", "Guest total memory", "By", int64(values.MemTotalBytes), now),
		gaugeInt("guest.memory.used_bytes", "Guest used memory", "By", int64(values.MemUsedBytes), now),
		gaugeDouble("guest.disk.read_bytes_per_sec", "Guest disk read throughput", "By/s", values.DiskReadBytesPerSec, now),
		gaugeDouble("guest.disk.write_bytes_per_sec", "Guest disk write throughput", "By/s", values.DiskWriteBytesPerSec, now),
		gaugeDouble("guest.network.rx_bytes_per_sec", "Guest network receive throughput", "By/s", values.NetRxBytesPerSec, now),
		gaugeDouble("guest.network.tx_bytes_per_sec", "Guest network transmit throughput", "By/s", values.NetTxBytesPerSec, now),
	}

	return &colmetricv1.ExportMetricsServiceRequest{
		ResourceMetrics: []*metricsv1.ResourceMetrics{{
			Resource: resource,
			ScopeMetrics: []*metricsv1.ScopeMetrics{{
				Scope:   scope,
				Metrics: metrics,
			}},
		}},
	}
}

func gaugeDouble(name, description, unit string, value float64, ts uint64) *metricsv1.Metric {
	return &metricsv1.Metric{
		Name:        name,
		Description: description,
		Unit:        unit,
		Data: &metricsv1.Metric_Gauge{
			Gauge: &metricsv1.Gauge{
				DataPoints: []*metricsv1.NumberDataPoint{{
					TimeUnixNano:      ts,
					StartTimeUnixNano: ts,
					Value:             &metricsv1.NumberDataPoint_AsDouble{AsDouble: value},
				}},
			},
		},
	}
}

func gaugeInt(name, description, unit string, value int64, ts uint64) *metricsv1.Metric {
	return &metricsv1.Metric{
		Name:        name,
		Description: description,
		Unit:        unit,
		Data: &metricsv1.Metric_Gauge{
			Gauge: &metricsv1.Gauge{
				DataPoints: []*metricsv1.NumberDataPoint{{
					TimeUnixNano:      ts,
					StartTimeUnixNano: ts,
					Value:             &metricsv1.NumberDataPoint_AsInt{AsInt: value},
				}},
			},
		},
	}
}
