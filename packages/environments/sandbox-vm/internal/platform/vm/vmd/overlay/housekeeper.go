//go:build linux || darwin

package overlay

import (
	"bytes"
	"fmt"
	"io"
	"log"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"syscall"
	"time"

	"golang.org/x/sys/unix"
)

// Housekeeper cleans up overlay filesystem state between task executions.
// It should be run when the overlay is unmounted.
type Housekeeper struct {
	upperDir string
	lowerDir string
}

// NewHousekeeper creates a new Housekeeper for the given overlay directories.
func NewHousekeeper(upperDir, lowerDir string) *Housekeeper {
	return &Housekeeper{
		upperDir: upperDir,
		lowerDir: lowerDir,
	}
}

// NewHousekeeperWithAnalyzer creates a new Housekeeper.
// The analyzer parameter is accepted for API compatibility but is no longer used.
func NewHousekeeperWithAnalyzer(upperDir, lowerDir string, _ *OverlayDiffAnalyzer) *Housekeeper {
	return &Housekeeper{
		upperDir: upperDir,
		lowerDir: lowerDir,
	}
}

// Run performs housekeeping on the overlay filesystem.
// The overlay must be unmounted before calling this method.
//
// Processing order:
// 0. Remove illegal redirects - redirects/symlinks that escape overlay boundaries
// 1. Flatten opaque directories - converts to regular dirs + whiteouts
// 2. Flatten redirect directories - converts dir redirects to file redirects
// 3. Remove stale entries - identical files, orphaned whiteouts, dead redirects
// 4. Clean up empty directories
func (h *Housekeeper) Run() error {
	overallStart := time.Now()
	defer func() {
		log.Printf("Housekeeper.Run took %v", time.Since(overallStart))
	}()

	// Drop page cache to ensure we read fresh data from disk
	_ = os.WriteFile("/proc/sys/vm/drop_caches", []byte("2"), 0644)

	// Step 0: Remove illegal redirects (redirects/symlinks that escape overlay)
	illegalStart := time.Now()
	if _, err := h.removeIllegalRedirects(); err != nil {
		return fmt.Errorf("removing illegal redirects: %w", err)
	}
	log.Printf("Housekeeper.Run: remove illegal redirects took %v", time.Since(illegalStart))

	// Step 1: Flatten opaque directories
	flattenStart := time.Now()
	opaqueDirs, err := h.findOpaqueDirs()
	if err != nil {
		return fmt.Errorf("finding opaque directories: %w", err)
	}

	for _, opaqueDir := range opaqueDirs {
		if _, err := h.flattenOpaqueDir(opaqueDir); err != nil {
			return fmt.Errorf("flattening opaque dir %s: %w", opaqueDir, err)
		}
	}
	log.Printf("Housekeeper.Run: flatten opaque dirs took %v", time.Since(flattenStart))

	// Step 2: Flatten redirect directories
	redirectStart := time.Now()
	if _, err := h.flattenRedirectDirs(); err != nil {
		return fmt.Errorf("flattening redirect dirs: %w", err)
	}
	log.Printf("Housekeeper.Run: flatten redirect dirs took %v", time.Since(redirectStart))

	// Step 3: Remove stale entries directly
	staleStart := time.Now()
	if _, err := h.removeStaleEntries(); err != nil {
		return fmt.Errorf("removing stale entries: %w", err)
	}
	log.Printf("Housekeeper.Run: remove stale entries took %v", time.Since(staleStart))

	// Step 4: Clean up empty directories
	cleanupStart := time.Now()
	if err := h.cleanEmptyDirs(); err != nil {
		return fmt.Errorf("cleaning empty directories: %w", err)
	}
	log.Printf("Housekeeper.Run: clean empty dirs took %v", time.Since(cleanupStart))

	return nil
}

// findOpaqueDirs finds all opaque directories in the upper layer.
func (h *Housekeeper) findOpaqueDirs() ([]string, error) {
	var opaqueDirs []string

	err := filepath.Walk(h.upperDir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if !info.IsDir() {
			return nil
		}
		if h.isOpaqueDir(path) {
			relPath, err := filepath.Rel(h.upperDir, path)
			if err != nil {
				return err
			}
			opaqueDirs = append(opaqueDirs, relPath)
		}
		return nil
	})

	return opaqueDirs, err
}

