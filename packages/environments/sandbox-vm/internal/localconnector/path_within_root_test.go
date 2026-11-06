package localconnector

import (
	"os"
	"path/filepath"
	"testing"
)

func TestResolvePathWithinRootFollowsSymlinks(t *testing.T) {
	// t.TempDir lives under /var on macOS, itself a symlink to /private/var, so the root's own
	// symlinks are resolved too.
	base := t.TempDir()
	root := filepath.Join(base, "project")
	outside := filepath.Join(base, "outside")
	hostMount := filepath.Join(base, "mounted")
	for _, dir := range []string{filepath.Join(root, "src"), outside, hostMount} {
		if err := os.MkdirAll(dir, 0o755); err != nil {
			t.Fatal(err)
		}
	}
	links := map[string]string{
		filepath.Join(root, "escape"):        outside,
		filepath.Join(root, "inner"):         filepath.Join(root, "src"),
		filepath.Join(hostMount, "escape"):   outside,
		filepath.Join(root, "src", "parent"): "..",
	}
	for link, target := range links {
		if err := os.Symlink(target, link); err != nil {
			t.Fatal(err)
		}
	}
	runtime := &Runtime{
		rootPath: root,
		mounts:   []Mount{{HostPath: hostMount, VMPath: "/workspace"}},
	}

	tests := []struct {
		path    string
		allowed bool
	}{
		{path: "src/main.go", allowed: true},
		{path: "src/new/file.go", allowed: true},
		{path: "inner/main.go", allowed: true},
		{path: "src/parent/src", allowed: true},
		{path: "../outside/secret", allowed: false},
		{path: "escape/secret", allowed: false},
		{path: "escape", allowed: false},
		{path: outside, allowed: false},
		{path: "/workspace/notes.md", allowed: true},
		{path: "/workspace/escape/secret", allowed: false},
	}
	for _, test := range tests {
		t.Run(test.path, func(t *testing.T) {
			_, err := runtime.resolvePathWithinRoot(test.path)
			if test.allowed && err != nil {
				t.Fatalf("expected %q to be allowed: %v", test.path, err)
			}
			if !test.allowed && err == nil {
				t.Fatalf("expected %q to be rejected", test.path)
			}
		})
	}
}
