// Package vmrpc provides bidirectional gRPC communication between Host and VM.
//
// Host side:
//
//	peer := vmrpc.NewPeer(vmrpc.PeerConfig{
//	    Listener: vzListener,
//	    Dialer: func(ctx) (net.Conn, error) { return vsockConnect(VMServicePort) },
//	})
//	peer.RegisterHostService(hostServiceImpl)
//	peer.ServeAsync()
//	peer.VMClient().CreateSession(ctx, req)
//
// VM side:
//
//	peer := vmrpc.NewVMPeer()
//	peer.RegisterVMService(vmServiceImpl)
//	peer.ServeAsync()
//	peer.CallTool(ctx, "bash", args)
package vmrpc

import (
	"context"
	"fmt"
	"log"
	"net"
	"sync"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

// Port constants for vsock communication
const (
	VMServicePort   = uint32(50051) // vmd listens, Host connects
	HostServicePort = uint32(50052) // Host listens, vmd connects
	NetstackPort    = uint32(50090) // Host listens, guest netd connects
)

// maxMsgSize is the maximum gRPC message size (64MB for local IPC)
const maxMsgSize = 64 * 1024 * 1024

// Peer represents one side of the Host-VM communication.
// A Peer can be both a server (being called) and a client (calling remote).
type Peer struct {
	grpcServer *grpc.Server
	listener   net.Listener
	clientConn *grpc.ClientConn
	dialer     func(ctx context.Context) (net.Conn, error)
	mu         sync.Mutex
	closed     bool
}

// NewHostPeer creates a Peer for the Host side.
// listener: vsock listener for vmd to connect (HostService)
// dialer: function to connect to vmd (VMService)
func NewHostPeer(listener net.Listener, dialer func(ctx context.Context) (net.Conn, error)) *Peer {
	return &Peer{
		grpcServer: grpc.NewServer(
			grpc.MaxRecvMsgSize(maxMsgSize),
			grpc.MaxSendMsgSize(maxMsgSize),
			grpc.ReadBufferSize(maxMsgSize),
			grpc.WriteBufferSize(maxMsgSize),
			grpc.InitialWindowSize(maxMsgSize),
			grpc.InitialConnWindowSize(maxMsgSize),
		),
		listener: listener,
		dialer:   dialer,
	}
}

// RegisterHostService registers the HostService implementation (Host side).
func (p *Peer) RegisterHostService(impl HostServiceServer) {
	RegisterHostServiceServer(p.grpcServer, impl)
}

// RegisterTelemetryService registers the TelemetryService implementation (Host side).
func (p *Peer) RegisterTelemetryService(impl TelemetryServiceServer) {
	RegisterTelemetryServiceServer(p.grpcServer, impl)
}

// RegisterVMService registers the VMService implementation (vmd side).
func (p *Peer) RegisterVMService(impl VMServiceServer) {
	RegisterVMServiceServer(p.grpcServer, impl)
}

// ServeAsync starts the gRPC server in a goroutine.
// If no listener is configured (e.g. client-only TCP peer), this is a no-op.
func (p *Peer) ServeAsync() error {
	p.mu.Lock()
	if p.listener == nil {
		p.mu.Unlock()
		return nil
	}
	p.mu.Unlock()

	go func() {
		log.Printf("[vmrpc] Serving on %s", p.listener.Addr())
		if err := p.grpcServer.Serve(p.listener); err != nil {
			log.Printf("[vmrpc] Server error: %v", err)
		}
	}()
	return nil
}

// Connect establishes the client connection.
func (p *Peer) Connect(ctx context.Context) error {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.connectLocked(ctx)
}

func (p *Peer) connectLocked(ctx context.Context) error {
	if p.clientConn != nil {
		return nil
	}
	if p.dialer == nil {
		return fmt.Errorf("no dialer configured")
	}

	conn, err := grpc.NewClient(
		"passthrough:///peer",
		grpc.WithContextDialer(func(ctx context.Context, addr string) (net.Conn, error) {
			return p.dialer(ctx)
		}),
		grpc.WithTransportCredentials(insecure.NewCredentials()),
		grpc.WithDefaultCallOptions(
			grpc.MaxCallRecvMsgSize(maxMsgSize),
			grpc.MaxCallSendMsgSize(maxMsgSize),
		),
		grpc.WithReadBufferSize(maxMsgSize),
		grpc.WithWriteBufferSize(maxMsgSize),
		grpc.WithInitialWindowSize(int32(maxMsgSize)),
		grpc.WithInitialConnWindowSize(int32(maxMsgSize)),
	)
	if err != nil {
		return fmt.Errorf("connect: %w", err)
	}
	p.clientConn = conn
	return nil
}

// IsConnected returns true if the client is connected.
func (p *Peer) IsConnected() bool {
	p.mu.Lock()
	defer p.mu.Unlock()
	return p.clientConn != nil
}

// ResetClient closes and clears the client connection, allowing reconnection.
func (p *Peer) ResetClient() {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.clientConn != nil {
		p.clientConn.Close()
		p.clientConn = nil
	}
}

// VMClient returns the VMServiceClient for calling vmd.
// Automatically connects if not already connected.
func (p *Peer) VMClient() VMServiceClient {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.clientConn == nil {
		return nil
	}
	return NewVMServiceClient(p.clientConn)
}

// HostClient returns the HostServiceClient for calling host runtime bridge methods.
func (p *Peer) HostClient() HostServiceClient {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.clientConn == nil {
		return nil
	}
	return NewHostServiceClient(p.clientConn)
}

// TelemetryClient returns the TelemetryServiceClient for calling Host telemetry methods.
func (p *Peer) TelemetryClient() TelemetryServiceClient {
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.clientConn == nil {
		return nil
	}
	return NewTelemetryServiceClient(p.clientConn)
}

// Close shuts down the peer.
func (p *Peer) Close() error {
	p.mu.Lock()
	defer p.mu.Unlock()

	if p.closed {
		return nil
	}
	p.closed = true

	if p.grpcServer != nil {
		p.grpcServer.Stop()
	}
	if p.clientConn != nil {
		p.clientConn.Close()
		p.clientConn = nil
	}
	return nil
}
