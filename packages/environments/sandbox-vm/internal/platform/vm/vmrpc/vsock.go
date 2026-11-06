//go:build linux

package vmrpc

import (
	"fmt"
	"io"
	"net"
	"time"

	"golang.org/x/sys/unix"
	"google.golang.org/grpc"
)

const (
	// HostCID is the vsock CID for the host (always 2)
	HostCID = 2
)

// vsockListener implements net.Listener for vsock connections.
type vsockListener struct {
	fd   int
	port uint32
}

// ListenVsock creates a listener on the specified vsock port.
func ListenVsock(port uint32) (net.Listener, error) {
	fd, err := unix.Socket(unix.AF_VSOCK, unix.SOCK_STREAM, 0)
	if err != nil {
		return nil, fmt.Errorf("failed to create vsock socket: %w", err)
	}

	sa := &unix.SockaddrVM{
		CID:  unix.VMADDR_CID_ANY,
		Port: port,
	}

	if err := unix.Bind(fd, sa); err != nil {
		unix.Close(fd)
		return nil, fmt.Errorf("failed to bind vsock: %w", err)
	}

	if err := unix.Listen(fd, 128); err != nil {
		unix.Close(fd)
		return nil, fmt.Errorf("failed to listen on vsock: %w", err)
	}

	return &vsockListener{fd: fd, port: port}, nil
}

func (l *vsockListener) Accept() (net.Conn, error) {
	nfd, _, err := unix.Accept(l.fd)
	if err != nil {
		return nil, err
	}
	return &vsockConn{fd: nfd, port: l.port}, nil
}

func (l *vsockListener) Close() error {
	return unix.Close(l.fd)
}

func (l *vsockListener) Addr() net.Addr {
	return &vsockAddr{port: l.port}
}

// vsockAddr implements net.Addr for vsock.
type vsockAddr struct {
	cid  uint32
	port uint32
}

func (a *vsockAddr) Network() string { return "vsock" }
func (a *vsockAddr) String() string  { return fmt.Sprintf("vsock:%d:%d", a.cid, a.port) }

// vsockConn implements net.Conn for vsock connections.
type vsockConn struct {
	fd   int
	port uint32
}

func (c *vsockConn) Read(b []byte) (int, error) {
	n, err := unix.Read(c.fd, b)
	// unix.Read returns -1 on error, but io.Reader requires n >= 0
	if n < 0 {
		n = 0
	}
	// Return io.EOF when connection is closed (read returns 0 with no error)
	if n == 0 && err == nil {
		return 0, io.EOF
	}
	return n, err
}

func (c *vsockConn) Write(b []byte) (int, error) {
	return unix.Write(c.fd, b)
}

func (c *vsockConn) Close() error {
	return unix.Close(c.fd)
}

func (c *vsockConn) LocalAddr() net.Addr {
	return &vsockAddr{port: c.port}
}

func (c *vsockConn) RemoteAddr() net.Addr {
	return &vsockAddr{port: c.port}
}

func (c *vsockConn) SetDeadline(t time.Time) error {
	if err := c.SetReadDeadline(t); err != nil {
		return err
	}
	return c.SetWriteDeadline(t)
}

func (c *vsockConn) SetReadDeadline(t time.Time) error {
	var tv unix.Timeval
	if !t.IsZero() {
		tv = unix.NsecToTimeval(t.Sub(time.Now()).Nanoseconds())
	}
	return unix.SetsockoptTimeval(c.fd, unix.SOL_SOCKET, unix.SO_RCVTIMEO, &tv)
}

func (c *vsockConn) SetWriteDeadline(t time.Time) error {
	var tv unix.Timeval
	if !t.IsZero() {
		tv = unix.NsecToTimeval(t.Sub(time.Now()).Nanoseconds())
	}
	return unix.SetsockoptTimeval(c.fd, unix.SOL_SOCKET, unix.SO_SNDTIMEO, &tv)
}

// DialVsock connects to a vsock port on the specified CID.
func DialVsock(cid, port uint32) (net.Conn, error) {
	fd, err := unix.Socket(unix.AF_VSOCK, unix.SOCK_STREAM, 0)
	if err != nil {
		return nil, fmt.Errorf("failed to create vsock socket: %w", err)
	}

	sa := &unix.SockaddrVM{
		CID:  cid,
		Port: port,
	}

	if err := unix.Connect(fd, sa); err != nil {
		unix.Close(fd)
		return nil, fmt.Errorf("failed to connect vsock: %w", err)
	}

	return &vsockConn{fd: fd, port: port}, nil
}

// NewVMPeer creates a server-only Peer for the VM side (vmd).
// The host dials VMServicePort to call VMService.
func NewVMPeer() (*Peer, error) {
	listener, err := ListenVsock(VMServicePort)
	if err != nil {
		return nil, fmt.Errorf("listen VMServicePort: %w", err)
	}

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
	}, nil
}
