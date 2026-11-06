package sandboxvm

import (
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/nora/sandbox-vm/internal/localconnector"
)

func TestNewSharedLocalConnectorRuntimeReusesScopeAndRefCounts(t *testing.T) {
	resetSharedLocalConnectorRegistryForTest()

	runtime := &localconnector.SharedRuntime{}
	var createCalls atomic.Int32
	var closeCalls atomic.Int32

	oldNew := newSharedLocalConnectorRuntime
	oldClose := closeSharedLocalConnector
	newSharedLocalConnectorRuntime = func(cfg localconnector.Config) (*localconnector.SharedRuntime, error) {
		createCalls.Add(1)
		return runtime, nil
	}
	closeSharedLocalConnector = func(got *localconnector.SharedRuntime) error {
		if got != runtime {
			t.Fatalf("unexpected runtime closed: %p", got)
		}
		closeCalls.Add(1)
		return nil
	}
	t.Cleanup(func() {
		newSharedLocalConnectorRuntime = oldNew
		closeSharedLocalConnector = oldClose
		resetSharedLocalConnectorRegistryForTest()
	})

	cfg := &LocalConnectorConfig{
		MetadataDir: "/tmp/metadata",
		RootPath:    "/tmp/root",
		KernelPath:  "/tmp/kernel.bin",
		RootfsPath:  "/tmp/rootfs.img",
	}

	first, err := NewSharedLocalConnectorRuntime(cfg)
	if err != nil {
		t.Fatalf("first NewSharedLocalConnectorRuntime: %v", err)
	}
	second, err := NewSharedLocalConnectorRuntime(cfg)
	if err != nil {
		t.Fatalf("second NewSharedLocalConnectorRuntime: %v", err)
	}

	if got := createCalls.Load(); got != 1 {
		t.Fatalf("expected one shared runtime creation, got %d", got)
	}

	firstRuntime, err := first.requireSharedRuntime()
	if err != nil {
		t.Fatalf("first requireSharedRuntime: %v", err)
	}
	secondRuntime, err := second.requireSharedRuntime()
	if err != nil {
		t.Fatalf("second requireSharedRuntime: %v", err)
	}
	if firstRuntime != runtime || secondRuntime != runtime {
		t.Fatalf("expected both wrappers to reuse the same shared runtime")
	}

	if err := first.Close(); err != nil {
		t.Fatalf("first Close: %v", err)
	}
	if got := closeCalls.Load(); got != 0 {
		t.Fatalf("expected shared runtime to stay open until final lease closes, got %d closes", got)
	}
	if err := second.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
	if got := closeCalls.Load(); got != 1 {
		t.Fatalf("expected one shared runtime close, got %d", got)
	}
}

func TestNewSharedLocalConnectorRuntimeConcurrentSingleCreation(t *testing.T) {
	resetSharedLocalConnectorRegistryForTest()

	runtime := &localconnector.SharedRuntime{}
	var createCalls atomic.Int32
	var closeCalls atomic.Int32

	oldNew := newSharedLocalConnectorRuntime
	oldClose := closeSharedLocalConnector
	newSharedLocalConnectorRuntime = func(cfg localconnector.Config) (*localconnector.SharedRuntime, error) {
		createCalls.Add(1)
		time.Sleep(25 * time.Millisecond)
		return runtime, nil
	}
	closeSharedLocalConnector = func(got *localconnector.SharedRuntime) error {
		if got != runtime {
			t.Fatalf("unexpected runtime closed: %p", got)
		}
		closeCalls.Add(1)
		return nil
	}
	t.Cleanup(func() {
		newSharedLocalConnectorRuntime = oldNew
		closeSharedLocalConnector = oldClose
		resetSharedLocalConnectorRegistryForTest()
	})

	cfg := &LocalConnectorConfig{
		MetadataDir: "/tmp/metadata",
		RootPath:    "/tmp/root",
		KernelPath:  "/tmp/kernel.bin",
		RootfsPath:  "/tmp/rootfs.img",
	}

	const goroutines = 8
	runtimes := make([]*SharedLocalConnectorRuntime, goroutines)
	errs := make([]error, goroutines)

	var wg sync.WaitGroup
	for i := 0; i < goroutines; i++ {
		wg.Add(1)
		go func(index int) {
			defer wg.Done()
			runtimes[index], errs[index] = NewSharedLocalConnectorRuntime(cfg)
		}(i)
	}
	wg.Wait()

	for i, err := range errs {
		if err != nil {
			t.Fatalf("goroutine %d NewSharedLocalConnectorRuntime: %v", i, err)
		}
	}
	if got := createCalls.Load(); got != 1 {
		t.Fatalf("expected one concurrent shared runtime creation, got %d", got)
	}

	for i, runtime := range runtimes {
		if err := runtime.Close(); err != nil {
			t.Fatalf("runtime %d Close: %v", i, err)
		}
	}
	if got := closeCalls.Load(); got != 1 {
		t.Fatalf("expected one shared runtime close after all leases release, got %d", got)
	}
}

