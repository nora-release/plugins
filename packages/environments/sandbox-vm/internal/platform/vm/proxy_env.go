package vm

import "os"

// GetHostProxyEnv reads proxy environment variables from the host.
// Used when building VM config so the guest inherits host proxy settings.
func GetHostProxyEnv() (httpProxy, httpsProxy, noProxy string) {
	httpProxy = os.Getenv("http_proxy")
	if httpProxy == "" {
		httpProxy = os.Getenv("HTTP_PROXY")
	}
	httpsProxy = os.Getenv("https_proxy")
	if httpsProxy == "" {
		httpsProxy = os.Getenv("HTTPS_PROXY")
	}
	noProxy = os.Getenv("no_proxy")
	if noProxy == "" {
		noProxy = os.Getenv("NO_PROXY")
	}
	return
}
