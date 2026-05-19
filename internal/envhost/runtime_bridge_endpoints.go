package envhost

import (
	"fmt"
	"strings"
)

const DefaultRuntimeBridgePort uint32 = 50080

func DefaultRuntimeBridgeBaseURL() string {
	return RuntimeBridgeBaseURL(DefaultRuntimeBridgePort)
}

func RuntimeBridgeBaseURL(port uint32) string {
	if port == 0 {
		port = DefaultRuntimeBridgePort
	}
	return fmt.Sprintf("http://127.0.0.1:%d", port)
}

func RuntimeCapabilityURL(baseURL string, capabilityToken string) string {
	baseURL = strings.TrimRight(strings.TrimSpace(baseURL), "/")
	capabilityToken = strings.TrimSpace(capabilityToken)
	if baseURL == "" || capabilityToken == "" {
		return ""
	}
	return baseURL + "/" + capabilityToken
}
