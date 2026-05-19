package apiproxy

import (
	"net/http"

	"github.com/nora/sandbox-vm/internal/envhost"
)

const (
	// DefaultPort is the default vsock port for the API proxy.
	DefaultPort = envhost.DefaultRuntimeBridgePort
)

// CapabilityClaims represents trusted runtime identity bound to one capability token.
type CapabilityClaims struct {
	SessionID     string
	CallerAgentID string
}

// CapabilityResolver validates capability tokens and returns trusted claims.
type CapabilityResolver interface {
	ResolveCapability(token string) (CapabilityClaims, bool)
}

// LocalDispatcher routes local API calls to a session-scoped handler.
type LocalDispatcher interface {
	DispatchLocal(sessionID string, callerAgentID string, req *http.Request) (*http.Response, bool)
}

// parsedCredentials represents credentials parsed from the provider's JSON.
type parsedCredentials struct {
	APIKey  string            `json:"apiKey"`
	BaseURL string            `json:"baseURL"`
	Headers map[string]string `json:"headers,omitempty"`
}