// isOpaqueDir checks if a directory has the opaque xattr.
func (h *Housekeeper) isOpaqueDir(path string) bool {
	val := make([]byte, 1)
	size, err := unix.Getxattr(path, opaqueXattr, val)
	if err != nil || size == 0 {
		return false
	}
	return val[0] == 'y'
}

// flattenOpaqueDir converts an opaque directory to a regular directory
// by creating whiteout files for all lower layer entries not present in upper.
func (h *Housekeeper) flattenOpaqueDir(relPath string) (int, error) {
	upperBasePath := filepath.Join(h.upperDir, relPath)
	lowerBasePath := filepath.Join(h.lowerDir, relPath)

	lowerInfo, err := os.Lstat(lowerBasePath)
	if os.IsNotExist(err) {
		return 0, h.removeOpaqueAttr(upperBasePath)
	}
	if err != nil {
		return 0, fmt.Errorf("stat lower dir: %w", err)
	}
	if !lowerInfo.IsDir() {
		return 0, h.removeOpaqueAttr(upperBasePath)
	}

	// Build map of upper entries
	upperEntries := make(map[string]struct{})
	err = filepath.Walk(upperBasePath, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if path == upperBasePath {
			return nil
		}
		entryRelPath, err := filepath.Rel(upperBasePath, path)
		if err != nil {
			return err
		}
		upperEntries[entryRelPath] = struct{}{}
		return nil
	})
	if err != nil {
		return 0, fmt.Errorf("walking upper dir: %w", err)
	}

	// Create whiteouts for lower entries not in upper
	whiteoutsCreated := 0
	err = filepath.Walk(lowerBasePath, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if path == lowerBasePath {
			return nil
		}
		entryRelPath, err := filepath.Rel(lowerBasePath, path)
		if err != nil {
			return err
		}

		if _, exists := upperEntries[entryRelPath]; !exists {
			whiteoutDir := filepath.Join(upperBasePath, filepath.Dir(entryRelPath))
			whiteoutPath := filepath.Join(whiteoutDir, filepath.Base(entryRelPath))

			if err := os.MkdirAll(whiteoutDir, 0755); err != nil {
				return fmt.Errorf("creating parent dir for whiteout: %w", err)
			}
			if err := h.createWhiteout(whiteoutPath); err != nil {
				return fmt.Errorf("creating whiteout for %s: %w", entryRelPath, err)
			}
			whiteoutsCreated++

			if info.IsDir() {
				return filepath.SkipDir
			}
		}
		return nil
	})
	if err != nil {
		return whiteoutsCreated, fmt.Errorf("walking lower dir: %w", err)
	}

	if err := h.removeOpaqueAttr(upperBasePath); err != nil {
		return whiteoutsCreated, fmt.Errorf("removing opaque attr: %w", err)
	}

	return whiteoutsCreated, nil
}

// flattenRedirectDirs finds and flattens all directory-level redirects in upper.
// Returns the number of directories flattened.
func (h *Housekeeper) flattenRedirectDirs() (int, error) {
	type redirectEntry struct {
		relPath  string
		redirect string
		depth    int
	}

	var entries []redirectEntry

	err := filepath.Walk(h.upperDir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if !info.IsDir() {
			return nil
		}
		redirect := h.getRedirect(path)
		if redirect == "" {
			return nil
		}
		relPath, err := filepath.Rel(h.upperDir, path)
		if err != nil {
			return err
		}
		entries = append(entries, redirectEntry{
			relPath:  relPath,
			redirect: redirect,
			depth:    strings.Count(relPath, string(filepath.Separator)),
		})
		return nil
	})
	if err != nil {
		return 0, err
	}

	// Sort deepest first to handle nested redirects
	sort.Slice(entries, func(i, j int) bool {
		return entries[i].depth > entries[j].depth
	})

	count := 0
	for _, e := range entries {
		if err := h.flattenRedirectDir(e.relPath, e.redirect); err != nil {
			return count, fmt.Errorf("flattening redirect dir %s: %w", e.relPath, err)
		}
		count++
	}
	return count, nil
}

