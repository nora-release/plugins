//go:build linux || darwin

package overlay

import (
	"os"
	"path/filepath"
	"testing"

	"golang.org/x/sys/unix"
)

// createWhiteout creates a whiteout entry for tests.
// It first tries the Linux char-device format, then falls back to xattr format.
func createWhiteout(path string) error {
	if err := unix.Mknod(path, unix.S_IFCHR|0666, 0); err == nil {
		return nil
	}
	return createXattrWhiteout(path)
}

func createXattrWhiteout(path string) error {
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	_ = f.Close()
	return unix.Setxattr(path, whiteoutXattr, []byte("y"), 0)
}

func setOpaqueDir(path string) error {
	return unix.Setxattr(path, opaqueXattr, []byte("y"), 0)
}

func TestAnalyzer_NewAndUpdatedFile(t *testing.T) {
	lower, upper, cleanup := setupTestDirs(t)
	defer cleanup()

	writeFile(t, filepath.Join(upper, "new.txt"), "new")
	writeFile(t, filepath.Join(lower, "updated.txt"), "before")
	writeFile(t, filepath.Join(upper, "updated.txt"), "after")

	result := analyzeDiff(t, upper, lower)
	if len(result.FileDiff) != 2 {
		t.Fatalf("expected 2 changes, got %d (%+v)", len(result.FileDiff), result.FileDiff)
	}

	var foundNew, foundUpdated bool
	for _, diff := range result.FileDiff {
		switch diff.Path {
		case "new.txt":
			foundNew = true
			if diff.IsDeleted || diff.IsUpdated || diff.MovedFrom != "" {
				t.Fatalf("unexpected new file diff: %+v", diff)
			}
		case "updated.txt":
			foundUpdated = true
			if !diff.IsUpdated || diff.IsDeleted {
				t.Fatalf("unexpected updated file diff: %+v", diff)
			}
		}
	}
	if !foundNew || !foundUpdated {
		t.Fatalf("expected new/updated diffs, got %+v", result.FileDiff)
	}
}

func TestAnalyzer_DeletedDirectoryByWhiteout(t *testing.T) {
	lower, upper, cleanup := setupTestDirs(t)
	defer cleanup()

	writeFile(t, filepath.Join(lower, "mydir", "a.txt"), "a")
	writeFile(t, filepath.Join(lower, "mydir", "b.txt"), "b")
	if err := createWhiteout(filepath.Join(upper, "mydir")); err != nil {
		t.Fatalf("createWhiteout: %v", err)
	}

	result := analyzeDiff(t, upper, lower)
	if len(result.FileDiff) != 3 {
		t.Fatalf("expected 3 deletions, got %d (%+v)", len(result.FileDiff), result.FileDiff)
	}

	deleted := map[string]bool{}
	for _, diff := range result.FileDiff {
		if !diff.IsDeleted {
			t.Fatalf("expected deleted diff, got %+v", diff)
		}
		deleted[diff.Path] = true
	}
	for _, path := range []string{"mydir", "mydir/a.txt", "mydir/b.txt"} {
		if !deleted[path] {
			t.Fatalf("expected deleted path %q in %+v", path, result.FileDiff)
		}
	}
}

func TestAnalyzer_OpaqueDirectory(t *testing.T) {
	lower, upper, cleanup := setupTestDirs(t)
	defer cleanup()

	writeFile(t, filepath.Join(lower, "opaquedir", "old.txt"), "old content")
	upperDir := filepath.Join(upper, "opaquedir")
	if err := os.MkdirAll(upperDir, 0755); err != nil {
		t.Fatalf("mkdir upper opaque dir: %v", err)
	}
	if err := setOpaqueDir(upperDir); err != nil {
		t.Skip("opaque xattr not supported")
	}
	val := make([]byte, 1)
	size, err := unix.Getxattr(upperDir, opaqueXattr, val)
	if err != nil || size == 0 || val[0] != 'y' {
		t.Skip("opaque xattr not readable")
	}
	writeFile(t, filepath.Join(upperDir, "new.txt"), "new content")

	analyzer, err := NewOverlayDiffAnalyzer(upper, lower)
	if err != nil {
		t.Fatalf("NewOverlayDiffAnalyzer: %v", err)
	}
	defer analyzer.Close()

	if !analyzer.isOpaqueDir(upperDir) {
		t.Skip("isOpaqueDir returned false on platform")
	}

	result, err := analyzer.Analyze()
	if err != nil {
		t.Fatalf("Analyze: %v", err)
	}

	var newFound, oldDeleted bool
	for _, diff := range result.FileDiff {
		switch diff.Path {
		case "opaquedir/new.txt":
			newFound = true
		case "opaquedir/old.txt":
			oldDeleted = true
			if !diff.IsDeleted {
				t.Fatalf("expected old.txt deleted, got %+v", diff)
			}
		}
	}
	if !newFound || !oldDeleted {
		t.Fatalf("expected new+deleted diff in opaque dir, got %+v", result.FileDiff)
	}
}

