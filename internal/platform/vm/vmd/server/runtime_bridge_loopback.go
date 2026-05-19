//go:build linux

package server

import (
	"fmt"
	"net"
	"net/http"
	"time"

	"github.com/nora/sandbox-vm/internal/envhost"
	"github.com/nora/sandbox-vm/internal/framework/runtimebridge"
)

type RuntimeBridgeLoopbackServer struct {
	listener net.Listener
	server   *http.Server
}

func StartRuntimeBridgeLoopbackServer(port uint32) (*RuntimeBridgeLoopbackServer, error) {
	if port == 0 {
		port = envhost.DefaultRuntimeBridgePort
	}

	listener, err := net.Listen("tcp", fmt.Sprintf("127.0.0.1:%d", port))
	if err != nil {
		return nil, fmt.Errorf("listen runtime bridge loopback on %d: %w", port, err)
	}

	server := &RuntimeBridgeLoopbackServer{
		listener: listener,
		server: &http.Server{
			Handler:           envhost.NewRuntimeBridgeHandler(newGuestRuntimeBridge()),
			ReadHeaderTimeout: 10 * time.Second,
		},
	}

	go func() {
		if err := server.server.Serve(listener); err != nil && err != http.ErrServerClosed {
			_ = listener.Close()
		}
	}()

	return server, nil
}

func (s *RuntimeBridgeLoopbackServer) Close() error {
	if s == nil || s.server == nil {
		return nil
	}
	return s.server.Close()
}

func newGuestRuntimeBridge() envhost.RuntimeBridge {
	return envhost.NewDirectRuntimeBridge(
		runtimebridge.UnsupportedToolHandler{},
		runtimebridge.UnsupportedHTTPHandler{},
	)
}
