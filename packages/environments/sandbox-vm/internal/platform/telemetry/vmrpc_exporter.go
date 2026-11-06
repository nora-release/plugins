package telemetry

import (
	"context"
	"fmt"

	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
)

type VMRPCExporter struct {
	peer *vmrpc.Peer
}

func NewVMRPCExporter(peer *vmrpc.Peer) *VMRPCExporter {
	return &VMRPCExporter{peer: peer}
}

func (e *VMRPCExporter) Export(ctx context.Context, envelope *vmrpc.OtlpEnvelope) (*vmrpc.ExportAck, error) {
	if e == nil || e.peer == nil {
		return nil, fmt.Errorf("peer is nil")
	}
	if ctx == nil {
		ctx = context.Background()
	}

	if err := e.peer.Connect(ctx); err != nil {
		return nil, err
	}
	client := e.peer.TelemetryClient()
	if client == nil {
		return nil, fmt.Errorf("telemetry client is not available")
	}

	return client.ExportOTLP(ctx, envelope)
}
