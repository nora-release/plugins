package netstack

import (
	"context"
	"fmt"
	"io"
	"log"
	"net"
	"strconv"
	"sync"
	"time"

	"github.com/nora/sandbox-vm/internal/platform/vm/guestnetwork"
	"github.com/nora/sandbox-vm/internal/platform/vm/netstack/frame"
	"gvisor.dev/gvisor/pkg/buffer"
	"gvisor.dev/gvisor/pkg/tcpip"
	"gvisor.dev/gvisor/pkg/tcpip/adapters/gonet"
	"gvisor.dev/gvisor/pkg/tcpip/header"
	"gvisor.dev/gvisor/pkg/tcpip/link/channel"
	"gvisor.dev/gvisor/pkg/tcpip/network/ipv4"
	"gvisor.dev/gvisor/pkg/tcpip/stack"
	"gvisor.dev/gvisor/pkg/tcpip/transport/tcp"
	"gvisor.dev/gvisor/pkg/tcpip/transport/udp"
	"gvisor.dev/gvisor/pkg/waiter"
)

const (
	defaultMTU            = 1500
	defaultQueueSize      = 1024
	defaultTCPMaxInFlight = 1024
	defaultTCPIdleTimeout = 5 * time.Minute
	defaultTCPMaxActive   = 200
	defaultDialTimeout    = 10 * time.Second
	defaultUDPIdleTimeout = 2 * time.Minute

	nicID = tcpip.NICID(1)
)

type Config struct {
	MTU            int
	QueueSize      int
	TCPMaxInFlight int
	TCPIdleTimeout time.Duration
	TCPMaxActive   int
	DialTimeout    time.Duration
	UDPIdleTimeout time.Duration
}

func DefaultConfig() Config {
	return Config{
		MTU:            defaultMTU,
		QueueSize:      defaultQueueSize,
		TCPMaxInFlight: defaultTCPMaxInFlight,
		TCPIdleTimeout: defaultTCPIdleTimeout,
		TCPMaxActive:   defaultTCPMaxActive,
		DialTimeout:    defaultDialTimeout,
		UDPIdleTimeout: defaultUDPIdleTimeout,
	}
}

func Run(ctx context.Context, conn io.ReadWriteCloser, cfg Config) error {
	cfg = normalizeConfig(cfg)
	defer conn.Close()

	linkEP := channel.New(cfg.QueueSize, uint32(cfg.MTU), "")
	defer linkEP.Close()

	s := stack.New(stack.Options{
		NetworkProtocols:   []stack.NetworkProtocolFactory{ipv4.NewProtocol},
		TransportProtocols: []stack.TransportProtocolFactory{tcp.NewProtocol, udp.NewProtocol},
	})

	if err := s.CreateNIC(nicID, linkEP); err != nil {
		return fmt.Errorf("create nic: %s", err)
	}
	if err := s.SetPromiscuousMode(nicID, true); err != nil {
		return fmt.Errorf("set promiscuous mode: %s", err)
	}
	if err := s.SetSpoofing(nicID, true); err != nil {
		return fmt.Errorf("set spoofing: %s", err)
	}
	s.SetRouteTable([]tcpip.Route{{
		Destination: header.IPv4EmptySubnet,
		NIC:         nicID,
	}})

	svc := service{
		ctx:        ctx,
		stack:      s,
		cfg:        cfg,
		tcpLimiter: newConnectionLimiter(cfg.TCPMaxActive),
	}

	tcpForwarder := tcp.NewForwarder(s, 0, cfg.TCPMaxInFlight, svc.handleTCP)
	s.SetTransportProtocolHandler(tcp.ProtocolNumber, tcpForwarder.HandlePacket)

	udpForwarder := udp.NewForwarder(s, svc.handleUDP)
	s.SetTransportProtocolHandler(udp.ProtocolNumber, udpForwarder.HandlePacket)

	errCh := make(chan error, 2)
	go func() {
		errCh <- readInboundPackets(ctx, conn, linkEP, cfg.MTU)
	}()
	go func() {
		errCh <- writeOutboundPackets(ctx, conn, linkEP, cfg.MTU)
	}()

	select {
	case <-ctx.Done():
		return nil
	case err := <-errCh:
		if err == nil || ctx.Err() != nil {
			return nil
		}
		return err
	}
}

