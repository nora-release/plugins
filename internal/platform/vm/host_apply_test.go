package vm

import (
	"archive/tar"
	"bytes"
	"context"
	"os"
	"path/filepath"
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
			Size:     int64(len(entry.body)),
		}
		if entry.typeflag != tar.TypeReg {
			header.Size = 0
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

// hostLayout creates a workspace next to a directory standing in for the rest of the
// user's files, which accepted VM changes must never touch.
func hostLayout(t *testing.T) (workspace, outside string, root *os.Root) {
	t.Helper()
	base := t.TempDir()
	workspace = filepath.Join(base, "workspace")
	outside = filepath.Join(base, "outside")
	if err := os.MkdirAll(outside, 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(outside, "secret"), []byte("keep"), 0o600); err != nil {
		t.Fatal(err)
	}
	root, err := openHostRoot(workspace)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { root.Close() })
	return workspace, outside, root
}

func requireOutsideUntouched(t *testing.T, outside string) {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(outside, "secret"))
	if err != nil || string(data) != "keep" {
		t.Fatalf("file outside the workspace changed: %q, %v", data, err)
	}
	entries, err := os.ReadDir(outside)
	if err != nil || len(entries) != 1 {
		t.Fatalf("directory outside the workspace changed: %v, %v", entries, err)
	}
}

func TestExtractTarInRootWritesOrdinaryEntries(t *testing.T) {
	workspace, _, root := hostLayout(t)
	tarball := buildTar(t, []tarEntry{
		{name: "src", typeflag: tar.TypeDir, mode: 0o755},
		{name: "src/main.go", typeflag: tar.TypeReg, body: "package main"},
		{name: "src/alias.go", typeflag: tar.TypeSymlink, linkname: "main.go"},
		{name: "src/copy.go", typeflag: tar.TypeLink, linkname: "src/main.go"},
	})
	if err := extractTarInRoot(context.Background(), root, tarball); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"src/main.go", "src/alias.go", "src/copy.go"} {
		data, err := os.ReadFile(filepath.Join(workspace, name))
		if err != nil || string(data) != "package main" {
			t.Fatalf("%s = %q, %v", name, data, err)
		}
	}
}

func TestExtractTarInRootRejectsEscapes(t *testing.T) {
	tests := []struct {
		name    string
		prepare func(t *testing.T, workspace, outside string)
		entries []tarEntry
		absent  string
	}{
		{
			name:    "absolute symlink target",
			entries: []tarEntry{{name: "ssh", typeflag: tar.TypeSymlink, linkname: "/etc"}},
			absent:  "ssh",
		},
		{
			name:    "symlink climbing out",
			entries: []tarEntry{{name: "a/up", typeflag: tar.TypeSymlink, linkname: "../../outside"}},
			absent:  "a/up",
		},
		{
			name:    "hard link to a file outside",
			entries: []tarEntry{{name: "grab", typeflag: tar.TypeLink, linkname: "../outside/secret"}},
			absent:  "grab",
		},
		{
			name:    "entry climbing out",
			entries: []tarEntry{{name: "../outside/planted", typeflag: tar.TypeReg, body: "x"}},
		},
		{
			name: "write through an existing directory symlink",
			prepare: func(t *testing.T, workspace, outside string) {
				if err := os.Symlink(outside, filepath.Join(workspace, "linked")); err != nil {
					t.Fatal(err)
				}
			},
			entries: []tarEntry{{name: "linked/planted", typeflag: tar.TypeReg, body: "x"}},
		},
		{
			name: "overwrite an existing file symlink",
			prepare: func(t *testing.T, workspace, outside string) {
				if err := os.Symlink(filepath.Join(outside, "secret"), filepath.Join(workspace, "note")); err != nil {
					t.Fatal(err)
				}
			},
			entries: []tarEntry{{name: "note", typeflag: tar.TypeReg, body: "replaced"}},
		},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			workspace, outside, root := hostLayout(t)
			if test.prepare != nil {
				test.prepare(t, workspace, outside)
			}
			if err := extractTarInRoot(context.Background(), root, buildTar(t, test.entries)); err != nil {
				t.Fatal(err)
			}
			requireOutsideUntouched(t, outside)
			if test.absent != "" {
				if _, err := os.Lstat(filepath.Join(workspace, test.absent)); !os.IsNotExist(err) {
					t.Fatalf("%s was created: %v", test.absent, err)
				}
			}
		})
	}
}