func TestAnalyzer_TypeChanges(t *testing.T) {
	t.Run("file to directory", func(t *testing.T) {
		lower, upper, cleanup := setupTestDirs(t)
		defer cleanup()

		writeFile(t, filepath.Join(lower, "item"), "file")
		writeFile(t, filepath.Join(upper, "item", "child.txt"), "child")

		result := analyzeDiff(t, upper, lower)
		var itemUpdated, childAdded bool
		for _, diff := range result.FileDiff {
			switch diff.Path {
			case "item":
				itemUpdated = true
				if !diff.IsDir || !diff.IsUpdated {
					t.Fatalf("unexpected item diff: %+v", diff)
				}
			case "item/child.txt":
				childAdded = true
			}
		}
		if !itemUpdated || !childAdded {
			t.Fatalf("expected type-change diffs, got %+v", result.FileDiff)
		}
	})

	t.Run("directory to file", func(t *testing.T) {
		lower, upper, cleanup := setupTestDirs(t)
		defer cleanup()

		writeFile(t, filepath.Join(lower, "item", "child.txt"), "child")
		writeFile(t, filepath.Join(upper, "item"), "now file")

		result := analyzeDiff(t, upper, lower)
		var itemUpdated, childDeleted bool
		for _, diff := range result.FileDiff {
			switch diff.Path {
			case "item":
				itemUpdated = true
				if diff.IsDir || !diff.IsUpdated {
					t.Fatalf("unexpected item diff: %+v", diff)
				}
			case "item/child.txt":
				childDeleted = true
				if !diff.IsDeleted {
					t.Fatalf("expected deleted child diff, got %+v", diff)
				}
			}
		}
		if !itemUpdated || !childDeleted {
			t.Fatalf("expected type-change diffs, got %+v", result.FileDiff)
		}
	})
}

func analyzeDiff(t *testing.T, upper, lower string) *OverlayDiffAnalyzerResult {
	t.Helper()

	analyzer, err := NewOverlayDiffAnalyzer(upper, lower)
	if err != nil {
		t.Fatalf("NewOverlayDiffAnalyzer: %v", err)
	}
	defer analyzer.Close()

	result, err := analyzer.Analyze()
	if err != nil {
		t.Fatalf("Analyze: %v", err)
	}
	return result
}

func setupTestDirs(t *testing.T) (lower, upper string, cleanup func()) {
	t.Helper()

	tmpDir, err := os.MkdirTemp("", "overlay-diff-test-*")
	if err != nil {
		t.Fatalf("creating temp dir: %v", err)
	}

	lower = filepath.Join(tmpDir, "lower")
	upper = filepath.Join(tmpDir, "upper")

	if err := os.MkdirAll(lower, 0755); err != nil {
		_ = os.RemoveAll(tmpDir)
		t.Fatalf("creating lower dir: %v", err)
	}
	if err := os.MkdirAll(upper, 0755); err != nil {
		_ = os.RemoveAll(tmpDir)
		t.Fatalf("creating upper dir: %v", err)
	}

	return lower, upper, func() {
		_ = os.RemoveAll(tmpDir)
	}
}

func writeFile(t *testing.T, path, content string) {
	t.Helper()

	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0755); err != nil {
		t.Fatalf("creating directory %s: %v", dir, err)
	}
	if err := os.WriteFile(path, []byte(content), 0644); err != nil {
		t.Fatalf("writing file %s: %v", path, err)
	}
}
