package vmrpc

import (
	"context"
	"net"

	"google.golang.org/grpc"
)

// NewTCPHostPeer creates a host-side Peer that dials vmd over TCP.
// The peer is client-only — no listener is configured because the remote vmd
// does not need to call back into the host via gRPC (no HostService/TelemetryService).
func NewTCPHostPeer(addr string) *Peer {
	return &Peer{
		grpcServer: grpc.NewServer(
			grpc.MaxRecvMsgSize(maxMsgSize),
			grpc.MaxSendMsgSize(maxMsgSize),
			grpc.ReadBufferSize(maxMsgSize),
			grpc.WriteBufferSize(maxMsgSize),
			grpc.InitialWindowSize(maxMsgSize),
			grpc.InitialConnWindowSize(maxMsgSize),
		),
		dialer: func(ctx context.Context) (net.Conn, error) {
			var d net.Dialer
			return d.DialContext(ctx, "tcp", addr)
		},
	}
}
