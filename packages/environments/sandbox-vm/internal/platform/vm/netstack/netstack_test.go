package netstack

import (
	"bytes"
	"context"
	"io"
	"net"
	"sync"
	"testing"
	"time"

	"github.com/nora/sandbox-vm/internal/platform/vm/guestnetwork"
	"gvisor.dev/gvisor/pkg/tcpip"
)

func TestResolveHostDialIPRewritesGuestHostLoopbackAlias(t *testing.T) {
	got := resolveHostDialIP(tcpip.AddrFromSlice(net.ParseIP(guestnetwork.HostLoopbackIP).To4()))
	want := net.IPv4(127, 0, 0, 1)
	if !got.Equal(want) {
		t.Fatalf("resolveHostDialIP(alias) = %v, want %v", got, want)
	}
}

func TestResolveHostDialIPKeepsNonAliasAddress(t *testing.T) {
	got := resolveHostDialIP(tcpip.AddrFromSlice([]byte{1, 1, 1, 1}))
	want := net.IPv4(1, 1, 1, 1)
	if !got.Equal(want) {
		t.Fatalf("resolveHostDialIP(non-alias) = %v, want %v", got, want)
	}
}

func TestConnectionLimiterTryAcquireRelease(t *testing.T) {
	limiter := newConnectionLimiter(2)
	if !limiter.TryAcquire() {
		t.Fatal("first acquire should succeed")
	}
	if !limiter.TryAcquire() {
		t.Fatal("second acquire should succeed")
	}
	if limiter.Active() != 2 {
		t.Fatalf("active = %d, want 2", limiter.Active())
	}
	if limiter.TryAcquire() {
		t.Fatal("third acquire should fail once limit is reached")
	}

	limiter.Release()

	if limiter.Active() != 1 {
		t.Fatalf("active after release = %d, want 1", limiter.Active())
	}
	if !limiter.TryAcquire() {
		t.Fatal("acquire after release should succeed")
	}
}

func TestServiceAcquireTCPProxySlotRejectsConnectionsAboveActiveLimit(t *testing.T) {
	const (
		limit    = 200
		attempts = 512
	)

	svc := service{
		tcpLimiter: newConnectionLimiter(limit),
	}

	start := make(chan struct{})
	hold := make(chan struct{})
	results := make(chan bool, attempts)

	var wg sync.WaitGroup
	for i := 0; i < attempts; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			<-start

			release, ok := svc.acquireTCPProxySlot("127.0.0.1:8080")
			results <- ok
			if !ok {
				return
			}

			<-hold
			release()
		}()
	}

	close(start)

	accepted := 0
	rejected := 0
	for i := 0; i < attempts; i++ {
		select {
		case ok := <-results:
			if ok {
				accepted++
			} else {
				rejected++
			}
		case <-time.After(2 * time.Second):
			t.Fatal("timed out waiting for connection attempts to be admitted or rejected")
		}
	}

	if accepted != limit {
		t.Fatalf("accepted = %d, want %d", accepted, limit)
	}
	if rejected != attempts-limit {
		t.Fatalf("rejected = %d, want %d", rejected, attempts-limit)
	}
	if got := svc.tcpLimiter.Active(); got != limit {
		t.Fatalf("active while connections are held = %d, want %d", got, limit)
	}

	close(hold)
	wg.Wait()

	if got := svc.tcpLimiter.Active(); got != 0 {
		t.Fatalf("active after releasing held connections = %d, want 0", got)
	}
}

func TestProxyStreamTransfersDataAndClosesIdleConnections(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	clientConn, inboundConn := net.Pipe()
	outboundConn, serverConn := net.Pipe()
	defer clientConn.Close()
	defer serverConn.Close()

	done := make(chan struct{})
	go func() {
		proxyStream(ctx, inboundConn, outboundConn, 50*time.Millisecond)
		close(done)
	}()

	payload := []byte("hello over proxy")
	go func() {
		_, _ = clientConn.Write(payload)
	}()

	got := make([]byte, len(payload))
	if _, err := io.ReadFull(serverConn, got); err != nil {
		t.Fatalf("read forwarded data: %v", err)
	}
	if !bytes.Equal(got, payload) {
		t.Fatalf("forwarded payload = %q, want %q", got, payload)
	}

	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("proxyStream did not close idle connections")
	}

	if _, err := clientConn.Write([]byte("x")); err == nil {
		t.Fatal("client write should fail after idle timeout closes the proxy")
	}
}

func TestProxyStreamKeepsOneWayTrafficAliveUntilBidirectionalIdle(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	clientConn, inboundConn := net.Pipe()
	outboundConn, serverConn := net.Pipe()
	defer clientConn.Close()
	defer serverConn.Close()

	const idleTimeout = 75 * time.Millisecond

	done := make(chan struct{})
	go func() {
		proxyStream(ctx, inboundConn, outboundConn, idleTimeout)
		close(done)
	}()

	chunks := [][]byte{
		[]byte("chunk-one"),
		[]byte("chunk-two"),
		[]byte("chunk-three"),
	}
	writeErrCh := make(chan error, 1)
	go func() {
		defer close(writeErrCh)
		for i, chunk := range chunks {
			if _, err := serverConn.Write(chunk); err != nil {
				writeErrCh <- err
				return
			}
			if i < len(chunks)-1 {
				time.Sleep(40 * time.Millisecond)
			}
		}
	}()

	for _, chunk := range chunks {
		got := make([]byte, len(chunk))
		if _, err := io.ReadFull(clientConn, got); err != nil {
			t.Fatalf("read one-way chunk: %v", err)
		}
		if !bytes.Equal(got, chunk) {
			t.Fatalf("one-way chunk = %q, want %q", got, chunk)
		}
	}

	if err := <-writeErrCh; err != nil {
		t.Fatalf("write one-way chunk: %v", err)
	}

	select {
	case <-done:
		t.Fatal("proxyStream closed while one-way traffic was still flowing")
	case <-time.After(idleTimeout / 2):
	}

	select {
	case <-done:
	case <-time.After(2 * time.Second):
		t.Fatal("proxyStream did not close after bidirectional inactivity")
	}

	if _, err := serverConn.Write([]byte("x")); err == nil {
		t.Fatal("server write should fail after idle timeout closes the proxy")
	}
}
