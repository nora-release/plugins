package runtimebridge

import (
	"strings"

	"github.com/nora/sandbox-vm/internal/envhost"
	"github.com/nora/sandbox-vm/internal/integrations/apiproxy"
)

func resolveCapability(resolver apiproxy.CapabilityResolver, token string) (apiproxy.CapabilityClaims, error) {
	if resolver == nil {
		return apiproxy.CapabilityClaims{}, envhost.NewProtocolError(envhost.ErrCodeCapabilityInvalid, "capability resolver is unavailable")
	}

	token = strings.TrimSpace(token)
	if token == "" {
		return apiproxy.CapabilityClaims{}, envhost.NewProtocolError(envhost.ErrCodeCapabilityInvalid, "capability token is required")
	}

	claims, ok := resolver.ResolveCapability(token)
	if !ok {
		return apiproxy.CapabilityClaims{}, envhost.NewProtocolError(envhost.ErrCodeCapabilityInvalid, "capability token is invalid")
	}
	return claims, nil
}