func TestNewSharedLocalConnectorRuntimeScopesDifferByRoot(t *testing.T) {
	resetSharedLocalConnectorRegistryForTest()

	firstRuntime := &localconnector.SharedRuntime{}
	secondRuntime := &localconnector.SharedRuntime{}
	var createCalls atomic.Int32
	var closeCalls atomic.Int32

	oldNew := newSharedLocalConnectorRuntime
	oldClose := closeSharedLocalConnector
	newSharedLocalConnectorRuntime = func(cfg localconnector.Config) (*localconnector.SharedRuntime, error) {
		call := createCalls.Add(1)
		if call == 1 {
			return firstRuntime, nil
		}
		return secondRuntime, nil
	}
	closeSharedLocalConnector = func(runtime *localconnector.SharedRuntime) error {
		closeCalls.Add(1)
		return nil
	}
	t.Cleanup(func() {
		newSharedLocalConnectorRuntime = oldNew
		closeSharedLocalConnector = oldClose
		resetSharedLocalConnectorRegistryForTest()
	})

	first, err := NewSharedLocalConnectorRuntime(&LocalConnectorConfig{
		MetadataDir: "/tmp/metadata",
		RootPath:    "/tmp/root-a",
		KernelPath:  "/tmp/kernel.bin",
		RootfsPath:  "/tmp/rootfs.img",
	})
	if err != nil {
		t.Fatalf("first NewSharedLocalConnectorRuntime: %v", err)
	}
	second, err := NewSharedLocalConnectorRuntime(&LocalConnectorConfig{
		MetadataDir: "/tmp/metadata",
		RootPath:    "/tmp/root-b",
		KernelPath:  "/tmp/kernel.bin",
		RootfsPath:  "/tmp/rootfs.img",
	})
	if err != nil {
		t.Fatalf("second NewSharedLocalConnectorRuntime: %v", err)
	}

	if got := createCalls.Load(); got != 2 {
		t.Fatalf("expected separate shared runtime creation per scope, got %d", got)
	}
	if got, err := first.requireSharedRuntime(); err != nil || got != firstRuntime {
		t.Fatalf("expected first scope runtime, got %p err=%v", got, err)
	}
	if got, err := second.requireSharedRuntime(); err != nil || got != secondRuntime {
		t.Fatalf("expected second scope runtime, got %p err=%v", got, err)
	}

	if err := first.Close(); err != nil {
		t.Fatalf("first Close: %v", err)
	}
	if err := second.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
	if got := closeCalls.Load(); got != 2 {
		t.Fatalf("expected one close per scope, got %d", got)
	}
}

func TestNewSharedLocalConnectorRuntimeIgnoresRuntimeCapsForScope(t *testing.T) {
	resetSharedLocalConnectorRegistryForTest()

	runtime := &localconnector.SharedRuntime{}
	var createCalls atomic.Int32
	var closeCalls atomic.Int32

	oldNew := newSharedLocalConnectorRuntime
	oldClose := closeSharedLocalConnector
	newSharedLocalConnectorRuntime = func(cfg localconnector.Config) (*localconnector.SharedRuntime, error) {
		createCalls.Add(1)
		return runtime, nil
	}
	closeSharedLocalConnector = func(got *localconnector.SharedRuntime) error {
		closeCalls.Add(1)
		return nil
	}
	t.Cleanup(func() {
		newSharedLocalConnectorRuntime = oldNew
		closeSharedLocalConnector = oldClose
		resetSharedLocalConnectorRegistryForTest()
	})

	first, err := NewSharedLocalConnectorRuntime(&LocalConnectorConfig{
		MetadataDir:        "/tmp/metadata",
		RootPath:           "/tmp/root",
		KernelPath:         "/tmp/kernel.bin",
		RootfsPath:         "/tmp/rootfs.img",
		RootfsOverlayDir:   "/tmp/overlay",
		ReadMaxBytes:       1,
		MaxMatches:         10,
		ExecOutputMaxBytes: 100,
	})
	if err != nil {
		t.Fatalf("first NewSharedLocalConnectorRuntime: %v", err)
	}
	second, err := NewSharedLocalConnectorRuntime(&LocalConnectorConfig{
		MetadataDir:        "/tmp/metadata",
		RootPath:           "/tmp/root",
		KernelPath:         "/tmp/kernel.bin",
		RootfsPath:         "/tmp/rootfs.img",
		RootfsOverlayDir:   "/tmp/overlay",
		ReadMaxBytes:       2,
		MaxMatches:         20,
		ExecOutputMaxBytes: 200,
	})
	if err != nil {
		t.Fatalf("second NewSharedLocalConnectorRuntime: %v", err)
	}

	if got := createCalls.Load(); got != 1 {
		t.Fatalf("expected runtime cap differences to reuse one shared runtime, got %d creations", got)
	}

	if err := first.Close(); err != nil {
		t.Fatalf("first Close: %v", err)
	}
	if err := second.Close(); err != nil {
		t.Fatalf("second Close: %v", err)
	}
	if got := closeCalls.Load(); got != 1 {
		t.Fatalf("expected one close for shared scope, got %d", got)
	}
}