func normalizeConfig(cfg Config) Config {
	if cfg.MTU <= 0 {
		cfg.MTU = defaultMTU
	}
	if cfg.QueueSize <= 0 {
		cfg.QueueSize = defaultQueueSize
	}
	if cfg.TCPMaxInFlight <= 0 {
		cfg.TCPMaxInFlight = defaultTCPMaxInFlight
	}
	if cfg.TCPIdleTimeout <= 0 {
		cfg.TCPIdleTimeout = defaultTCPIdleTimeout
	}
	if cfg.TCPMaxActive <= 0 {
		cfg.TCPMaxActive = defaultTCPMaxActive
	}
	if cfg.DialTimeout <= 0 {
		cfg.DialTimeout = defaultDialTimeout
	}
	if cfg.UDPIdleTimeout <= 0 {
		cfg.UDPIdleTimeout = defaultUDPIdleTimeout
	}
	return cfg
}

func readInboundPackets(ctx context.Context, conn io.Reader, linkEP *channel.Endpoint, mtu int) error {
	for {
		select {
		case <-ctx.Done():
			return nil
		default:
		}

		packet, err := frame.Read(conn, mtu)
		if err != nil {
			return fmt.Errorf("read framed packet: %w", err)
		}
		if len(packet) == 0 {
			continue
		}

		proto, ok := detectNetworkProtocol(packet)
		if !ok {
			continue
		}

		pkt := stack.NewPacketBuffer(stack.PacketBufferOptions{
			Payload: buffer.MakeWithData(packet),
		})
		linkEP.InjectInbound(proto, pkt)
		pkt.DecRef()
	}
}

func writeOutboundPackets(ctx context.Context, conn io.Writer, linkEP *channel.Endpoint, mtu int) error {
	for {
		pkt := linkEP.ReadContext(ctx)
		if pkt == nil {
			return nil
		}

		payloadBuf := pkt.ToBuffer()
		payload := payloadBuf.Flatten()
		payloadBuf.Release()
		pkt.DecRef()

		if len(payload) == 0 {
			continue
		}

		if err := frame.Write(conn, payload, mtu); err != nil {
			return fmt.Errorf("write framed packet: %w", err)
		}
	}
}

type service struct {
	ctx        context.Context
	stack      *stack.Stack
	cfg        Config
	tcpLimiter *connectionLimiter
}

func (s service) handleTCP(req *tcp.ForwarderRequest) {
	id := req.ID()
	target := joinHostPort(id.LocalAddress, id.LocalPort)

	release, ok := s.acquireTCPProxySlot(target)
	if !ok {
		req.Complete(true)
		return
	}

	dialCtx, cancel := context.WithTimeout(s.ctx, s.cfg.DialTimeout)
	defer cancel()

	hostConn, err := (&net.Dialer{}).DialContext(dialCtx, "tcp", target)
	if err != nil {
		release()
		req.Complete(true)
		return
	}

	var wq waiter.Queue
	ep, tcpErr := req.CreateEndpoint(&wq)
	if tcpErr != nil {
		_ = hostConn.Close()
		release()
		req.Complete(true)
		return
	}
	req.Complete(false)

	guestConn := gonet.NewTCPConn(&wq, ep)
	proxyStream(s.ctx, guestConn, hostConn, s.cfg.TCPIdleTimeout)
	release()
}

func (s service) acquireTCPProxySlot(target string) (func(), bool) {
	if !s.tcpLimiter.TryAcquire() {
		log.Printf("[netstack] rejecting tcp connection for %s: active=%d limit=%d", target, s.tcpLimiter.Active(), s.tcpLimiter.Limit())
		return nil, false
	}
	return sync.OnceFunc(func() {
		s.tcpLimiter.Release()
	}), true
}

func (s service) handleUDP(req *udp.ForwarderRequest) {
	id := req.ID()
	target := &net.UDPAddr{
		IP:   resolveHostDialIP(id.LocalAddress),
		Port: int(id.LocalPort),
	}

	hostConn, err := net.DialUDP("udp", nil, target)
	if err != nil {
		return
	}

	var wq waiter.Queue
	ep, udpErr := req.CreateEndpoint(&wq)
	if udpErr != nil {
		_ = hostConn.Close()
		return
	}

	guestConn := gonet.NewUDPConn(&wq, ep)
	go proxyUDP(s.ctx, guestConn, hostConn, s.cfg.UDPIdleTimeout)
}

func proxyStream(ctx context.Context, a, b net.Conn, idleTimeout time.Duration) {
	var wg sync.WaitGroup
	wg.Add(2)
	closeBoth := sync.OnceFunc(func() {
		_ = a.Close()
		_ = b.Close()
	})
	stopCloseOnCancel := context.AfterFunc(ctx, closeBoth)
	defer stopCloseOnCancel()
	idleTracker := newIdleTracker(idleTimeout, a, b)

	copyOneWay := func(dst, src net.Conn) {
		defer wg.Done()
		_, _ = copyWithIdleTimeout(dst, src, idleTracker)
		closeBoth()
	}

	go copyOneWay(a, b)
	go copyOneWay(b, a)
	wg.Wait()
}

