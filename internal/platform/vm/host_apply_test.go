package vm

import (
	"archive/tar"
	"bytes"
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/nora/sandbox-vm/pkg/types"
)

type tarEntry struct {
	name     string
	typeflag byte
	linkname string
	mode     int64
	body     string
}

func buildTar(t *testing.T, entries []tarEntry) *bytes.Reader {
	t.Helper()
	var buffer bytes.Buffer
	writer := tar.NewWriter(&buffer)
	for _, entry := range entries {
		mode := entry.mode
		if mode == 0 {
			mode = 0o644
		}
		header := &tar.Header{
			Name:     entry.name,
			Typeflag: entry.typeflag,
			Linkname: entry.linkname,
			Mode:     mode,
		}
		if entry.typeflag == tar.TypeReg {
			header.Size = int64(len(entry.body))
		}
		if err := writer.WriteHeader(header); err != nil {
			t.Fatal(err)
		}
		if entry.typeflag == tar.TypeReg {
			if _, err := writer.Write([]byte(entry.body)); err != nil {
				t.Fatal(err)
			}
		}
	}
	if err := writer.Close(); err != nil {
		t.Fatal(err)
	}
	return bytes.NewReader(buffer.Bytes())
}

// layout mirrors production: the VM mounts the host workspace at the same absolute path,
// accepts apply against "/", diff paths are absolute, and tar names drop the leading "/".
type layout struct {
	workspace string
	outside   string
	mounts    *hostMounts
}

func newLayout(t *testing.T) layout {
	t.Helper()
	base, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	workspace := filepath.Join(base, "workspace")
	outside := filepath.Join(base, "outside")
	for _, dir := range []string{workspace, outside} {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	if err := os.WriteFile(filepath.Join(outside, "secret"), []byte("keep"), 0o600); err != nil {
		t.Fatal(err)
	}
	mounts, err := openHostMounts(string(filepath.Separator), []Mount{{HostPath: workspace}})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(mounts.Close)
	return layout{workspace: workspace, outside: outside, mounts: mounts}
}

// vm returns the absolute VM path of name inside the workspace.
func (l layout) vm(name string) string { return filepath.Join(l.workspace, name) }

// tarName returns the tar entry name the VM uses for name inside the workspace.
func (l layout) tarName(name string) string {
	return strings.TrimPrefix(l.vm(name), string(filepath.Separator))
}

func (l layout) requireOutsideUntouched(t *testing.T) {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(l.outside, "secret"))
	if err != nil || string(data) != "keep" {
		t.Fatalf("file outside the mount changed: %q, %v", data, err)
	}
	entries, err := os.ReadDir(l.outside)
	if err != nil || len(entries) != 1 {
		t.Fatalf("directory outside the mount changed: %v, %v", entries, err)
	}
}

func TestExtractTarInMountsWritesOrdinaryEntries(t *testing.T) {
	l := newLayout(t)
	tarball := buildTar(t, []tarEntry{
		{name: l.tarName("."), typeflag: tar.TypeDir, mode: 0o755},
		{name: l.tarName("src"), typeflag: tar.TypeDir, mode: 0o755},
		{name: l.tarName("src/main.go"), typeflag: tar.TypeReg, body: "package main"},
		{name: l.tarName("src/alias.go"), typeflag: tar.TypeSymlink, linkname: "main.go"},
		{name: l.tarName("src/absolute.go"), typeflag: tar.TypeSymlink, linkname: l.vm("src/main.go")},
		{name: l.tarName("src/copy.go"), typeflag: tar.TypeLink, linkname: l.tarName("src/main.go")},
	})
	if err := extractTarInMounts(context.Background(), l.mounts, tarball); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"src/main.go", "src/alias.go", "src/absolute.go", "src/copy.go"} {
		data, err := os.ReadFile(l.vm(name))
		if err != nil || string(data) != "package main" {
			t.Fatalf("%s = %q, %v", name, data, err)
		}
	}
	// An absolute target inside the mount is stored relative, so it survives a different host path.
	if target, err := os.Readlink(l.vm("src/absolute.go")); err != nil || target != "main.go" {
		t.Fatalf("absolute.go -> %q, %v", target, err)
	}
}

func TestExtractTarInMountsRejectsEscapes(t *testing.T) {
	tests := []struct {
		name    string
		prepare func(t *testing.T, l layout)
		entries func(l layout) []tarEntry
		absent  string
	}{
		{
			name: "relative symlink climbing out of the mount",
			entries: func(l layout) []tarEntry {
				return []tarEntry{{name: l.tarName("ssh"), typeflag: tar.TypeSymlink, linkname: "../outside"}}
			},
			absent: "ssh",
		},
		{
			name: "absolute symlink outside the mount",
			entries: func(l layout) []tarEntry {
				return []tarEntry{{name: l.tarName("ssh"), typeflag: tar.TypeSymlink, linkname: l.outside}}
			},
			absent: "ssh",
		},
		{
			name: "hard link to a file outside the mount",
			entries: func(l layout) []tarEntry {
				name := strings.TrimPrefix(filepath.Join(l.outside, "secret"), "/")
				return []tarEntry{{name: l.tarName("grab"), typeflag: tar.TypeLink, linkname: name}}
			},
			absent: "grab",
		},
		{
			name: "entry outside every mount",
			entries: func(l layout) []tarEntry {
				name := strings.TrimPrefix(filepath.Join(l.outside, "planted"), "/")
				return []tarEntry{{name: name, typeflag: tar.TypeReg, body: "x"}}
			},
		},
		{
			name: "entry climbing out with ..",
			entries: func(l layout) []tarEntry {
				return []tarEntry{{name: l.tarName("../outside/planted"), typeflag: tar.TypeReg, body: "x"}}
			},
		},
		{
			name: "write through an existing directory symlink",
			prepare: func(t *testing.T, l layout) {
				if err := os.Symlink(l.outside, l.vm("linked")); err != nil {
					t.Fatal(err)
				}
			},
			entries: func(l layout) []tarEntry {
				return []tarEntry{{name: l.tarName("linked/planted"), typeflag: tar.TypeReg, body: "x"}}
			},
		},
		{
			name: "overwrite through an existing file symlink",
			prepare: func(t *testing.T, l layout) {
				if err := os.Symlink(filepath.Join(l.outside, "secret"), l.vm("note")); err != nil {
					t.Fatal(err)
				}
			},
			entries: func(l layout) []tarEntry {
				return []tarEntry{{name: l.tarName("note"), typeflag: tar.TypeReg, body: "replaced"}}
			},
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			l := newLayout(t)
			if test.prepare != nil {
				test.prepare(t, l)
			}
			if err := extractTarInMounts(context.Background(), l.mounts, buildTar(t, test.entries(l))); err != nil {
				t.Fatal(err)
			}
			l.requireOutsideUntouched(t)
			if test.absent != "" {
				if _, err := os.Lstat(l.vm(test.absent)); !os.IsNotExist(err) {
					t.Fatalf("%s was created: %v", test.absent, err)
				}
			}
		})
	}
}

