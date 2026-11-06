//go:build !linux

package main

import (
	"fmt"
	"os"
)

func main() {
	fmt.Fprintln(os.Stderr, "vm-init only runs on Linux")
	os.Exit(1)
}
