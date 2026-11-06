//go:build linux

// Package init implements the VM init process.
// This replaces the shell-based init.sh with a Go implementation.
package init

import (
	"context"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	_ "net/http/pprof"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"
	"unsafe"

	"github.com/nora/sandbox-vm/internal/platform/vm/guestnetwork"
	"github.com/nora/sandbox-vm/internal/platform/vm/netstack/frame"
	"github.com/nora/sandbox-vm/internal/platform/vm/vmd/ops"
	"github.com/nora/sandbox-vm/internal/platform/vm/vmd/server"
	"github.com/nora/sandbox-vm/internal/platform/vm/vmrpc"
	"golang.org/x/sys/unix"
)

const (
	defaultRuntimeBridgeGuestPort uint32 = 50080
)

// Run is the main entry point for the init process.
// When VMD_REMOTE=1, it runs as a systemd service (not PID 1) and only starts
// the gRPC sandbox daemon on TCP. Otherwise it performs full PID-1 init.
func Run() error {
	log.SetFlags(log.LstdFlags | log.Lshortfile)
	log.Println("VM init starting...")

	// Check remote mode early, before any PID-1-specific init steps.
	// In remote mode (fly-vault), systemd is PID 1 and handles /proc, networking,
	// filesystems, etc. vmd only needs to run the sandbox daemon on TCP.
	if os.Getenv("VMD_REMOTE") == "1" {
		return runRemote()
	}

	// Mount /proc early to read cmdline
	if err := mountProc(); err != nil {
		return fmt.Errorf("mount /proc: %w", err)
	}

	log.Println("VM boot OK!")

	// Check build mode from cmdline
	buildMode := checkBuildMode()
	if buildMode {
		log.Println("==> Builder mode enabled")
	}

	// Setup overlay filesystem
	if err := setupOverlay(buildMode); err != nil {
		return fmt.Errorf("setup overlay: %w", err)
	}

	// Pivot root
	if err := pivotRoot(); err != nil {
		return fmt.Errorf("pivot root: %w", err)
	}

	log.Println("==> Init Starting...")

	// Mount essential filesystems
	if err := mountFilesystems(); err != nil {
		return fmt.Errorf("mount filesystems: %w", err)
	}

	// Setup hostname and hosts
	setupHostname()

	// Setup network
	if err := setupNetwork(); err != nil {
		log.Printf("Warning: network setup failed: %v", err)
	}

	// Setup machine ID
	setupMachineID()

	// Setup messagebus user
	setupMessagebusUser()

	// Divert systemd-sysusers
	divertSysusers()

	// Start guest-local support services (netd, runtime bridge).
	startSupportServices()

	// Start sandbox daemon in goroutine
	go runSandboxDaemon()

	// Start pprof server for debugging
	go runPprofServer()

	log.Println("==> System ready")

	// Handle signals and reap zombies (PID 1 responsibilities)
	handleSignals()

	return nil
}

// runRemote runs vmd in remote mode as a systemd service.
// Systemd handles system init; vmd only starts the gRPC sandbox daemon on TCP.
func runRemote() error {
	log.Println("==> Remote mode enabled (VMD_REMOTE=1)")

	// Ensure remote runtime directories exist.
	for _, dir := range []string{
		"/mnt/workspace",
		"/mnt/sandbox",
		"/root/.local/state/bridge/sandboxes",
	} {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			log.Printf("Warning: failed to create %s: %v", dir, err)
		}
	}
	log.Println("==> Using persistent sandbox storage at /root/.local/state/bridge/sandboxes")

	// Register default workspace mount so CreateSandbox works before the host
	// has a chance to call SetupWorkspaces. In remote mode there is no VirtioFS,
	// so each sandbox bind-mounts its own managed workspace directory from
	// /root/.local/state/bridge/sandboxes/<sandbox-id>/workspace-* at /mnt/workspace.
	ops.SetGlobalWorkspaceMounts([]ops.MountConfig{
		{
			VirtioTag:   "", // no VirtioFS in remote mode
			MountPath:   "/mnt/workspace",
			Passthrough: true,
		},
	})
	log.Println("==> Registered default workspace mount at /mnt/workspace")

	// Start pprof server for debugging
	go runPprofServer()

	// Run sandbox daemon on TCP (blocks forever)
	runSandboxDaemonRemote()
	return nil
}

