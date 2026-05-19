package vm

import (
	"context"
	"net"
	"path/filepath"
	"testing"
	"time"

	"github.com/nora/sandbox-vm/internal/platform/telemetry"
	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
	"google.golang.org/grpc"
)

func TestBuildBootCommandIncludesRuntimeBridgePorts(t *testing.T) {
	command := buildBootCommand("console=hvc0 root=/dev/vda quiet", 41000)

	if got, want := command, "console=hvc0 root=/dev/vda quiet cue_rt_guest_port=41000"; got != want {
		t.Fatalf("unexpected boot command:\n got: %q\nwant: %q", got, want)
	}
}

func TestNormalizeConfigSetsRuntimeBridgeDefaults(t *testing.T) {
	cfg := &Config{}
	normalizeConfig(cfg)

	if cfg.RuntimeBridgeGuestPort != DefaultRuntimeBridgeGuestPort {
		t.Fatalf("unexpected guest runtime bridge port: %d", cfg.RuntimeBridgeGuestPort)
	}
}

func TestBuildWorkspaceSetupMountsForLocalVM(t *testing.T) {
	mounts := []Mount{
		{HostPath: "/host/project", VMPath: "/mnt/workspace"},
		{HostPath: "/Applications", VMPath: "/Applications", ReadOnly: true},
	}

	got := buildWorkspaceSetupMounts(mounts, true)
	if len(got) != len(mounts) {
		t.Fatalf("unexpected mount count: got %d want %d", len(got), len(mounts))
	}

	if got[0].GetVirtioTag() != "mount-0" {
		t.Fatalf("unexpected first virtio tag: %q", got[0].GetVirtioTag())
	}
	if got[0].GetMountPath() != "/mnt/workspace" {
		t.Fatalf("unexpected first mount path: %q", got[0].GetMountPath())
	}
	if got[0].GetPassthrough() {
		t.Fatalf("workspace mount should not be passthrough")
	}

	if got[1].GetVirtioTag() != "mount-1" {
		t.Fatalf("unexpected second virtio tag: %q", got[1].GetVirtioTag())
	}
	if !got[1].GetReadOnly() {
		t.Fatalf("expected second mount to be read-only")
	}
}

func TestBuildWorkspaceSetupMountsForRemoteVM(t *testing.T) {
	mounts := []Mount{
		{HostPath: "/host/project", VMPath: "/mnt/workspace", Passthrough: true},
	}

	got := buildWorkspaceSetupMounts(mounts, false)
	if len(got) != 1 {
		t.Fatalf("unexpected mount count: got %d want 1", len(got))
	}
	if got[0].GetVirtioTag() != "" {
		t.Fatalf("remote workspace mount should not use a virtio tag: %q", got[0].GetVirtioTag())
	}
	if !got[0].GetPassthrough() {
		t.Fatalf("remote workspace mount should write directly to the remote filesystem")
	}
	if got[0].GetMountPath() != "/mnt/workspace" {
		t.Fatalf("unexpected remote mount path: %q", got[0].GetMountPath())
	}
}

func TestSelectMountedMountsFiltersFailedMounts(t *testing.T) {
	mounts := []Mount{
		{HostPath: "/Users/tester", VMPath: "/Users/tester"},
		{HostPath: "/Applications", VMPath: "/Applications", ReadOnly: true},
		{HostPath: "/tmp/workspace", VMPath: "/tmp/workspace", Passthrough: true},
	}

	selected := selectMountedMounts(mounts, []*vmrpc.WorkspaceResult{
		{MountedPath: "/Users/tester"},
		{MountedPath: "/tmp/workspace"},
	})
	if len(selected) != 2 {
		t.Fatalf("expected 2 selected mounts, got %d", len(selected))
	}
	if selected[0].VMPath != "/Users/tester" {
		t.Fatalf("unexpected first selected mount: %q", selected[0].VMPath)
	}
	if selected[1].VMPath != "/tmp/workspace" {
		t.Fatalf("unexpected second selected mount: %q", selected[1].VMPath)
	}
}

func TestManagerExecutePythonInjectsTraceContextEnv(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen: %v", err)
	}
	defer listener.Close()

	vmServer := &traceEnvVMServer{}
	grpcServer := grpc.NewServer()
	vmrpc.RegisterVMServiceServer(grpcServer, vmServer)
	go grpcServer.Serve(listener)
	defer grpcServer.Stop()

	peer := vmrpc.NewTCPHostPeer(listener.Addr().String())
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := peer.Connect(ctx); err != nil {
		t.Fatalf("connect peer: %v", err)
	}
	defer peer.Close()

	manager := &Manager{peer: peer}
	traceCtx := telemetry.WithTraceContext(
		context.Background(),
		telemetry.TraceContextFromStrings(
			"00-4bf92f3577b34da6a3ce929d0e0e4736-1111111111111111-01",
			"vendor=value",
		),
	)

	req := &vmrpc.ExecutePythonRequest{
		Code: "print('hello')",
		Env: map[string]string{
			"FOO": "bar",
		},
	}
	if _, err := manager.ExecutePython(traceCtx, req); err != nil {
		t.Fatalf("ExecutePython failed: %v", err)
	}

	if got := vmServer.lastExecutePythonEnv["FOO"]; got != "bar" {
		t.Fatalf("unexpected env passthrough: %q", got)
	}
	if got := vmServer.lastExecutePythonEnv["CUE_TRACEPARENT"]; got != "00-4bf92f3577b34da6a3ce929d0e0e4736-1111111111111111-01" {
		t.Fatalf("unexpected traceparent env: %q", got)
	}
	if got := vmServer.lastExecutePythonEnv["CUE_TRACESTATE"]; got != "vendor=value" {
		t.Fatalf("unexpected tracestate env: %q", got)
	}
}

func TestManagerStopReleasesOverlayLockWithoutRunningVM(t *testing.T) {
	overlayPath := filepath.Join(t.TempDir(), "rootfs-overlay.img")
	lockFile := tryLockOverlay(overlayPath)
	if lockFile == nil {
		t.Fatal("expected initial overlay lock acquisition to succeed")
	}

	manager := &Manager{overlayLockFile: lockFile}
	if err := manager.Stop(context.Background()); err != nil {
		t.Fatalf("Stop returned error: %v", err)
	}

	reacquiredLock := tryLockOverlay(overlayPath)
	if reacquiredLock == nil {
		t.Fatal("expected Stop to release overlay lock for partial startup cleanup")
	}
	unlockOverlay(reacquiredLock)
}

type traceEnvVMServer struct {
	vmrpc.UnimplementedVMServiceServer
	lastExecutePythonEnv map[string]string
}

func (s *traceEnvVMServer) ExecutePython(ctx context.Context, req *vmrpc.ExecutePythonRequest) (*vmrpc.ExecutePythonResponse, error) {
	_ = ctx
	s.lastExecutePythonEnv = cloneStringMap(req.GetEnv())
	return &vmrpc.ExecutePythonResponse{}, nil
}

func cloneStringMap(input map[string]string) map[string]string {
	if input == nil {
		return nil
	}
	cloned := make(map[string]string, len(input))
	for key, value := range input {
		cloned[key] = value
	}
	return cloned
}
