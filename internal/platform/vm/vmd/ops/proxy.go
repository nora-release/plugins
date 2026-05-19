//go:build linux

package ops

import (
	"fmt"
	"log"
	"os"
	"strings"
)

// ProxyManager handles proxy environment configuration.
type ProxyManager struct{}

// NewProxyManager creates a new ProxyManager.
func NewProxyManager() *ProxyManager {
	return &ProxyManager{}
}

// SetProxyEnv configures proxy environment variables in the VM.
func (m *ProxyManager) SetProxyEnv(httpProxy, httpsProxy, noProxy string) error {
	if httpProxy == "" && httpsProxy == "" && noProxy == "" {
		log.Println("No proxy environment variables to configure")
		return nil
	}

	// Build profile content (with export prefix)
	var profileContent strings.Builder
	if httpProxy != "" {
		profileContent.WriteString(fmt.Sprintf("export http_proxy=%s\n", httpProxy))
		profileContent.WriteString(fmt.Sprintf("export HTTP_PROXY=%s\n", httpProxy))
	}
	if httpsProxy != "" {
		profileContent.WriteString(fmt.Sprintf("export https_proxy=%s\n", httpsProxy))
		profileContent.WriteString(fmt.Sprintf("export HTTPS_PROXY=%s\n", httpsProxy))
	}
	if noProxy != "" {
		profileContent.WriteString(fmt.Sprintf("export no_proxy=%s\n", noProxy))
		profileContent.WriteString(fmt.Sprintf("export NO_PROXY=%s\n", noProxy))
	}

	// Write to /etc/profile.d for shell sessions
	profilePath := "/etc/profile.d/host-proxy.sh"
	if err := os.WriteFile(profilePath, []byte(profileContent.String()), 0644); err != nil {
		return fmt.Errorf("write profile: %w", err)
	}
	log.Printf("Configured proxy in %s", profilePath)

	return nil
}
