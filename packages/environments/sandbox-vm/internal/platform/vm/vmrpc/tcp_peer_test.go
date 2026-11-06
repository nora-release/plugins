package vmrpc

import (
	"context"
	"net"
	"testing"
	"time"

	"google.golang.org/grpc"
	"google.golang.org/grpc/credentials/insecure"
)

func TestNewTCPHostPeer_CreatesDialer(t *testing.T) {
	peer := NewTCPHostPeer("localhost:0")
	if peer == nil {
		t.Fatal("expected non-nil peer")
	}
	if peer.dialer == nil {
		t.Fatal("expected dialer to be set")
	}
	if peer.grpcServer == nil {
		t.Fatal("expected grpc server to be set")
	}
}

func TestTCPHostPeer_ServeAsync_NoListener(t *testing.T) {
	peer := NewTCPHostPeer("localhost:0")

	// ServeAsync should be a no-op (not error) when there's no listener.
	if err := peer.ServeAsync(); err != nil {
		t.Fatalf("ServeAsync should succeed with no listener, got: %v", err)
	}
}

func TestTCPHostPeer_ConnectToServer(t *testing.T) {
	// Start a real gRPC server on a random port.
	listener, err := net.Listen("tcp", "localhost:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	addr := listener.Addr().String()

	srv := grpc.NewServer()
	RegisterVMServiceServer(srv, &testVMServer{})
	go srv.Serve(listener)
	defer srv.Stop()

	// Create a TCP peer pointing at the server.
	peer := NewTCPHostPeer(addr)

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	if err := peer.Connect(ctx); err != nil {
		t.Fatalf("Connect failed: %v", err)
	}
	defer peer.Close()

	// Verify we can get a client and call Health.
	client := peer.VMClient()
	if client == nil {
		t.Fatal("expected non-nil VMClient")
	}

	resp, err := client.Health(ctx, &HealthRequest{})
	if err != nil {
		t.Fatalf("Health RPC failed: %v", err)
	}
	if resp.GetStatus() != "ok" {
		t.Fatalf("expected status 'ok', got %q", resp.GetStatus())
	}
}

// testVMServer implements a minimal VMServiceServer for testing.
type testVMServer struct {
	UnimplementedVMServiceServer
}

func (s *testVMServer) Health(ctx context.Context, req *HealthRequest) (*HealthResponse, error) {
	return &HealthResponse{Status: "ok"}, nil
}

func TestPeer_ConnectWithDialer(t *testing.T) {
	// Start a real gRPC server.
	listener, err := net.Listen("tcp", "localhost:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	addr := listener.Addr().String()

	srv := grpc.NewServer()
	RegisterVMServiceServer(srv, &testVMServer{})
	go srv.Serve(listener)
	defer srv.Stop()

	// Create a Peer manually with a TCP dialer and no listener.
	peer := &Peer{
		grpcServer: grpc.NewServer(),
		dialer: func(ctx context.Context) (net.Conn, error) {
			var d net.Dialer
			return d.DialContext(ctx, "tcp", addr)
		},
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	// Connect with grpc.WithTransportCredentials since we're using custom dialer.
	peer.clientConn, err = grpc.DialContext(ctx, addr,
		grpc.WithTransportCredentials(insecure.NewCredentials()),
	)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	defer peer.Close()

	client := peer.VMClient()
	if client == nil {
		t.Fatal("expected non-nil VMClient")
	}

	resp, err := client.Health(ctx, &HealthRequest{})
	if err != nil {
		t.Fatalf("Health RPC failed: %v", err)
	}
	if resp.GetStatus() != "ok" {
		t.Fatalf("expected 'ok', got %q", resp.GetStatus())
	}
}