func mountProc() error {
	return syscall.Mount("proc", "/proc", "proc", 0, "")
}

func checkBuildMode() bool {
	cmdline := readCmdline()
	log.Printf("Cmdline: %s", strings.TrimSpace(cmdline))
	return strings.Contains(cmdline, "cuebuilder=1")
}

func readCmdline() string {
	data, err := os.ReadFile("/proc/cmdline")
	if err != nil {
		return ""
	}
	return string(data)
}

func runtimeBridgeGuestPort() uint32 {
	return cmdlineUint32("cue_rt_guest_port", defaultRuntimeBridgeGuestPort)
}

func cmdlineUint32(key string, defaultValue uint32) uint32 {
	key = strings.TrimSpace(key)
	if key == "" {
		return defaultValue
	}

	prefix := key + "="
	for _, field := range strings.Fields(readCmdline()) {
		if !strings.HasPrefix(field, prefix) {
			continue
		}
		value, err := strconv.ParseUint(strings.TrimPrefix(field, prefix), 10, 32)
		if err != nil || value == 0 {
			return defaultValue
		}
		return uint32(value)
	}
	return defaultValue
}

func setupOverlay(buildMode bool) error {
	// Mount tmpfs on /mnt
	if err := syscall.Mount("tmpfs", "/mnt", "tmpfs", 0, ""); err != nil {
		return fmt.Errorf("mount tmpfs on /mnt: %w", err)
	}

	if err := os.MkdirAll("/mnt/overlay", 0755); err != nil {
		return err
	}

	lowerDir := "/"
	ephemeralDevice := "/dev/vdb"

	// Mount ephemeral overlay for writable layer
	log.Printf("==> Found ephemeral overlay device %s", ephemeralDevice)
	if err := os.MkdirAll("/mnt/temp-overlay", 0755); err != nil {
		log.Printf("ERROR: Failed to create /mnt/temp-overlay: %v", err)
		return err
	} else if err := mountWithOptionalFormat(ephemeralDevice, "/mnt/temp-overlay", ephemeralDevice); err != nil {
		log.Printf("ERROR: Failed to mount %s: %v, falling back to tmpfs", ephemeralDevice, err)
		return err
	}

	upperDir := "/mnt/temp-overlay/upper"
	workDir := "/mnt/temp-overlay/work"
	log.Println("==> Using ephemeral overlay for writable layer")

	// Create directories
	if err := os.MkdirAll(upperDir, 0755); err != nil {
		return err
	}
	if err := os.MkdirAll(workDir, 0755); err != nil {
		return err
	}

	// Mount overlay
	opts := fmt.Sprintf("lowerdir=%s,upperdir=%s,workdir=%s", lowerDir, upperDir, workDir)
	if err := syscall.Mount("overlay", "/mnt/overlay", "overlay", 0, opts); err != nil {
		return fmt.Errorf("mount overlay: %w", err)
	}

	// Move mount to new root
	os.MkdirAll("/mnt/overlay/mnt/temp-overlay", 0755)
	syscall.Mount("/mnt/temp-overlay", "/mnt/overlay/mnt/temp-overlay", "", syscall.MS_MOVE, "")

	return nil
}

func mountWithOptionalFormat(device, mountpoint, label string) error {
	// Try to mount directly first (with discard for TRIM support)
	if err := syscall.Mount(device, mountpoint, "ext4", 0, "discard"); err == nil {
		log.Printf("==> Mounted %s with discard", label)
		// Sync to ensure journal recovery is complete
		syscall.Sync()
		return nil
	}

	// Try auto-detect filesystem
	if err := syscall.Mount(device, mountpoint, "", 0, ""); err == nil {
		log.Printf("==> Mounted %s", label)
		return nil
	}

	// Format and mount
	log.Printf("==> Formatting %s...", label)
	// Use absolute path because PATH may not be set yet during early boot
	cmd := exec.Command("/sbin/mkfs.ext4", "-F", device)
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("mkfs.ext4: %w", err)
	}

	if err := syscall.Mount(device, mountpoint, "ext4", 0, "discard"); err != nil {
		return fmt.Errorf("mount after format: %w", err)
	}

	log.Printf("==> Mounted %s with discard", label)
	return nil
}

