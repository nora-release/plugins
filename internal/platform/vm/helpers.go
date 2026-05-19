package vm

import (
	"context"
	"fmt"
	"log"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"
)

// InitOptions contains options for VM initialization
type InitOptions struct {
	Mounts           []Mount // Directory mounts
	ResourcesDir     string  // Path to VM resources (kernel, rootfs)
	RootfsOverlayDir string  // Directory to store temporary overlay files (optional)
	OnShutdown       func()  // Optional callback before VM shutdown
}

// InitResult contains the result of VM initialization
type InitResult struct {
	Manager *Manager
	Mounts  []Mount // The configured mounts
}

// InitializeVM is a high-level helper that creates, configures, and starts a VM.
// It also sets up signal handlers for graceful shutdown.
//
// This function:
// - Resolves the resources directory path
// - Creates VM config with shared workspace
// - Creates and starts the VM manager
// - Sets up signal handlers (SIGINT, SIGTERM) for graceful shutdown
// - Returns the initialized manager and executor
//
// Example usage:
//
//	result, err := vm.InitializeVM(vm.InitOptions{
//	    Mounts: []vm.Mount{{HostPath: "/path/to/project", VMPath: "/path/to/project"}},
//	    ResourcesDir: "resources",
//	})
//	if err != nil {
//	    log.Fatal(err)
//	}
//	defer result.Manager.Stop(context.Background())
func InitializeVM(opts InitOptions) (*InitResult, error) {
	log.Println("🚀 Initializing VM...")

	mounts := opts.Mounts
	if len(mounts) == 0 {
		return nil, fmt.Errorf("at least one mount must be specified")
	}

	// Resolve resources directory
	resourcesDir := opts.ResourcesDir
	if !filepath.IsAbs(resourcesDir) {
		// Use first mount's host path as base for relative resources path
		baseDir := ""
		if len(mounts) > 0 {
			baseDir = mounts[0].HostPath
		}
		if baseDir != "" {
			resourcesDir = filepath.Join(baseDir, resourcesDir)
		}
	}

	// Create VM config
	config := DefaultConfig(resourcesDir)
	config.Mounts = mounts
	if opts.RootfsOverlayDir != "" {
		config.RootfsOverlayDir = opts.RootfsOverlayDir
	}

	// Create VM manager
	manager, err := NewManager(config)
	if err != nil {
		return nil, fmt.Errorf("failed to create VM manager: %w", err)
	}

	// Setup signal handler for graceful shutdown
	SetupSignalHandler(manager, opts.OnShutdown)

	// Start VM
	ctx := context.Background()
	if err := manager.Start(ctx); err != nil {
		return nil, fmt.Errorf("failed to start VM: %w", err)
	}

	log.Println("✅ VM started successfully")

	return &InitResult{
		Manager: manager,
		Mounts:  config.Mounts,
	}, nil
}

// SetupSignalHandler installs signal handlers for graceful VM shutdown.
// Call this when the main process should exit on SIGINT/SIGTERM after stopping the VM.
func SetupSignalHandler(manager *Manager, onShutdown func()) {
	sigChan := make(chan os.Signal, 1)
	signal.Notify(sigChan, os.Interrupt, syscall.SIGTERM)

	go func() {
		<-sigChan
		log.Println("\n⚠️  Received shutdown signal...")

		// Call optional shutdown callback
		if onShutdown != nil {
			onShutdown()
		}

		// Stop VM
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()

		if err := manager.Stop(ctx); err != nil {
			log.Printf("Error stopping VM: %v", err)
		} else {
			log.Println("✅ VM stopped successfully")
		}

		os.Exit(0)
	}()
}