// flattenRedirectDir converts a directory redirect to file-level redirects.
func (h *Housekeeper) flattenRedirectDir(relPath, redirect string) error {
	upperDirPath := filepath.Join(h.upperDir, relPath)

	// Resolve redirect to lower path.
	// For nested redirects (e.g. x/y where x also has a redirect), we need to
	// resolve through the parent's redirect chain to find the actual lower path.
	parentLowerDir := h.resolveParentLowerDir(filepath.Dir(relPath))
	lowerDirPath := h.resolveRedirect(redirect, parentLowerDir)

	// Check if lower target exists
	lowerInfo, err := os.Lstat(lowerDirPath)
	if os.IsNotExist(err) || (err == nil && !lowerInfo.IsDir()) {
		// Target doesn't exist or isn't a directory — delete the upper dir
		if err := os.RemoveAll(upperDirPath); err != nil {
			return fmt.Errorf("removing stale redirect dir: %w", err)
		}
		return nil
	}
	if err != nil {
		return fmt.Errorf("stat lower redirect target: %w", err)
	}

	// Resolve the lower relative path for building absolute redirect values
	parentLowerRelDir := h.resolveParentLowerRelDir(filepath.Dir(relPath))
	lowerRelDir := h.resolveRedirectRel(redirect, parentLowerRelDir)

	// Check if this is a no-op redirect (folder moved then moved back to original location)
	if lowerRelDir == relPath {
		// Just remove the redirect xattr — no effective change
		return h.removeRedirectAttr(upperDirPath)
	}

	// Recursively flatten: set file redirects and create placeholders
	if err := h.flattenRedirectDirRecursive(upperDirPath, lowerDirPath, lowerRelDir); err != nil {
		return err
	}

	// Remove directory redirect xattr
	return h.removeRedirectAttr(upperDirPath)
}

// flattenRedirectDirRecursive processes a single directory level during redirect flattening.
func (h *Housekeeper) flattenRedirectDirRecursive(upperDirPath, lowerDirPath, lowerRelDir string) error {
	// Build set of upper entries
	upperEntries := make(map[string]os.FileInfo)
	upperDirEntries, err := os.ReadDir(upperDirPath)
	if err != nil && !os.IsNotExist(err) {
		return fmt.Errorf("reading upper dir: %w", err)
	}
	for _, de := range upperDirEntries {
		absPath := filepath.Join(upperDirPath, de.Name())
		info, err := os.Lstat(absPath)
		if err != nil {
			continue
		}
		upperEntries[de.Name()] = info
	}

	// Walk lower dir
	lowerDirEntries, err := os.ReadDir(lowerDirPath)
	if err != nil && !os.IsNotExist(err) {
		return fmt.Errorf("reading lower dir: %w", err)
	}

	for _, de := range lowerDirEntries {
		name := de.Name()
		lowerAbsPath := filepath.Join(lowerDirPath, name)
		lowerInfo, err := os.Lstat(lowerAbsPath)
		if err != nil {
			continue
		}
		upperAbsPath := filepath.Join(upperDirPath, name)
		lowerFileRelPath := filepath.Join(lowerRelDir, name)

		upperInfo, inUpper := upperEntries[name]

		if lowerInfo.IsDir() {
			if inUpper && upperInfo.IsDir() {
				// Both have this directory — recurse
				if err := h.flattenRedirectDirRecursive(upperAbsPath, lowerAbsPath, lowerFileRelPath); err != nil {
					return err
				}
			} else if !inUpper {
				// Directory only in lower — create in upper and recurse
				if err := os.MkdirAll(upperAbsPath, 0755); err != nil {
					return fmt.Errorf("creating dir placeholder: %w", err)
				}
				if err := h.flattenRedirectDirRecursive(upperAbsPath, lowerAbsPath, lowerFileRelPath); err != nil {
					return err
				}
			}
			// If upper has non-dir at this name, skip (type conflict, upper wins)
		} else {
			// Lower is a file/symlink
			absoluteRedirect := "/" + lowerFileRelPath
			if !inUpper {
				// File only in lower — create metacopy placeholder.
				// A metacopy file is a sparse file with correct size + metadata,
				// plus trusted.overlay.metacopy="" and trusted.overlay.redirect xattrs.
				// The kernel reads actual data from the lower file via the redirect path.
				if err := h.createMetacopy(lowerAbsPath, upperAbsPath, lowerInfo, absoluteRedirect); err != nil {
					return fmt.Errorf("creating metacopy for %s: %w", name, err)
				}
			} else if upperInfo.Mode()&os.ModeSymlink != 0 && lowerInfo.Mode()&os.ModeSymlink != 0 {
				// Both are symlinks — set redirect xattr on upper symlink for move tracking.
				// We use Lsetxattr to avoid following the symlink.
				if err := unix.Lsetxattr(upperAbsPath, redirectXattr, []byte(absoluteRedirect), 0); err != nil {
					return fmt.Errorf("setting redirect on symlink %s: %w", name, err)
				}
			}
		}
	}

	return nil
}

