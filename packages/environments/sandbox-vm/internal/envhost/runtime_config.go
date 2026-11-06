package envhost

import "strings"

func cloneRuntimeConfig(runtime *RuntimeConfig) *RuntimeConfig {
	if runtime == nil {
		return nil
	}
	cloned := &RuntimeConfig{
		CapabilityToken: strings.TrimSpace(runtime.CapabilityToken),
		SessionID:       strings.TrimSpace(runtime.SessionID),
		CallerAgentID:   strings.TrimSpace(runtime.CallerAgentID),
		BackendURL:      strings.TrimSpace(runtime.BackendURL),
		BackendAPIKey:   strings.TrimSpace(runtime.BackendAPIKey),
	}
	if cloned.CapabilityToken == "" {
		return nil
	}
	return cloned
}

func CloneRuntimeConfig(runtime *RuntimeConfig) *RuntimeConfig {
	return cloneRuntimeConfig(runtime)
}

func mergeExecutionEnv(base map[string]string, runtime *RuntimeConfig, bridgeBaseURL string) map[string]string {
	if len(base) == 0 && runtime == nil {
		return nil
	}

	merged := make(map[string]string, len(base)+3)
	for key, value := range base {
		key = strings.TrimSpace(key)
		if key == "" {
			continue
		}
		merged[key] = value
	}

	runtime = cloneRuntimeConfig(runtime)
	if runtime == nil {
		if len(merged) == 0 {
			return nil
		}
		return merged
	}

	baseURL := strings.TrimRight(strings.TrimSpace(bridgeBaseURL), "/")
	token := strings.TrimSpace(runtime.CapabilityToken)
	if baseURL != "" && token != "" {
		merged[RuntimeEnvCapabilityURL] = RuntimeCapabilityURL(baseURL, token)
	}

	if len(merged) == 0 {
		return nil
	}
	return merged
}

func BuildExecutionEnv(base map[string]string, runtime *RuntimeConfig, bridgeBaseURL string) map[string]string {
	return mergeExecutionEnv(base, runtime, bridgeBaseURL)
}