func fileExists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

func isEROFS(path string) bool {
	data, err := os.ReadFile("/proc/mounts")
	if err != nil {
		return false
	}
	return strings.Contains(string(data), path+" erofs")
}

func moveMounts() {
	moves := []struct {
		src, dst string
	}{
		{"/mnt/temp-overlay", "/mnt/overlay/mnt/temp-overlay"},
	}

	for _, m := range moves {
		if isMountpoint(m.src) {
			os.MkdirAll(m.dst, 0755)
			syscall.Mount(m.src, m.dst, "", syscall.MS_MOVE, "")
		}
	}
}

func isMountpoint(path string) bool {
	// Use stat to check if path is on different device than parent
	// This avoids depending on mountpoint command and PATH being set
	pathStat, err := os.Stat(path)
	if err != nil {
		return false
	}
	parentStat, err := os.Stat(filepath.Dir(path))
	if err != nil {
		return false
	}
	pathSys, ok1 := pathStat.Sys().(*syscall.Stat_t)
	parentSys, ok2 := parentStat.Sys().(*syscall.Stat_t)
	if !ok1 || !ok2 {
		return false
	}
	return pathSys.Dev != parentSys.Dev
}

func pivotRoot() error {
	syscall.Unmount("/dev", 0)
	if err := syscall.PivotRoot("/mnt/overlay", "/mnt/overlay"); err != nil {
		return err
	}
	os.Setenv("PATH", "/sbin:/bin:/usr/sbin:/usr/bin")
	os.Setenv("DEBIAN_FRONTEND", "noninteractive")
	return nil
}

func mountFilesystems() error {
	mounts := []struct {
		source, target, fstype string
		flags                  uintptr
		data                   string
	}{
		{"proc", "/proc", "proc", 0, ""},
		{"sys", "/sys", "sysfs", 0, ""},
		{"none", "/sys/fs/cgroup", "cgroup2", 0, ""},
		{"dev", "/dev", "devtmpfs", 0, ""},
		{"tmpfs", "/dev/shm", "tmpfs", 0, ""},
		{"tmpfs", "/tmp", "tmpfs", 0, ""},
		{"tmpfs", "/run", "tmpfs", 0, ""},
		{"devpts", "/dev/pts", "devpts", 0, "mode=0620,gid=5"},
	}

	for _, m := range mounts {
		os.MkdirAll(m.target, 0755)
		if err := syscall.Mount(m.source, m.target, m.fstype, m.flags, m.data); err != nil {
			// Non-fatal, just log
			log.Printf("mount %s: %v", m.target, err)
		}
	}

	os.MkdirAll("/run/lock", 0755)

	// Fix mtab
	os.Remove("/etc/mtab")
	os.Symlink("/proc/mounts", "/etc/mtab")

	return nil
}

func setupHostname() {
	syscall.Sethostname([]byte("vm"))

	f, err := os.OpenFile("/etc/hosts", os.O_APPEND|os.O_WRONLY|os.O_CREATE, 0644)
	if err == nil {
		f.WriteString("127.0.0.1 localhost\n")
		f.Close()
	}
}

const (
	tunIfName                   = "tun0"
	tunGuestCIDR                = guestnetwork.GuestCIDR
	defaultNetdMTU              = 1500
	tunDeviceNodePath           = "/dev/net/tun"
	dnsPreflightStartupWindow   = 90 * time.Second
	dnsPreflightRetryInterval   = 2 * time.Second
	dnsPreflightResolverTimeout = 3 * time.Second
	dnsPreflightQueryTimeout    = 2 * time.Second
)

var (
	tunDNSResolvers     = []string{"1.1.1.1", "9.9.9.9", "8.8.8.8"}
	dnsPreflightDomains = []string{"github.com"}
)

func setupNetwork() error {
	log.Println("==> Configuring network...")

	// ip link set lo up
	if err := exec.Command("ip", "link", "set", "lo", "up").Run(); err != nil {
		return fmt.Errorf("lo up: %w", err)
	}
	if err := setupTunInterface(); err != nil {
		return fmt.Errorf("setup tun interface: %w", err)
	}
	return nil
}