// removeStaleEntries walks upper and removes entries that are stale:
// 1. Files identical to lower (same path, same content)
// 2. Whiteouts pointing to nonexistent lower entries
// 3. File redirects pointing to nonexistent lower targets
func (h *Housekeeper) removeStaleEntries() (int, error) {
	count := 0

	err := filepath.Walk(h.upperDir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if path == h.upperDir {
			return nil
		}

		relPath, err := filepath.Rel(h.upperDir, path)
		if err != nil {
			return nil
		}
		lowerPath := filepath.Join(h.lowerDir, relPath)

		// Skip directories — they are handled by cleanEmptyDirs
		if info.IsDir() {
			return nil
		}

		// Check whiteout
		if h.isWhiteoutFile(path, info) {
			target := filepath.Base(path)
			targetLowerPath := filepath.Join(filepath.Dir(lowerPath), target)
			if _, err := os.Lstat(targetLowerPath); os.IsNotExist(err) {
				// Orphaned whiteout
				if err := os.Remove(path); err != nil && !os.IsNotExist(err) {
					return fmt.Errorf("removing orphaned whiteout %s: %w", relPath, err)
				}
				count++
			}
			return nil
		}

		// Check file redirect
		redirect := h.getRedirect(path)
		if redirect != "" {
			parentLowerRelDir := h.resolveParentLowerRelDir(filepath.Dir(relPath))
			resolvedLowerRelPath := h.resolveRedirectRel(redirect, parentLowerRelDir)
			resolvedLowerPath := filepath.Join(h.lowerDir, resolvedLowerRelPath)
			lowerInfo, lErr := os.Lstat(resolvedLowerPath)
			if os.IsNotExist(lErr) {
				// Redirect target doesn't exist — remove
				if err := os.Remove(path); err != nil && !os.IsNotExist(err) {
					return fmt.Errorf("removing dead redirect %s: %w", relPath, err)
				}
				count++
			} else if resolvedLowerRelPath == relPath {
				// Redirect pointing to self (move then move back) — check if effectively unchanged
				shouldRemove := false
				if h.hasMetacopyXattr(path) {
					// Metacopy: content identical by definition
					shouldRemove = true
				} else if lErr == nil && filesAreIdentical(path, info, resolvedLowerPath, lowerInfo) {
					// Not metacopy but content is identical to lower
					shouldRemove = true
				}
				if shouldRemove {
					if err := os.Remove(path); err != nil && !os.IsNotExist(err) {
						return fmt.Errorf("removing no-op redirect %s: %w", relPath, err)
					}
					count++
				}
			}
			return nil
		}

		// Check identical file
		lowerInfo, err := os.Lstat(lowerPath)
		if err != nil {
			return nil // No lower counterpart — not stale
		}

		if filesAreIdentical(path, info, lowerPath, lowerInfo) {
			if err := os.Remove(path); err != nil && !os.IsNotExist(err) {
				return fmt.Errorf("removing identical file %s: %w", relPath, err)
			}
			count++
		}

		return nil
	})

	return count, err
}

