package frame

import (
	"bytes"
	"io"
	"testing"
)

func TestRoundTrip(t *testing.T) {
	var buf bytes.Buffer
	packet := []byte{0x45, 0x00, 0x01, 0x02}

	if err := Write(&buf, packet, 1500); err != nil {
		t.Fatalf("write failed: %v", err)
	}

	got, err := Read(&buf, 1500)
	if err != nil {
		t.Fatalf("read failed: %v", err)
	}

	if !bytes.Equal(got, packet) {
		t.Fatalf("packet mismatch: got %v want %v", got, packet)
	}
}

func TestRejectOversizeWrite(t *testing.T) {
	var buf bytes.Buffer
	packet := make([]byte, 1501)

	if err := Write(&buf, packet, 1500); err == nil {
		t.Fatalf("expected oversize write error")
	}
}

func TestRejectOversizeRead(t *testing.T) {
	var buf bytes.Buffer
	packet := make([]byte, 16)

	if err := Write(&buf, packet, 16); err != nil {
		t.Fatalf("write failed: %v", err)
	}

	if _, err := Read(&buf, 8); err == nil {
		t.Fatalf("expected oversize read error")
	}
}

func TestReadEOF(t *testing.T) {
	if _, err := Read(bytes.NewReader(nil), 1500); err != io.EOF {
		t.Fatalf("expected io.EOF, got %v", err)
	}
}