func TestExtractTarInMountsReplacesSymlinkAndDropsSpecialModes(t *testing.T) {
	l := newLayout(t)
	if err := os.Symlink(filepath.Join(l.outside, "secret"), l.vm("note")); err != nil {
		t.Fatal(err)
	}
	tarball := buildTar(t, []tarEntry{
		{name: l.tarName("note"), typeflag: tar.TypeReg, body: "replaced"},
		{name: l.tarName("tool"), typeflag: tar.TypeReg, mode: 0o4755, body: "#!/bin/sh"},
	})
	if err := extractTarInMounts(context.Background(), l.mounts, tarball); err != nil {
		t.Fatal(err)
	}
	if info, err := os.Lstat(l.vm("note")); err != nil || !info.Mode().IsRegular() {
		t.Fatalf("note should be a regular file: %v, %v", info, err)
	}
	info, err := os.Stat(l.vm("tool"))
	if err != nil || info.Mode()&os.ModeSetuid != 0 || info.Mode().Perm() != 0o755 {
		t.Fatalf("unexpected tool mode %v, %v", info, err)
	}
	l.requireOutsideUntouched(t)
}

func TestApplyMovesAndDeletionsUseAbsoluteVMPaths(t *testing.T) {
	l := newLayout(t)
	for _, name := range []string{"a.txt", "old.txt"} {
		if err := os.WriteFile(l.vm(name), []byte("x"), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	changes := []types.FileDiff{
		{Path: l.vm("dir/b.txt"), MovedFrom: l.vm("a.txt")},
		{Path: l.vm("old.txt"), IsDeleted: true},
	}
	applyMovesInMounts(l.mounts, changes)
	applyDeletionsInMounts(l.mounts, changes)

	if _, err := os.Stat(l.vm("dir/b.txt")); err != nil {
		t.Fatalf("move was not applied: %v", err)
	}
	for _, name := range []string{"a.txt", "old.txt"} {
		if _, err := os.Stat(l.vm(name)); !os.IsNotExist(err) {
			t.Fatalf("%s should be gone: %v", name, err)
		}
	}
}

func TestApplyMovesAndDeletionsStayInsideMounts(t *testing.T) {
	l := newLayout(t)
	if err := os.WriteFile(l.vm("a.txt"), []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
	applyMovesInMounts(l.mounts, []types.FileDiff{
		{Path: l.vm("stolen"), MovedFrom: filepath.Join(l.outside, "secret")},
		{Path: filepath.Join(l.outside, "planted"), MovedFrom: l.vm("a.txt")},
		{Path: l.vm("../outside/planted"), MovedFrom: l.vm("a.txt")},
	})
	applyDeletionsInMounts(l.mounts, []types.FileDiff{
		{Path: l.outside, IsDeleted: true},
		{Path: l.vm("../outside"), IsDeleted: true},
		{Path: l.workspace, IsDeleted: true},
		{Path: "relative", IsDeleted: true},
		{Path: "/", IsDeleted: true},
	})
	if _, err := os.Stat(l.vm("a.txt")); err != nil {
		t.Fatalf("a.txt should not have moved out: %v", err)
	}
	if _, err := os.Lstat(l.vm("stolen")); !os.IsNotExist(err) {
		t.Fatalf("a file outside the mount was moved in: %v", err)
	}
	l.requireOutsideUntouched(t)
}

func TestHostMountsMapVMPathsToHostPaths(t *testing.T) {
	base, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	host := filepath.Join(base, "host-project")
	if err := os.MkdirAll(host, 0o755); err != nil {
		t.Fatal(err)
	}
	mounts, err := openHostMounts(string(filepath.Separator), []Mount{{HostPath: host, VMPath: "/workspace"}})
	if err != nil {
		t.Fatal(err)
	}
	defer mounts.Close()
	tarball := buildTar(t, []tarEntry{{name: "workspace/readme.md", typeflag: tar.TypeReg, body: "hi"}})
	if err := extractTarInMounts(context.Background(), mounts, tarball); err != nil {
		t.Fatal(err)
	}
	if data, err := os.ReadFile(filepath.Join(host, "readme.md")); err != nil || string(data) != "hi" {
		t.Fatalf("readme.md = %q, %v", data, err)
	}
}