// filesAreIdentical checks if two files are identical (same type, mode, size, and content).
func filesAreIdentical(path1 string, info1 os.FileInfo, path2 string, info2 os.FileInfo) bool {
	// Must be same type
	if info1.Mode().Type() != info2.Mode().Type() {
		return false
	}

	// Symlink comparison
	if info1.Mode()&os.ModeSymlink != 0 {
		target1, err1 := os.Readlink(path1)
		target2, err2 := os.Readlink(path2)
		return err1 == nil && err2 == nil && target1 == target2
	}

	// Regular file comparison
	if !info1.Mode().IsRegular() || !info2.Mode().IsRegular() {
		return false
	}

	if info1.Size() != info2.Size() {
		return false
	}

	// Compare content
	f1, err := os.Open(path1)
	if err != nil {
		return false
	}
	defer f1.Close()

	f2, err := os.Open(path2)
	if err != nil {
		return false
	}
	defer f2.Close()

	buf1 := make([]byte, 32*1024)
	buf2 := make([]byte, 32*1024)
	for {
		n1, err1 := f1.Read(buf1)
		n2, err2 := f2.Read(buf2)
		if n1 != n2 || !bytes.Equal(buf1[:n1], buf2[:n2]) {
			return false
		}
		if err1 == io.EOF && err2 == io.EOF {
			return true
		}
		if err1 != nil || err2 != nil {
			return false
		}
	}
}

// cleanEmptyDirs removes unnecessary empty directories from the upper layer.
// An empty directory is removed if the same directory exists in lower
// (it's a meaningless copy). New empty directories (not in lower) are kept.
func (h *Housekeeper) cleanEmptyDirs() error {
	var dirs []string
	err := filepath.Walk(h.upperDir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if info.IsDir() && path != h.upperDir {
			dirs = append(dirs, path)
		}
		return nil
	})
	if err != nil {
		return err
	}

	// Process deepest first
	for i := len(dirs) - 1; i >= 0; i-- {
		dir := dirs[i]
		entries, err := os.ReadDir(dir)
		if err != nil {
			continue
		}
		if len(entries) == 0 {
			relPath, err := filepath.Rel(h.upperDir, dir)
			if err != nil {
				continue
			}
			lowerPath := filepath.Join(h.lowerDir, relPath)
			if _, err := os.Lstat(lowerPath); err == nil {
				os.Remove(dir)
			}
		}
	}

	return nil
}

// createWhiteout creates a whiteout entry.
// First tries character device (Linux standard, requires root),
// then falls back to xattr-based whiteout (works without root on macOS).
func (h *Housekeeper) createWhiteout(path string) error {
	dev := unix.Mkdev(0, 0)
	err := unix.Mknod(path, unix.S_IFCHR|0666, int(dev))
	if err == nil {
		return nil
	}
	// Fall back to xattr-based whiteout
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	f.Close()
	return unix.Setxattr(path, whiteoutXattr, []byte("y"), 0)
}

// removeOpaqueAttr removes the opaque xattr from a directory.
func (h *Housekeeper) removeOpaqueAttr(path string) error {
	err := unix.Removexattr(path, opaqueXattr)
	if err != nil && err != unix.ENODATA {
		return err
	}
	return nil
}

// removeRedirectAttr removes the redirect xattr from a path.
func (h *Housekeeper) removeRedirectAttr(path string) error {
	err := unix.Removexattr(path, redirectXattr)
	if err != nil && err != unix.ENODATA {
		return err
	}
	return nil
}

// getRedirect reads the redirect xattr from a path.
// Uses Lgetxattr to read xattrs from symlinks themselves (not their targets).
func (h *Housekeeper) getRedirect(path string) string {
	buf := make([]byte, 512)
	size, err := unix.Lgetxattr(path, redirectXattr, buf)
	if err != nil || size == 0 {
		return ""
	}
	return string(buf[:size])
}

// hasMetacopyXattr checks if a file has the metacopy xattr set.
// Uses Lgetxattr to read xattrs from symlinks themselves (not their targets).
func (h *Housekeeper) hasMetacopyXattr(path string) bool {
	buf := make([]byte, 1)
	_, err := unix.Lgetxattr(path, metacopyXattr, buf)
	return err == nil
}

