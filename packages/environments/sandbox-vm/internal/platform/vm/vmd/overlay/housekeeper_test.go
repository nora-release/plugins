//go:build linux || darwin

package overlay

import (
	"os"
	"path/filepath"
	"sort"
	"testing"

	"golang.org/x/sys/unix"
)

type expectedDiff struct {
	Path      string
	IsDir     bool
	IsUpdated bool
	IsDeleted bool
	MovedFrom string
}

func runHousekeeperAndAssert(t *testing.T, upper, lower string, expected []expectedDiff) {
	t.Helper()

	hk := NewHousekeeper(upper, lower)
	if err := hk.Run(); err != nil {
		t.Fatalf("Housekeeper.Run: %v", err)
	}

	result := analyzeHousekeeperDiff(t, upper, lower)
	assertExpectedDiffs(t, result.FileDiff, expected)
}

func analyzeHousekeeperDiff(t *testing.T, upper, lower string) *OverlayDiffAnalyzerResult {
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

func assertExpectedDiffs(t *testing.T, actual []FileDiff, expected []expectedDiff) {
	t.Helper()

	sort.Slice(actual, func(i, j int) bool { return actual[i].Path < actual[j].Path })
	sort.Slice(expected, func(i, j int) bool { return expected[i].Path < expected[j].Path })

	if len(actual) != len(expected) {
		t.Fatalf("expected %d diffs, got %d (actual=%+v)", len(expected), len(actual), actual)
	}

	for i := range expected {
		got := actual[i]
		want := expected[i]
		if got.Path != want.Path {
			t.Fatalf("diff[%d] path: got %q want %q", i, got.Path, want.Path)
		}
		if got.IsDir != want.IsDir {
			t.Fatalf("diff[%d] %s IsDir: got %v want %v", i, got.Path, got.IsDir, want.IsDir)
		}
		if got.IsUpdated != want.IsUpdated {
			t.Fatalf("diff[%d] %s IsUpdated: got %v want %v", i, got.Path, got.IsUpdated, want.IsUpdated)
		}
		if got.IsDeleted != want.IsDeleted {
			t.Fatalf("diff[%d] %s IsDeleted: got %v want %v", i, got.Path, got.IsDeleted, want.IsDeleted)
		}
		if got.MovedFrom != want.MovedFrom {
			t.Fatalf("diff[%d] %s MovedFrom: got %q want %q", i, got.Path, got.MovedFrom, want.MovedFrom)
		}
	}
}

func TestHousekeeper_CoreScenarios(t *testing.T) {
	t.Run("removes redundant upper files", func(t *testing.T) {
		lower, upper, cleanup := setupTestDirs(t)
		defer cleanup()

		writeFile(t, filepath.Join(lower, "a.txt"), "same")
		writeFile(t, filepath.Join(lower, "b.txt"), "old")
		writeFile(t, filepath.Join(upper, "a.txt"), "same")
		writeFile(t, filepath.Join(upper, "b.txt"), "new")

		runHousekeeperAndAssert(t, upper, lower, []expectedDiff{
			{Path: "b.txt", IsUpdated: true},
		})
	})

	t.Run("removes orphaned whiteouts", func(t *testing.T) {
		lower, upper, cleanup := setupTestDirs(t)
		defer cleanup()

		writeFile(t, filepath.Join(lower, "exists.txt"), "value")
		if err := createWhiteout(filepath.Join(upper, "exists.txt")); err != nil {
			t.Fatalf("createWhiteout exists: %v", err)
		}
		if err := createWhiteout(filepath.Join(upper, "orphan.txt")); err != nil {
			t.Fatalf("createWhiteout orphan: %v", err)
		}

		runHousekeeperAndAssert(t, upper, lower, []expectedDiff{
			{Path: "exists.txt", IsDeleted: true},
		})
	})

	t.Run("keeps legal redirect-based directory move", func(t *testing.T) {
		lower, upper, cleanup := setupTestDirs(t)
		defer cleanup()

		os.MkdirAll(filepath.Join(lower, "oldname"), 0755)
		writeFile(t, filepath.Join(lower, "oldname", "file.txt"), "hello")

		os.MkdirAll(filepath.Join(upper, "newname"), 0755)
		if err := unix.Setxattr(filepath.Join(upper, "newname"), redirectXattr, []byte("oldname"), 0); err != nil {
			t.Skip("xattr not supported")
		}
		writeFile(t, filepath.Join(upper, "newname", "file.txt"), "hello modified")

		runHousekeeperAndAssert(t, upper, lower, []expectedDiff{
			{Path: "newname", IsDir: true},
			{Path: "newname/file.txt"},
		})
	})
}

func TestHousekeeper_RejectsIllegalRedirects(t *testing.T) {
	lower, upper, cleanup := setupTestDirs(t)
	defer cleanup()

	writeFile(t, filepath.Join(lower, "safe.txt"), "safe")
	writeFile(t, filepath.Join(upper, "escape.txt"), "bad")
	if err := unix.Setxattr(filepath.Join(upper, "escape.txt"), redirectXattr, []byte("../../../etc/passwd"), 0); err != nil {
		t.Skip("xattr not supported")
	}
	writeFile(t, filepath.Join(upper, "safe.txt"), "safe modified")

	runHousekeeperAndAssert(t, upper, lower, []expectedDiff{
		{Path: "safe.txt", IsUpdated: true},
	})
}
