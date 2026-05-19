//go:build darwin

package vm

// Compile-time interface assertions.
// Guarded by darwin because Manager imports the Apple Virtualization Framework,
// and all files in the vm package must compile together.
var _ SandboxBackend = (*Manager)(nil)
