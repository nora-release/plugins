//go:build linux

// vmd (VM Daemon) is the core process for the VM.
// It runs as PID 1, initializes the system, and listens on vsock for sandbox RPC.
package main

import (
	"log"
	"os"

	vminit "github.com/nora/sandbox-vm/internal/platform/vm/vmd/init"
)

func main() {
	log.SetFlags(log.LstdFlags | log.Lshortfile)

	if os.Getpid() != 1 {
		log.Println("Warning: not running as PID 1, some init features may not work")
	}

	if err := vminit.Run(); err != nil {
		log.Fatalf("Init failed: %v", err)
	}
}
