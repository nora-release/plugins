package runtimebridge

import (
	"context"

	"github.com/nora/sandbox-vm/internal/envhost"
	"github.com/nora/sandbox-vm/internal/integrations/apiproxy"
)

func NewLocalExecutionBridge() (*apiproxy.CapabilityProvider, envhost.RuntimeBridge) {
	capabilityProvider := apiproxy.NewCapabilityProvider()
	return capabilityProvider, envhost.NewDirectRuntimeBridge(
		UnsupportedToolHandler{},
		UnsupportedHTTPHandler{},
	)
}

type UnsupportedToolHandler struct{}

func (UnsupportedToolHandler) CallTool(ctx context.Context, req *envhost.RuntimeToolRequest) (*envhost.RuntimeToolResult, error) {
	return nil, envhost.NewProtocolError(envhost.ErrCodeCapabilityNotSupported, "runtime tool callbacks are local-only and no external relay is configured")
}

type UnsupportedHTTPHandler struct{}

func (UnsupportedHTTPHandler) DoHTTP(ctx context.Context, req *envhost.RuntimeHTTPRequest) (*envhost.RuntimeHTTPResult, error) {
	return nil, envhost.NewProtocolError(envhost.ErrCodeCapabilityNotSupported, "runtime HTTP callbacks are local-only and no external relay is configured")
}