func copyWithIdleTimeout(dst, src net.Conn, idleTracker *idleTracker) (int64, error) {
	buf := make([]byte, 32*1024)
	var written int64

	for {
		n, err := src.Read(buf)
		if n > 0 {
			if writeErr := writeFull(dst, buf[:n]); writeErr != nil {
				written += int64(n)
				return written, writeErr
			}
			idleTracker.Bump()
			written += int64(n)
		}
		if err != nil {
			return written, err
		}
	}
}

func writeFull(dst net.Conn, buf []byte) error {
	for len(buf) > 0 {
		n, err := dst.Write(buf)
		if n > 0 {
			buf = buf[n:]
		}
		if err != nil {
			return err
		}
		if n == 0 {
			return io.ErrShortWrite
		}
	}
	return nil
}

type idleTracker struct {
	mu      sync.Mutex
	timeout time.Duration
	conns   []net.Conn
}

func newIdleTracker(timeout time.Duration, conns ...net.Conn) *idleTracker {
	tracker := &idleTracker{
		timeout: timeout,
		conns:   conns,
	}
	tracker.Bump()
	return tracker
}

func (t *idleTracker) Bump() {
	if t == nil || t.timeout <= 0 {
		return
	}

	deadline := time.Now().Add(t.timeout)

	t.mu.Lock()
	defer t.mu.Unlock()

	for _, conn := range t.conns {
		_ = conn.SetReadDeadline(deadline)
		_ = conn.SetWriteDeadline(deadline)
	}
}

func proxyUDP(ctx context.Context, guestConn *gonet.UDPConn, hostConn *net.UDPConn, idleTimeout time.Duration) {
	defer guestConn.Close()
	defer hostConn.Close()

	done := make(chan struct{}, 2)
	closeBoth := sync.OnceFunc(func() {
		_ = guestConn.Close()
		_ = hostConn.Close()
	})

	go func() {
		defer func() { done <- struct{}{} }()
		buf := make([]byte, defaultMTU)
		for {
			_ = guestConn.SetReadDeadline(time.Now().Add(idleTimeout))
			n, err := guestConn.Read(buf)
			if err != nil {
				return
			}
			_ = hostConn.SetWriteDeadline(time.Now().Add(idleTimeout))
			if _, err := hostConn.Write(buf[:n]); err != nil {
				return
			}
		}
	}()

	go func() {
		defer func() { done <- struct{}{} }()
		buf := make([]byte, defaultMTU)
		for {
			_ = hostConn.SetReadDeadline(time.Now().Add(idleTimeout))
			n, err := hostConn.Read(buf)
			if err != nil {
				return
			}
			_ = guestConn.SetWriteDeadline(time.Now().Add(idleTimeout))
			if _, err := guestConn.Write(buf[:n]); err != nil {
				return
			}
		}
	}()

	select {
	case <-ctx.Done():
	case <-done:
	}
	closeBoth()
	<-done
}

func detectNetworkProtocol(packet []byte) (tcpip.NetworkProtocolNumber, bool) {
	if len(packet) == 0 {
		return 0, false
	}
	switch packet[0] >> 4 {
	case 4:
		return ipv4.ProtocolNumber, true
	default:
		log.Printf("[netstack] dropping unsupported network version=%d", packet[0]>>4)
		return 0, false
	}
}

func joinHostPort(addr tcpip.Address, port uint16) string {
	return net.JoinHostPort(resolveHostDialIP(addr).String(), strconv.Itoa(int(port)))
}

func resolveHostDialIP(addr tcpip.Address) net.IP {
	return guestnetwork.ResolveHostDialIP(net.IP(addr.AsSlice()))
}

type connectionLimiter struct {
	tokens chan struct{}
}

func newConnectionLimiter(limit int) *connectionLimiter {
	if limit <= 0 {
		return &connectionLimiter{}
	}
	return &connectionLimiter{
		tokens: make(chan struct{}, limit),
	}
}

func (l *connectionLimiter) TryAcquire() bool {
	if l == nil || l.tokens == nil {
		return true
	}
	select {
	case l.tokens <- struct{}{}:
		return true
	default:
		return false
	}
}

func (l *connectionLimiter) Release() {
	if l == nil || l.tokens == nil {
		return
	}
	select {
	case <-l.tokens:
	default:
	}
}

func (l *connectionLimiter) Active() int {
	if l == nil || l.tokens == nil {
		return 0
	}
	return len(l.tokens)
}

func (l *connectionLimiter) Limit() int {
	if l == nil || l.tokens == nil {
		return 0
	}
	return cap(l.tokens)
}
