//go:build linux

package ops

import (
	"fmt"
	"testing"
)

func TestWorkspaceManagerSetupAllSkipsFailedMounts(t *testing.T) {
	t.Cleanup(func() {
		SetGlobalWorkspaceMounts(nil)
	})

	manager := NewWorkspaceManager(&PathConfig{})
	manager.setupFunc = func(mount MountConfig) (*SetupResult, error) {
		if mount.MountPath == "/Applications" {
			return nil, fmt.Errorf("boom")
		}
		return &SetupResult{MountedPath: mount.MountPath}, nil
	}

	result, err := manager.SetupAll([]MountConfig{
		{VirtioTag: "mount-0", MountPath: "/Users/tester"},
		{VirtioTag: "mount-1", MountPath: "/Applications", ReadOnly: true},
		{VirtioTag: "mount-2", MountPath: "/tmp/workspace", Passthrough: true},
	})
	if err != nil {
		t.Fatalf("SetupAll returned error: %v", err)
	}
	if len(result.Results) != 2 {
		t.Fatalf("expected 2 mounted workspaces, got %d", len(result.Results))
	}

	mounts := GetGlobalWorkspaceMounts()
	if len(mounts) != 2 {
		t.Fatalf("expected 2 global workspace mounts, got %d", len(mounts))
	}
	if mounts[0].MountPath != "/Users/tester" {
		t.Fatalf("unexpected first mounted path: %q", mounts[0].MountPath)
	}
	if mounts[1].MountPath != "/tmp/workspace" {
		t.Fatalf("unexpected second mounted path: %q", mounts[1].MountPath)
	}
}

func TestWorkspaceManagerSetupAllFailsWhenNoMountSucceeds(t *testing.T) {
	t.Cleanup(func() {
		SetGlobalWorkspaceMounts(nil)
	})

	manager := NewWorkspaceManager(&PathConfig{})
	manager.setupFunc = func(mount MountConfig) (*SetupResult, error) {
		return nil, fmt.Errorf("boom")
	}

	if _, err := manager.SetupAll([]MountConfig{{VirtioTag: "mount-0", MountPath: "/tmp/workspace"}}); err == nil {
		t.Fatal("expected SetupAll to fail when every mount is skipped")
	}
	if mounts := GetGlobalWorkspaceMounts(); len(mounts) != 0 {
		t.Fatalf("expected no global workspace mounts after total failure, got %d", len(mounts))
	}
}
