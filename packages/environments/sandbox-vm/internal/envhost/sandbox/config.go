package sandbox

import (
	"context"
	"fmt"
	"log"
	"strings"
	"time"

	"github.com/nora/sandbox-vm/internal/platform/vm"
)

type Mount = vm.Mount

// VMOptions configures sandbox VM creation.
type VMOptions struct {
	// ResourcesDir: path containing kernel.bin, rootfs.img.
	// Ignored when KernelPath is set.
	ResourcesDir string

	// KernelPath, RootfsPath: explicit resource paths.
	// When set, ResourcesDir is ignored.
	KernelPath string
	RootfsPath string

	RootfsOverlayDir string
	Mounts           []Mount
	WorkspaceDir     string

	HTTPProxy  string
	HTTPSProxy string
	NoProxy    string

	RuntimeBridgeGuestPort uint32
}

// InProcessHostConfig configures one VM-backed in-process sandbox host.
type InProcessHostConfig struct {
	MetadataDir          string
	WorkspaceDir         string
	VMOptions            *VMOptions
	InstallSignalHandler bool
	HostID               string
	HostName             string
	HostLabel            string
}

// ApplyHostProxyFromEnv reads host proxy env vars and applies them when safe.
func ApplyHostProxyFromEnv(opts *VMOptions) {
	if opts == nil {
		return
	}
	httpProxy, httpsProxy, noProxy := vm.GetHostProxyEnv()
	if httpProxy == "" && httpsProxy == "" {
		return
	}
	if strings.Contains(httpProxy, "127.0.0.1") || strings.Contains(httpsProxy, "127.0.0.1") ||
		strings.Contains(httpProxy, "localhost") || strings.Contains(httpsProxy, "localhost") {
		log.Printf("⚠️  Host proxy uses localhost address which may not work in VM")
		return
	}
	opts.HTTPProxy = httpProxy
	opts.HTTPSProxy = httpsProxy
	opts.NoProxy = noProxy
}

func createVMManager(ctx context.Context, opts *VMOptions) (*vm.Manager, error) {
	if opts == nil {
		return nil, nil
	}

	var vmConfig *vm.Config
	if opts.KernelPath != "" {
		vmConfig = vm.DefaultConfig("")
		vmConfig.KernelPath = opts.KernelPath
		vmConfig.RootfsPath = opts.RootfsPath
	} else {
		vmConfig = vm.DefaultConfig(opts.ResourcesDir)
	}

	vmConfig.Mounts = opts.Mounts
	vmConfig.RootfsOverlayDir = opts.RootfsOverlayDir
	vmConfig.HTTPProxy = opts.HTTPProxy
	vmConfig.HTTPSProxy = opts.HTTPSProxy
	vmConfig.NoProxy = opts.NoProxy
	vmConfig.RuntimeBridgeGuestPort = opts.RuntimeBridgeGuestPort

	manager, err := vm.NewManager(vmConfig)
	if err != nil {
		return nil, fmt.Errorf("create vm manager: %w", err)
	}

	log.Println("🚀 Initializing VM...")
	startCtx, cancel := context.WithTimeout(ctx, 60*time.Second)
	defer cancel()
	if err := manager.Start(startCtx); err != nil {
		stopCtx, stopCancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer stopCancel()
		_ = manager.Stop(stopCtx)
		return nil, fmt.Errorf("start vm: %w", err)
	}
	log.Println("✅ VM started successfully")

	return manager, nil
}