func setupTunInterface() error {
	if err := ensureTunDeviceNode(); err != nil {
		return err
	}

	// Create tun0 (idempotent; may fail if already created by a previous run).
	_ = exec.Command("ip", "tuntap", "add", "dev", tunIfName, "mode", "tun").Run()

	if err := exec.Command("ip", "link", "set", tunIfName, "up").Run(); err != nil {
		return fmt.Errorf("bring %s up: %w", tunIfName, err)
	}
	if err := exec.Command("ip", "addr", "replace", tunGuestCIDR, "dev", tunIfName).Run(); err != nil {
		return fmt.Errorf("configure %s address: %w", tunIfName, err)
	}
	if err := exec.Command("ip", "route", "replace", "default", "dev", tunIfName).Run(); err != nil {
		return fmt.Errorf("configure default route: %w", err)
	}

	if err := writeResolvConf(tunDNSResolvers); err != nil {
		return fmt.Errorf("write /etc/resolv.conf: %w", err)
	}
	return nil
}

func writeResolvConf(resolvers []string) error {
	resolvConf, err := renderResolvConf(resolvers)
	if err != nil {
		return err
	}
	return os.WriteFile("/etc/resolv.conf", []byte(resolvConf), 0o644)
}

func renderResolvConf(resolvers []string) (string, error) {
	resolvers = normalizeDNSResolvers(resolvers)
	if len(resolvers) == 0 {
		return "", fmt.Errorf("no DNS resolvers configured")
	}

	var b strings.Builder
	for _, resolver := range resolvers {
		fmt.Fprintf(&b, "nameserver %s\n", resolver)
	}
	return b.String(), nil
}

func normalizeDNSResolvers(resolvers []string) []string {
	normalized := make([]string, 0, len(resolvers))
	seen := make(map[string]struct{}, len(resolvers))
	for _, resolver := range resolvers {
		resolver = strings.TrimSpace(resolver)
		if resolver == "" {
			continue
		}
		if _, ok := seen[resolver]; ok {
			continue
		}
		seen[resolver] = struct{}{}
		normalized = append(normalized, resolver)
	}
	return normalized
}

func startDNSPreflightLoop() {
	// Startup-only repair: once at least one resolver can reach our backend
	// domains, keep that healthy set for this boot and avoid ongoing DNS churn.
	deadline := time.Now().Add(dnsPreflightStartupWindow)
	for {
		healthy, failures := preflightDNSResolvers(tunDNSResolvers, dnsPreflightDomains)
		if len(healthy) > 0 {
			if sameStringSlice(healthy, normalizeDNSResolvers(tunDNSResolvers)) {
				log.Printf("DNS preflight OK: resolvers=%s domains=%s", strings.Join(healthy, ","), strings.Join(dnsPreflightDomains, ","))
				return
			}
			if err := writeResolvConf(healthy); err != nil {
				log.Printf("DNS preflight: failed to rewrite /etc/resolv.conf with healthy resolvers %s: %v", strings.Join(healthy, ","), err)
				return
			}
			log.Printf("DNS preflight rewrote /etc/resolv.conf: healthy=%s excluded=%s", strings.Join(healthy, ","), strings.Join(failures, "; "))
			return
		}

		if time.Now().After(deadline) {
			log.Printf("DNS preflight failed before startup deadline; keeping default resolvers %s. Last failures: %s", strings.Join(normalizeDNSResolvers(tunDNSResolvers), ","), strings.Join(failures, "; "))
			return
		}
		time.Sleep(dnsPreflightRetryInterval)
	}
}

type dnsResolverLookup func(resolver string, domains []string) error

func preflightDNSResolvers(resolvers []string, domains []string) ([]string, []string) {
	return preflightDNSResolversWithLookup(resolvers, domains, preflightDNSResolver)
}

func preflightDNSResolversWithLookup(resolvers []string, domains []string, lookup dnsResolverLookup) ([]string, []string) {
	resolvers = normalizeDNSResolvers(resolvers)
	healthy := make([]string, 0, len(resolvers))
	failures := make([]string, 0)
	for _, resolver := range resolvers {
		if err := lookup(resolver, domains); err != nil {
			failures = append(failures, fmt.Sprintf("%s: %v", resolver, err))
			continue
		}
		healthy = append(healthy, resolver)
	}
	return healthy, failures
}

