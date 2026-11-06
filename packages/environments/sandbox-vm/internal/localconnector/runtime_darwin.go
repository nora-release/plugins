//go:build darwin

package localconnector

import (
	"context"

	envsandbox "github.com/nora/sandbox-vm/internal/envhost/sandbox"
	"github.com/nora/sandbox-vm/internal/integrations/apiproxy"
)

// New creates one long-lived overlay-backed sandbox rooted at cfg.RootPath.
func New(cfg Config) (*Runtime, error) {
	opts, err := normalizeConfig(cfg)
	if err != nil {
		return nil, err
	}

	vmOptions := &envsandbox.VMOptions{
		KernelPath:       opts.kernelPath,
		RootfsPath:       opts.rootfsPath,
		RootfsOverlayDir: opts.rootfsOverlayDir,
		WorkspaceDir:     opts.rootPath,
		Mounts:           sandboxMounts(opts.mounts),
	}
	envsandbox.ApplyHostProxyFromEnv(vmOptions)

	host, err := envsandbox.NewManagedHost(context.Background(), envsandbox.InProcessHostConfig{
		MetadataDir:          opts.metadataDir,
		WorkspaceDir:         opts.rootPath,
		VMOptions:            vmOptions,
		InstallSignalHandler: false,
		HostName:             "Local Connector Sandbox Host",
		HostLabel:            "Connector Sandbox",
	})
	if err != nil {
		return nil, err
	}

	if opts.backendURL != "" && opts.backendAPIKey != "" {
		opts.capabilityProvider = apiproxy.NewCapabilityProvider()
	}

	runtime, err := newRuntimeWithHost(host, host, opts)
	if err != nil {
		_ = host.Close()
		return nil, err
	}
	return runtime, nil
}

// NewShared creates one long-lived VM-backed host and provisions one environment per session.
func NewShared(cfg Config) (*SharedRuntime, error) {
	opts, err := normalizeConfig(cfg)
	if err != nil {
		return nil, err
	}

	vmOptions := &envsandbox.VMOptions{
		KernelPath:       opts.kernelPath,
		RootfsPath:       opts.rootfsPath,
		RootfsOverlayDir: opts.rootfsOverlayDir,
		WorkspaceDir:     opts.rootPath,
		Mounts:           sandboxMounts(opts.mounts),
	}
	envsandbox.ApplyHostProxyFromEnv(vmOptions)

	host, err := envsandbox.NewManagedHost(context.Background(), envsandbox.InProcessHostConfig{
		MetadataDir:          opts.metadataDir,
		WorkspaceDir:         opts.rootPath,
		VMOptions:            vmOptions,
		InstallSignalHandler: false,
		HostName:             "Local Connector Sandbox Host",
		HostLabel:            "Connector Sandbox",
	})
	if err != nil {
		return nil, err
	}

	if opts.backendURL != "" && opts.backendAPIKey != "" {
		opts.capabilityProvider = apiproxy.NewCapabilityProvider()
	}

	runtime, err := newSharedRuntimeWithHost(host, host, opts)
	if err != nil {
		_ = host.Close()
		return nil, err
	}
	return runtime, nil
}

func sandboxMounts(mounts []Mount) []envsandbox.Mount {
	result := make([]envsandbox.Mount, 0, len(mounts))
	for _, mount := range mounts {
		result = append(result, envsandbox.Mount{
			HostPath:    mount.HostPath,
			VMPath:      mount.VMPath,
			ReadOnly:    mount.ReadOnly,
			Passthrough: mount.Passthrough,
		})
	}
	return result
}
