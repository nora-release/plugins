//go:build !darwin

package telemetry

type noopHostProfiler struct{}

func newHostProfiler(_ *Collector) HostProfiler { return noopHostProfiler{} }

func (noopHostProfiler) Start() {}
func (noopHostProfiler) Stop()  {}