// resolveRedirect resolves a redirect value to an absolute lower path.
func (h *Housekeeper) resolveRedirect(redirect string, currentLowerDir string) string {
	if strings.HasPrefix(redirect, "/") {
		return filepath.Join(h.lowerDir, strings.TrimPrefix(redirect, "/"))
	}
	return filepath.Join(currentLowerDir, redirect)
}

// resolveRedirectRel resolves a redirect value to a relative path from overlay root.
func (h *Housekeeper) resolveRedirectRel(redirect string, currentRelDir string) string {
	if strings.HasPrefix(redirect, "/") {
		return strings.TrimPrefix(redirect, "/")
	}
	return filepath.Join(currentRelDir, redirect)
}

// resolveParentLowerDir resolves the actual lower directory path for a given
// upper relative directory, following any redirect xattrs on ancestor directories.
func (h *Housekeeper) resolveParentLowerDir(relDir string) string {
	if relDir == "." || relDir == "" {
		return h.lowerDir
	}

	parts := strings.Split(filepath.Clean(relDir), string(filepath.Separator))
	lowerPath := h.lowerDir

	for i, part := range parts {
		upperComponentPath := filepath.Join(h.upperDir, filepath.Join(parts[:i+1]...))
		redirect := h.getRedirect(upperComponentPath)
		if redirect != "" {
			lowerPath = h.resolveRedirect(redirect, lowerPath)
		} else {
			lowerPath = filepath.Join(lowerPath, part)
		}
	}

	return lowerPath
}

// resolveParentLowerRelDir resolves the relative lower path for a given
// upper relative directory, following any redirect xattrs on ancestor directories.
func (h *Housekeeper) resolveParentLowerRelDir(relDir string) string {
	if relDir == "." || relDir == "" {
		return ""
	}

	parts := strings.Split(filepath.Clean(relDir), string(filepath.Separator))
	lowerRel := ""

	for i, part := range parts {
		upperComponentPath := filepath.Join(h.upperDir, filepath.Join(parts[:i+1]...))
		redirect := h.getRedirect(upperComponentPath)
		if redirect != "" {
			lowerRel = h.resolveRedirectRel(redirect, lowerRel)
		} else {
			lowerRel = filepath.Join(lowerRel, part)
		}
	}

	return lowerRel
}

// createMetacopy creates a metacopy placeholder file in upper that references
// the given lower file via redirect. The file is sparse (no data blocks) but has
// the correct size and metadata. The kernel reads actual data from the lower file.
func (h *Housekeeper) createMetacopy(lowerPath, upperPath string, lowerInfo os.FileInfo, redirect string) error {
	if lowerInfo.Mode()&os.ModeSymlink != 0 {
		// Symlinks cannot be metacopy — copy the link target
		target, err := os.Readlink(lowerPath)
		if err != nil {
			return err
		}
		if err := os.Symlink(target, upperPath); err != nil {
			return err
		}
		// Set redirect xattr on the symlink itself (use Lsetxattr to avoid following the symlink)
		return unix.Lsetxattr(upperPath, redirectXattr, []byte(redirect), 0)
	}

	// Create sparse file with correct size
	f, err := os.OpenFile(upperPath, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, lowerInfo.Mode().Perm())
	if err != nil {
		return err
	}
	if err := f.Truncate(lowerInfo.Size()); err != nil {
		f.Close()
		return err
	}
	f.Close()

	// Copy modification time
	if err := os.Chtimes(upperPath, lowerInfo.ModTime(), lowerInfo.ModTime()); err != nil {
		return err
	}

	// Set metacopy xattr (empty value)
	if err := unix.Setxattr(upperPath, metacopyXattr, nil, 0); err != nil {
		return err
	}

	// Set redirect to the lower file path
	return unix.Setxattr(upperPath, redirectXattr, []byte(redirect), 0)
}

