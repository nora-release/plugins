//go:build linux

package server

import (
	"net"

	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
)

// globalPeer is the single Peer for VM-Host bidirectional communication.
// Initialized by InitGlobalPeer, accessed via GetPeer.
var globalPeer *vmrpc.Peer

// InitGlobalPeer initializes the global Peer with VMService over vsock.
// Must be called once during vmd startup.
func InitGlobalPeer(vmService vmrpc.VMServiceServer) error {
	peer, err := vmrpc.NewVMPeer()
	if err != nil {
		return err
	}
	peer.RegisterVMService(vmService)
	if err := peer.ServeAsync(); err != nil {
		return err
	}
	globalPeer = peer
	return nil
}

// InitGlobalPeerTCP initializes the global Peer with VMService listening on a TCP address.
// Used in remote mode where vsock is not available.
func InitGlobalPeerTCP(vmService vmrpc.VMServiceServer, listenAddr string) error {
	listener, err := net.Listen("tcp", listenAddr)
	if err != nil {
		return err
	}

	peer := vmrpc.NewHostPeer(listener, nil)
	peer.RegisterVMService(vmService)
	if err := peer.ServeAsync(); err != nil {
		listener.Close()
		return err
	}
	globalPeer = peer
	return nil
}

// GetPeer returns the global Peer.
func GetPeer() *vmrpc.Peer {
	return globalPeer
}