func preflightDNSResolver(resolver string, domains []string) error {
	if len(domains) == 0 {
		return nil
	}
	dialer := &net.Dialer{Timeout: dnsPreflightResolverTimeout}
	netResolver := &net.Resolver{
		PreferGo: true,
		Dial: func(ctx context.Context, network, address string) (net.Conn, error) {
			return dialer.DialContext(ctx, network, net.JoinHostPort(resolver, "53"))
		},
	}
	for _, domain := range domains {
		domain = strings.TrimSpace(domain)
		if domain == "" {
			continue
		}
		ctx, cancel := context.WithTimeout(context.Background(), dnsPreflightQueryTimeout)
		_, err := netResolver.LookupHost(ctx, domain)
		cancel()
		if err != nil {
			return fmt.Errorf("lookup %s: %w", domain, err)
		}
	}
	return nil
}

func sameStringSlice(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func setupMachineID() {
	if _, err := os.Stat("/etc/machine-id"); os.IsNotExist(err) {
		log.Println("==> Generating machine-id...")
		os.WriteFile("/etc/machine-id", []byte("b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0\n"), 0444)
	}
}

func setupMessagebusUser() {
	if _, err := exec.LookPath("groupadd"); err != nil {
		return
	}

	log.Println("==> Creating messagebus user...")
	exec.Command("groupadd", "-r", "messagebus").Run()
	exec.Command("useradd", "-r", "-g", "messagebus", "-d", "/run/dbus", "-s", "/bin/false", "messagebus").Run()
	os.MkdirAll("/var/lib/dbus", 0755)
}

func divertSysusers() {
	if _, err := exec.LookPath("dpkg-divert"); err != nil {
		return
	}

	log.Println("==> Diverting systemd-sysusers...")
	exec.Command("dpkg-divert", "--local", "--rename", "--add", "/usr/bin/systemd-sysusers").Run()
	os.Remove("/usr/bin/systemd-sysusers")
	os.Symlink("/bin/true", "/usr/bin/systemd-sysusers")
}

func ensureTunDeviceNode() error {
	if err := os.MkdirAll("/dev/net", 0o755); err != nil {
		return fmt.Errorf("create /dev/net: %w", err)
	}
	if _, err := os.Stat(tunDeviceNodePath); err == nil {
		return nil
	} else if !os.IsNotExist(err) {
		return fmt.Errorf("stat %s: %w", tunDeviceNodePath, err)
	}

	dev := int(unix.Mkdev(10, 200))
	mode := uint32(unix.S_IFCHR | 0o666)
	if err := unix.Mknod(tunDeviceNodePath, mode, dev); err != nil && err != unix.EEXIST {
		return fmt.Errorf("mknod %s: %w", tunDeviceNodePath, err)
	}
	return nil
}

type tunIfreq struct {
	Name  [unix.IFNAMSIZ]byte
	Flags uint16
	_     [40 - unix.IFNAMSIZ - 2]byte
}

func openTunInterface(name string) (*os.File, error) {
	f, err := os.OpenFile(tunDeviceNodePath, os.O_RDWR, 0)
	if err != nil {
		return nil, fmt.Errorf("open %s: %w", tunDeviceNodePath, err)
	}

	var ifr tunIfreq
	copy(ifr.Name[:], name)
	ifr.Flags = unix.IFF_TUN | unix.IFF_NO_PI
	if _, _, errno := unix.Syscall(unix.SYS_IOCTL, f.Fd(), uintptr(unix.TUNSETIFF), uintptr(unsafe.Pointer(&ifr))); errno != 0 {
		f.Close()
		return nil, fmt.Errorf("ioctl TUNSETIFF %s: %w", name, errno)
	}
	return f, nil
}

func startNetdLoop() {
	for {
		conn, err := vmrpc.DialVsock(vmrpc.HostCID, vmrpc.NetstackPort)
		if err != nil {
			log.Printf("netd: connect to host netstack failed: %v", err)
			time.Sleep(time.Second)
			continue
		}

		if err := runNetdSession(conn); err != nil {
			log.Printf("netd: session ended: %v", err)
		}
		_ = conn.Close()
		time.Sleep(time.Second)
	}
}

func runNetdSession(conn io.ReadWriteCloser) error {
	tunFile, err := openTunInterface(tunIfName)
	if err != nil {
		return err
	}
	defer tunFile.Close()

	errCh := make(chan error, 2)
	go func() {
		errCh <- pumpTunToVsock(tunFile, conn)
	}()
	go func() {
		errCh <- pumpVsockToTun(conn, tunFile)
	}()

	err = <-errCh
	_ = conn.Close()
	return err
}

func pumpTunToVsock(tun io.Reader, vsock io.Writer) error {
	buf := make([]byte, defaultNetdMTU)
	for {
		n, err := tun.Read(buf)
		if err != nil {
			return err
		}
		if n == 0 {
			continue
		}
		if err := frame.Write(vsock, buf[:n], defaultNetdMTU); err != nil {
			return err
		}
	}
}

func pumpVsockToTun(vsock io.Reader, tun io.Writer) error {
	for {
		packet, err := frame.Read(vsock, defaultNetdMTU)
		if err != nil {
			return err
		}
		if len(packet) == 0 {
			continue
		}
		if _, err := tun.Write(packet); err != nil {
			return err
		}
	}
}

func startSupportServices() {
	guestPort := runtimeBridgeGuestPort()

	go startNetdLoop()
	go startDNSPreflightLoop()
	go func() {
		server, err := server.StartRuntimeBridgeLoopbackServer(guestPort)
		if err != nil {
			log.Printf("runtime bridge loopback failed: %v", err)
			return
		}
		defer server.Close()
		select {}
	}()
}

func runSandboxDaemon() {
	log.Printf("==> Starting vmd peer...")

	vmService, err := server.NewVMService()
	if err != nil {
		log.Printf("Failed to create VMService: %v", err)
		return
	}

	if err := server.InitGlobalPeer(vmService); err != nil {
		log.Printf("Failed to init global peer: %v", err)
		return
	}

	log.Printf("✓ vmd peer ready (VMService on %d)", vmrpc.VMServicePort)

	// Block forever (peer serves in background)
	select {}
}

func runSandboxDaemonRemote() {
	log.Printf("==> Starting vmd peer (TCP remote mode)...")

	vmService, err := server.NewVMService()
	if err != nil {
		log.Printf("Failed to create VMService: %v", err)
		return
	}

	listenAddr := ":50051"
	if addr := os.Getenv("VMD_LISTEN_ADDR"); addr != "" {
		listenAddr = addr
	}

	if err := server.InitGlobalPeerTCP(vmService, listenAddr); err != nil {
		log.Printf("Failed to init global TCP peer: %v", err)
		return
	}

	log.Printf("✓ vmd peer ready (TCP on %s)", listenAddr)

	// Block forever (peer serves in background)
	select {}
}

func runPprofServer() {
	const pprofAddr = ":6060"
	log.Printf("==> Starting pprof server on %s...", pprofAddr)
	if err := http.ListenAndServe(pprofAddr, nil); err != nil {
		log.Printf("pprof server failed: %v", err)
	}
}

func handleSignals() {
	sigCh := make(chan os.Signal, 1)
	// Handle SIGCHLD for zombie reaping, SIGTERM/SIGINT for shutdown
	// Explicitly ignore SIGQUIT to prevent accidental shutdown from debug signals
	signal.Notify(sigCh, syscall.SIGCHLD, syscall.SIGTERM, syscall.SIGINT)
	signal.Ignore(syscall.SIGQUIT)

	for sig := range sigCh {
		switch sig {
		case syscall.SIGCHLD:
			// Reap zombie processes (PID 1 responsibility)
			for {
				var status syscall.WaitStatus
				pid, err := syscall.Wait4(0, &status, syscall.WNOHANG, nil)
				if pid <= 0 || err != nil {
					break
				}
			}
		case syscall.SIGTERM, syscall.SIGINT:
			log.Println("Received shutdown signal")
			syscall.Reboot(syscall.LINUX_REBOOT_CMD_POWER_OFF)
		}
	}
}
