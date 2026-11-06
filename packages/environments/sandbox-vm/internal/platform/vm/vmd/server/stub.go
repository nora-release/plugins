//go:build !linux

// Package server implements the sandbox RPC server that runs inside the VM.
package server

import "fmt"

// Server is a stub for non-Linux systems.
type Server struct{}

// NewServer is not supported on non-Linux systems.
func NewServer(addr string) (*Server, error) {
	return nil, fmt.Errorf("server is only supported on Linux")
}

// NewVsockServer is not supported on non-Linux systems.
func NewVsockServer(port uint32) (*Server, error) {
	return nil, fmt.Errorf("server is only supported on Linux")
}

// Serve is not supported on non-Linux systems.
func (s *Server) Serve() {}

// Addr returns empty string on non-Linux systems.
func (s *Server) Addr() string { return "" }

// Close is a no-op on non-Linux systems.
func (s *Server) Close() error { return nil }