func TestNewSharedLocalConnectorRuntimeWaitsForCloseBeforeRecreate(t *testing.T) {
	resetSharedLocalConnectorRegistryForTest()

	firstRuntime := &localconnector.SharedRuntime{}
	secondRuntime := &localconnector.SharedRuntime{}
	var createCalls atomic.Int32
	var closeCalls atomic.Int32
	closeStarted := make(chan struct{})
	releaseClose := make(chan struct{})
	var closeStartedOnce sync.Once

	oldNew := newSharedLocalConnectorRuntime
	oldClose := closeSharedLocalConnector
	newSharedLocalConnectorRuntime = func(cfg localconnector.Config) (*localconnector.SharedRuntime, error) {
		call := createCalls.Add(1)
		if call == 1 {
			return firstRuntime, nil
		}
		return secondRuntime, nil
	}
	closeSharedLocalConnector = func(runtime *localconnector.SharedRuntime) error {
		closeCalls.Add(1)
		closeStartedOnce.Do(func() {
			close(closeStarted)
		})
		<-releaseClose
		return nil
	}
	t.Cleanup(func() {
		newSharedLocalConnectorRuntime = oldNew
		closeSharedLocalConnector = oldClose
		resetSharedLocalConnectorRegistryForTest()
	})

	cfg := &LocalConnectorConfig{
		MetadataDir: "/tmp/metadata",
		RootPath:    "/tmp/root",
		KernelPath:  "/tmp/kernel.bin",
		RootfsPath:  "/tmp/rootfs.img",
	}

	first, err := NewSharedLocalConnectorRuntime(cfg)
	if err != nil {
		t.Fatalf("first NewSharedLocalConnectorRuntime: %v", err)
	}

	closeDone := make(chan error, 1)
	go func() {
		closeDone <- first.Close()
	}()

	<-closeStarted

	type acquireResult struct {
		runtime *SharedLocalConnectorRuntime
		err     error
	}
	acquireDone := make(chan acquireResult, 1)
	go func() {
		runtime, err := NewSharedLocalConnectorRuntime(cfg)
		acquireDone <- acquireResult{runtime: runtime, err: err}
	}()

	time.Sleep(50 * time.Millisecond)
	select {
	case result := <-acquireDone:
		t.Fatalf("expected acquire to wait for close completion, got runtime=%v err=%v", result.runtime, result.err)
	default:
	}

	close(releaseClose)

	if err := <-closeDone; err != nil {
		t.Fatalf("first Close: %v", err)
	}

	result := <-acquireDone
	if result.err != nil {
		t.Fatalf("second NewSharedLocalConnectorRuntime: %v", result.err)
	}
	t.Cleanup(func() {
		if result.runtime != nil {
			_ = result.runtime.Close()
		}
	})

	if got := createCalls.Load(); got != 2 {
		t.Fatalf("expected recreate after close completes, got %d creations", got)
	}
	if got := closeCalls.Load(); got != 1 {
		t.Fatalf("expected one close while recreating, got %d", got)
	}
	if got, err := result.runtime.requireSharedRuntime(); err != nil || got != secondRuntime {
		t.Fatalf("expected recreated runtime after close, got %p err=%v", got, err)
	}
}

func resetSharedLocalConnectorRegistryForTest() {
	sharedRuntimeRegistryMu.Lock()
	defer sharedRuntimeRegistryMu.Unlock()
	sharedRuntimeRegistry = map[string]*sharedRuntimeRegistryEntry{}
}
