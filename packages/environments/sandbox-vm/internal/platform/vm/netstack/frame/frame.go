package frame

import (
	"encoding/binary"
	"fmt"
	"io"
)

const HeaderSize = 4

func Read(r io.Reader, maxPayload int) ([]byte, error) {
	if maxPayload <= 0 {
		return nil, fmt.Errorf("invalid max payload: %d", maxPayload)
	}

	var header [HeaderSize]byte
	if _, err := io.ReadFull(r, header[:]); err != nil {
		return nil, err
	}

	size := int(binary.BigEndian.Uint32(header[:]))
	if size < 0 || size > maxPayload {
		return nil, fmt.Errorf("frame size %d exceeds max payload %d", size, maxPayload)
	}

	packet := make([]byte, size)
	if _, err := io.ReadFull(r, packet); err != nil {
		return nil, err
	}

	return packet, nil
}

func Write(w io.Writer, packet []byte, maxPayload int) error {
	if maxPayload <= 0 {
		return fmt.Errorf("invalid max payload: %d", maxPayload)
	}
	if len(packet) > maxPayload {
		return fmt.Errorf("frame payload %d exceeds max payload %d", len(packet), maxPayload)
	}

	var header [HeaderSize]byte
	binary.BigEndian.PutUint32(header[:], uint32(len(packet)))
	if err := writeAll(w, header[:]); err != nil {
		return err
	}
	if len(packet) == 0 {
		return nil
	}
	return writeAll(w, packet)
}

func writeAll(w io.Writer, payload []byte) error {
	for len(payload) > 0 {
		written, err := w.Write(payload)
		if err != nil {
			return err
		}
		payload = payload[written:]
	}
	return nil
}