func TestExtractTarInRootReplacesSymlinkWithRegularFile(t *testing.T) {
	workspace, outside, root := hostLayout(t)
	if err := os.Symlink(filepath.Join(outside, "secret"), filepath.Join(workspace, "note")); err != nil {
		t.Fatal(err)
	}
	tarball := buildTar(t, []tarEntry{{name: "note", typeflag: tar.TypeReg, body: "replaced"}})
	if err := extractTarInRoot(context.Background(), root, tarball); err != nil {
		t.Fatal(err)
	}
	info, err := os.Lstat(filepath.Join(workspace, "note"))
	if err != nil || !info.Mode().IsRegular() {
		t.Fatalf("note should be a regular file: %v, %v", info, err)
	}
	requireOutsideUntouched(t, outside)
}

func TestExtractTarInRootDropsSpecialModeBits(t *testing.T) {
	workspace, _, root := hostLayout(t)
	tarball := buildTar(t, []tarEntry{{name: "tool", typeflag: tar.TypeReg, mode: 0o4755, body: "#!/bin/sh"}})
	if err := extractTarInRoot(context.Background(), root, tarball); err != nil {
		t.Fatal(err)
	}
	info, err := os.Stat(filepath.Join(workspace, "tool"))
	if err != nil {
		t.Fatal(err)
	}
	if info.Mode()&os.ModeSetuid != 0 || info.Mode().Perm() != 0o755 {
		t.Fatalf("unexpected mode %v", info.Mode())
	}
}

func TestApplyDeletionsInRootStaysInsideWorkspace(t *testing.T) {
	workspace, outside, root := hostLayout(t)
	if err := os.WriteFile(filepath.Join(workspace, "old.txt"), []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
	applyDeletionsInRoot(root, []types.FileDiff{
		{Path: "old.txt", IsDeleted: true},
		{Path: "../outside", IsDeleted: true},
		{Path: ".", IsDeleted: true},
		{Path: "", IsDeleted: true},
		{Path: "/", IsDeleted: true},
	})
	if _, err := os.Stat(filepath.Join(workspace, "old.txt")); !os.IsNotExist(err) {
		t.Fatalf("old.txt should be deleted: %v", err)
	}
	if _, err := os.Stat(workspace); err != nil {
		t.Fatalf("workspace itself was removed: %v", err)
	}
	requireOutsideUntouched(t, outside)
}

func TestApplyMovesInRootStaysInsideWorkspace(t *testing.T) {
	workspace, outside, root := hostLayout(t)
	if err := os.WriteFile(filepath.Join(workspace, "a.txt"), []byte("x"), 0o644); err != nil {
		t.Fatal(err)
	}
	applyMovesInRoot(root, []types.FileDiff{
		{Path: "dir/b.txt", MovedFrom: "a.txt"},
		{Path: "stolen", MovedFrom: "../outside/secret"},
		{Path: "../outside/planted", MovedFrom: "dir/b.txt"},
	})
	if _, err := os.Stat(filepath.Join(workspace, "dir/b.txt")); err != nil {
		t.Fatalf("in-workspace move failed: %v", err)
	}
	if _, err := os.Lstat(filepath.Join(workspace, "stolen")); !os.IsNotExist(err) {
		t.Fatalf("a file outside the workspace was moved in: %v", err)
	}
	requireOutsideUntouched(t, outside)
}
