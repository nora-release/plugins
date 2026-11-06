//go:build linux

package server

import (
	"context"
	"fmt"
	"strings"
	"sync"

	"github.com/nora/sandbox-vm/internal/integrations/apiproxy"
	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/status"
)

type runtimeBridgeConfig struct {
	CapabilityToken string
	SessionID       string
	CallerAgentID   string
	BackendURL      string
	BackendAPIKey   string
}

type runtimeBridgeConfigStore struct {
	mu      sync.RWMutex
	byToken map[string]runtimeBridgeConfig
}

var globalRuntimeBridgeConfigStore = &runtimeBridgeConfigStore{
	byToken: make(map[string]runtimeBridgeConfig),
}

func (s *runtimeBridgeConfigStore) Set(cfg runtimeBridgeConfig) error {
	cfg.CapabilityToken = strings.TrimSpace(cfg.CapabilityToken)
	cfg.SessionID = strings.TrimSpace(cfg.SessionID)
	cfg.CallerAgentID = strings.TrimSpace(cfg.CallerAgentID)
	cfg.BackendURL = strings.TrimSpace(cfg.BackendURL)
	cfg.BackendAPIKey = strings.TrimSpace(cfg.BackendAPIKey)

	if cfg.CapabilityToken == "" {
		return fmt.Errorf("capability token is required")
	}
	if cfg.SessionID == "" {
		return fmt.Errorf("session id is required")
	}
	if cfg.CallerAgentID == "" {
		return fmt.Errorf("caller agent id is required")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.byToken[cfg.CapabilityToken] = cfg
	return nil
}

func (s *runtimeBridgeConfigStore) ResolveCapability(token string) (apiproxy.CapabilityClaims, bool) {
	token = strings.TrimSpace(token)
	if token == "" {
		return apiproxy.CapabilityClaims{}, false
	}

	s.mu.RLock()
	cfg, ok := s.byToken[token]
	s.mu.RUnlock()
	if !ok {
		return apiproxy.CapabilityClaims{}, false
	}
	return apiproxy.CapabilityClaims{
		SessionID:     cfg.SessionID,
		CallerAgentID: cfg.CallerAgentID,
	}, true
}

func (s *vmDaemonServer) SetRuntimeBridgeConfig(ctx context.Context, req *vmrpc.SetRuntimeBridgeConfigRequest) (*vmrpc.SetRuntimeBridgeConfigResponse, error) {
	if s == nil {
		return nil, status.Error(codes.Internal, "vm daemon server is not initialized")
	}
	if req == nil {
		return nil, status.Error(codes.InvalidArgument, "runtime bridge config is required")
	}
	if err := globalRuntimeBridgeConfigStore.Set(runtimeBridgeConfig{
		CapabilityToken: req.GetCapabilityToken(),
		SessionID:       req.GetSessionId(),
		CallerAgentID:   req.GetCallerAgentId(),
		BackendURL:      req.GetBackendUrl(),
		BackendAPIKey:   req.GetBackendApiKey(),
	}); err != nil {
		return nil, status.Error(codes.InvalidArgument, err.Error())
	}
	return &vmrpc.SetRuntimeBridgeConfigResponse{}, nil
}
