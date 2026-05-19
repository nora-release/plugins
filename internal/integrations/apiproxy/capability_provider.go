package apiproxy

import (
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	"github.com/nora/sandbox-vm/internal/envhost"
)

type capabilityPayload struct {
	SessionID     string `json:"s"`
	CallerAgentID string `json:"a"`
}

// CapabilityProvider issues and resolves signed capability tokens.
// It also provides execution-scoped runtime URLs for VM tools.
type CapabilityProvider struct {
	signingKey []byte
}

// NewCapabilityProvider creates a capability provider with an in-memory signing key.
func NewCapabilityProvider() *CapabilityProvider {
	return &CapabilityProvider{
		signingKey: generateCapabilitySigningKey(),
	}
}

// ExecutionRuntime returns execution-scoped runtime metadata for one caller.
func (p *CapabilityProvider) ExecutionRuntime(sessionID string, callerAgentID string) *envhost.RuntimeConfig {
	if p == nil {
		return nil
	}

	token := p.issueToken(sessionID, callerAgentID)
	if token == "" {
		return nil
	}

	return &envhost.RuntimeConfig{
		CapabilityToken: token,
	}
}

// ResolveCapability validates token signature and restores trusted claims.
func (p *CapabilityProvider) ResolveCapability(token string) (CapabilityClaims, bool) {
	token = strings.TrimSpace(token)
	if p == nil || token == "" {
		return CapabilityClaims{}, false
	}

	payloadToken, signatureToken, ok := strings.Cut(token, ".")
	if !ok || payloadToken == "" || signatureToken == "" {
		return CapabilityClaims{}, false
	}
	payloadBytes, err := base64.RawURLEncoding.DecodeString(payloadToken)
	if err != nil {
		return CapabilityClaims{}, false
	}
	signature, err := base64.RawURLEncoding.DecodeString(signatureToken)
	if err != nil {
		return CapabilityClaims{}, false
	}
	expectedSignature := signCapabilityPayload(p.signingKey, payloadBytes)
	if !hmac.Equal(signature, expectedSignature) {
		return CapabilityClaims{}, false
	}

	var payload capabilityPayload
	if err := json.Unmarshal(payloadBytes, &payload); err != nil {
		return CapabilityClaims{}, false
	}
	payload.SessionID = strings.TrimSpace(payload.SessionID)
	payload.CallerAgentID = strings.TrimSpace(payload.CallerAgentID)
	if payload.SessionID == "" || payload.CallerAgentID == "" {
		return CapabilityClaims{}, false
	}

	return CapabilityClaims{
		SessionID:     payload.SessionID,
		CallerAgentID: payload.CallerAgentID,
	}, true
}

func (p *CapabilityProvider) issueToken(sessionID string, callerAgentID string) string {
	sessionID = strings.TrimSpace(sessionID)
	callerAgentID = strings.TrimSpace(callerAgentID)
	if sessionID == "" || callerAgentID == "" {
		return ""
	}

	payload := capabilityPayload{
		SessionID:     sessionID,
		CallerAgentID: callerAgentID,
	}
	payloadBytes, err := json.Marshal(payload)
	if err != nil {
		return ""
	}
	signature := signCapabilityPayload(p.signingKey, payloadBytes)

	payloadToken := base64.RawURLEncoding.EncodeToString(payloadBytes)
	signatureToken := base64.RawURLEncoding.EncodeToString(signature)
	return payloadToken + "." + signatureToken
}

func generateCapabilitySigningKey() []byte {
	key := make([]byte, sha256.Size)
	if _, err := rand.Read(key); err == nil {
		return key
	}
	fallback := sha256.Sum256([]byte(fmt.Sprintf("capability-fallback-%d", time.Now().UnixNano())))
	copy(key, fallback[:])
	return key
}

func signCapabilityPayload(key []byte, payload []byte) []byte {
	mac := hmac.New(sha256.New, key)
	_, _ = mac.Write(payload)
	return mac.Sum(nil)
}
