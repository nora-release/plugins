package envhost

const (
	DefaultLocalHostID   = "local"
	DefaultSandboxHostID = "sandbox"
	DefaultRemoteHostID  = "remote"

	RuntimeEnvCapabilityURL = "BRIDGE_CAP_URL"
)

// EnvironmentType identifies the kind of execution backend.
type EnvironmentType string

const (
	EnvironmentTypeSandbox EnvironmentType = "sandbox"
	EnvironmentTypeLocal   EnvironmentType = "local"
	EnvironmentTypeRemote  EnvironmentType = "remote"
)

// HostInfo describes one environment host exposed to the framework.
type HostInfo struct {
	ID        string          `json:"id"`
	Name      string          `json:"name"`
	Label     string          `json:"label,omitempty"`
	Type      EnvironmentType `json:"type"`
	Available bool            `json:"available"`
}

// DefaultLabel returns the preferred environment label for this host.
func (h HostInfo) DefaultLabel() string {
	if h.Label != "" {
		return h.Label
	}
	if h.Name != "" {
		return h.Name
	}
	return h.ID
}

// EnvironmentMetadata describes one execution environment exposed to sessions.
type EnvironmentMetadata struct {
	ID           string                  `json:"id"`
	Type         EnvironmentType         `json:"type"`
	Description  string                  `json:"description,omitempty"`
	Protected    bool                    `json:"protected,omitempty"`
	WorkspaceDir string                  `json:"workspace_dir,omitempty"`
	Capabilities EnvironmentCapabilities `json:"capabilities,omitempty"`
}

type EnvironmentCapabilities struct {
	WorkspaceState      bool `json:"workspace_state,omitempty"`
	WorkspaceReview     bool `json:"workspace_review,omitempty"`
	WorkspaceFileIO     bool `json:"workspace_file_io,omitempty"`
	WorkspaceFileExport bool `json:"workspace_file_export,omitempty"`
}

func (m EnvironmentMetadata) SupportsWorkspaceState() bool {
	return m.Capabilities.WorkspaceState
}

func (m EnvironmentMetadata) SupportsWorkspaceReview() bool {
	return m.Capabilities.WorkspaceReview
}

func (m EnvironmentMetadata) SupportsWorkspaceFileIO() bool {
	return m.Capabilities.WorkspaceFileIO
}

func (m EnvironmentMetadata) SupportsWorkspaceFileExport() bool {
	return m.Capabilities.WorkspaceFileExport
}

// RuntimeConfig carries execution-time runtime bridge metadata.
// Hosts expand this into local runtime URLs.
type RuntimeConfig struct {
	CapabilityToken string `json:"capability_token,omitempty"`
	SessionID       string `json:"session_id,omitempty"`
	CallerAgentID   string `json:"caller_agent_id,omitempty"`
	BackendURL      string `json:"backend_url,omitempty"`
	BackendAPIKey   string `json:"backend_api_key,omitempty"`
}
