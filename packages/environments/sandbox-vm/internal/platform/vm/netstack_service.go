package vm

import (
	"context"
	"fmt"
	"log"
	"strings"

	"github.com/nora/sandbox-vm/internal/platform/vm/netstack"
	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
)

func (m *Manager) startNetworkDataPlane() error {
	listener, err := m.VsockListen(vmrpc.NetstackPort)
	if err != nil {
		return fmt.Errorf("listen on netstack vsock port %d: %w", vmrpc.NetstackPort, err)
	}

	runCtx, cancel := context.WithCancel(context.Background())
	m.netstackListener = listener
	m.netstackCancel = cancel
	cfg := netstack.DefaultConfig()

	go func() {
		for {
			conn, err := listener.Accept()
			if err != nil {
				if runCtx.Err() != nil || isClosedListenerError(err) {
					return
				}
				log.Printf("netstack accept failed: %v", err)
				continue
			}

			if err := netstack.Run(runCtx, conn, cfg); err != nil && runCtx.Err() == nil {
				log.Printf("netstack session ended with error: %v", err)
			}
		}
	}()

	log.Printf("User-mode netstack listening on vsock port %d", vmrpc.NetstackPort)
	return nil
}

func (m *Manager) stopNetworkDataPlaneLocked() {
	if m.netstackCancel != nil {
		m.netstackCancel()
		m.netstackCancel = nil
	}
	if m.netstackListener != nil {
		_ = m.netstackListener.Close()
		m.netstackListener = nil
	}
}

func isClosedListenerError(err error) bool {
	if err == nil {
		return false
	}
	if err == context.Canceled {
		return true
	}
	msg := strings.ToLower(err.Error())
	return strings.Contains(msg, "closed")
}
