//go:build linux

package ops

import "testing"

func TestResolveSandboxLowerDirUsesPerSandboxWorkspaceForRemotePassthrough(t *testing.T) {
	t.Parallel()

	base := "/root/.local/state/bridge/sandboxes/sandbox-1"
	mount := MountConfig{
		MountPath:   "/mnt/workspace",
		Passthrough: true,
	}

	got := resolveSandboxLowerDir(base, mount, 0)
	want := "/root/.local/state/bridge/sandboxes/sandbox-1/workspace-0"
	if got != want {
		t.Fatalf("unexpected lower dir: got %q want %q", got, want)
	}
}

func TestResolveSandboxLowerDirKeepsSharedLowerDirForOverlayMounts(t *testing.T) {
	t.Parallel()

	base := "/root/.local/state/bridge/sandboxes/sandbox-1"
	mount := MountConfig{
		VirtioTag: "mount-0",
		MountPath: "/mnt/workspace",
	}

	got := resolveSandboxLowerDir(base, mount, 0)
	want := GetVirtioFSMountPoint("mount-0")
	if got != want {
		t.Fatalf("unexpected lower dir: got %q want %q", got, want)
	}
}

func TestFilterRemoteWorkspaceMountsKeepsOnlyWorkspaceMount(t *testing.T) {
	t.Setenv("VMD_REMOTE", "1")

	mounts := []MountConfig{
		{MountPath: "/mnt/workspace", Passthrough: true},
		{MountPath: "/Users/user/project", Passthrough: true},
		{MountPath: "/Applications", ReadOnly: true},
	}

	filtered := filterRemoteWorkspaceMounts(mounts)
	if len(filtered) != 1 {
		t.Fatalf("unexpected remote mounts length: got %d want 1", len(filtered))
	}
	if filtered[0].MountPath != "/mnt/workspace" {
		t.Fatalf("unexpected remote mount path: got %q", filtered[0].MountPath)
	}
}

func TestResolveHostPathUsesSandboxWorkspaceBackingDirInRemoteMode(t *testing.T) {
	t.Setenv("VMD_REMOTE", "1")

	sandbox := &Sandbox{
		ID:               "sandbox-1",
		workspaceMounted: true,
		Overlays: []*OverlayMount{
			{
				MountConfig: MountConfig{
					MountPath:   "/mnt/workspace",
					Passthrough: true,
				},
				LowerDir: "/root/.local/state/bridge/sandboxes/sandbox-1/workspace-0",
			},
		},
	}

	got, err := sandbox.ResolveHostPath("/mnt/workspace/src/main.go")
	if err != nil {
		t.Fatalf("ResolveHostPath returned error: %v", err)
	}

	want := "/root/.local/state/bridge/sandboxes/sandbox-1/workspace-0/src/main.go"
	if got != want {
		t.Fatalf("unexpected resolved path: got %q want %q", got, want)
	}
}

func TestResolveHostPathUsesLiveVMPathOutsideWorkspaceInRemoteMode(t *testing.T) {
	t.Setenv("VMD_REMOTE", "1")

	sandbox := &Sandbox{workspaceMounted: true}
	got, err := sandbox.ResolveHostPath("/etc/hosts")
	if err != nil {
		t.Fatalf("ResolveHostPath returned error: %v", err)
	}
	if got != "/etc/hosts" {
		t.Fatalf("unexpected resolved path: got %q want %q", got, "/etc/hosts")
	}
}