// removeIllegalRedirects finds and removes entries in upper that have illegal redirects.
// An illegal redirect is one where the redirect xattr resolves to a path outside the overlay root.
// Returns the number of entries removed.
func (h *Housekeeper) removeIllegalRedirects() (int, error) {
	count := 0

	// Collect entries to remove (we can't modify during walk)
	var toRemove []string

	err := filepath.Walk(h.upperDir, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		if path == h.upperDir {
			return nil
		}

		relPath, err := filepath.Rel(h.upperDir, path)
		if err != nil {
			return nil
		}

		// Check redirect xattr
		redirect := h.getRedirect(path)
		if redirect != "" && h.isIllegalRedirect(relPath, redirect) {
			toRemove = append(toRemove, path)
			if info.IsDir() {
				return filepath.SkipDir
			}
			return nil
		}

		return nil
	})
	if err != nil {
		return 0, err
	}

	// Remove entries safely (only within upper)
	for _, path := range toRemove {
		if err := h.safeRemoveInUpper(path); err != nil {
			return count, fmt.Errorf("removing illegal redirect %s: %w", path, err)
		}
		count++
	}

	return count, nil
}

// isIllegalRedirect checks if a redirect value would resolve to a path outside the overlay.
// relPath is the relative path of the entry in upper.
// redirect is the redirect xattr value.
func (h *Housekeeper) isIllegalRedirect(relPath, redirect string) bool {
	// Resolve the redirect to a relative path from overlay root
	parentRelDir := filepath.Dir(relPath)
	if parentRelDir == "." {
		parentRelDir = ""
	}

	// For directory redirects, resolve through parent chain
	parentLowerRelDir := h.resolveParentLowerRelDir(parentRelDir)
	resolvedRel := h.resolveRedirectRel(redirect, parentLowerRelDir)

	// Clean and check for escape
	return h.pathEscapesRoot(resolvedRel)
}

// pathEscapesRoot checks if a relative path escapes the overlay root.
// A path escapes if after cleaning it starts with ".." or is outside the root.
func (h *Housekeeper) pathEscapesRoot(relPath string) bool {
	// Clean the path to resolve . and ..
	cleaned := filepath.Clean(relPath)

	// Check for escape patterns
	if cleaned == ".." || strings.HasPrefix(cleaned, ".."+string(filepath.Separator)) {
		return true
	}

	// Also verify the absolute path stays within bounds
	absPath := filepath.Join(h.lowerDir, cleaned)
	absPath = filepath.Clean(absPath)

	// Check it's still under lowerDir
	lowerDirClean := filepath.Clean(h.lowerDir)
	if !strings.HasPrefix(absPath, lowerDirClean+string(filepath.Separator)) && absPath != lowerDirClean {
		return true
	}

	return false
}

// safeRemoveInUpper removes a path only if it's verified to be within upper.
// This prevents accidental deletion outside the upper directory.
func (h *Housekeeper) safeRemoveInUpper(path string) error {
	// Verify path is within upper
	absPath, err := filepath.Abs(path)
	if err != nil {
		return fmt.Errorf("getting absolute path: %w", err)
	}
	absPath = filepath.Clean(absPath)

	upperClean := filepath.Clean(h.upperDir)
	upperAbs, err := filepath.Abs(upperClean)
	if err != nil {
		return fmt.Errorf("getting absolute upper path: %w", err)
	}

	if !strings.HasPrefix(absPath, upperAbs+string(filepath.Separator)) {
		return fmt.Errorf("refusing to remove path outside upper: %s", path)
	}

	return os.RemoveAll(path)
}

// isWhiteoutFile checks if a file is a whiteout entry.
func (h *Housekeeper) isWhiteoutFile(path string, info os.FileInfo) bool {
	// Character device with major:minor = 0:0
	if info.Mode()&os.ModeCharDevice != 0 {
		if stat, ok := info.Sys().(*syscall.Stat_t); ok {
			major := unix.Major(uint64(stat.Rdev))
			minor := unix.Minor(uint64(stat.Rdev))
			if major == 0 && minor == 0 {
				return true
			}
		}
	}
	// xattr-based whiteout (for testing on macOS)
	if info.Mode().IsRegular() && info.Size() == 0 {
		val := make([]byte, 1)
		size, err := unix.Getxattr(path, whiteoutXattr, val)
		if err == nil && size > 0 && val[0] == 'y' {
			return true
		}
	}
	return false
}
