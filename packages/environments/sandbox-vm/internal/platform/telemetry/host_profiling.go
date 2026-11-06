package telemetry

type HostProfiler interface {
	Start()
	Stop()
}

func NewHostProfiler(collector *Collector) HostProfiler {
	return newHostProfiler(collector)
}
