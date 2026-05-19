package runtimepath

import (
	"os"
	"path/filepath"
	"strings"
)

// Layout defines the filesystem layout rooted at one Nora runtime directory.
type Layout struct {
	rootDir string
}

func New(rootDir string) Layout {
	rootDir = strings.TrimSpace(rootDir)
	if rootDir == "" {
		rootDir = DefaultRootDir()
	}
	return Layout{rootDir: filepath.Clean(rootDir)}
}

func DefaultRootDir() string {
	home, err := os.UserHomeDir()
	if err != nil || home == "" {
		return filepath.Join(os.TempDir(), "Nora", "sandbox-vm")
	}
	return filepath.Join(home, ".nora", "sandbox-vm")
}

func (l Layout) RootDir() string {
	return l.rootDir
}

func (l Layout) SessionsDir() string {
	return filepath.Join(l.rootDir, "sessions")
}

func (l Layout) EnvironmentStorePath() string {
	return filepath.Join(l.rootDir, "environment-store.json")
}

func (l Layout) EnvhostDir() string {
	return filepath.Join(l.rootDir, "envhost")
}

func (l Layout) WorkspaceDir() string {
	return filepath.Join(l.rootDir, "workspace")
}
