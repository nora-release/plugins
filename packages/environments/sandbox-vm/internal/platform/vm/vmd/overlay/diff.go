//go:build linux || darwin

package overlay

import (
	"fmt"
	"log"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"syscall"
	"time"

	"golang.org/x/sys/unix"
)

const (
	// opaqueXattr is the xattr key for opaque directories
	opaqueXattr = "trusted.overlay.opaque"
	// whiteoutXattr is the xattr key for whiteout files (alternative to character device)
	whiteoutXattr = "trusted.overlay.whiteout"
	// redirectXattr is the xattr key for redirect (set on renamed/moved files and directories)
	redirectXattr = "trusted.overlay.redirect"
	// metacopyXattr is the xattr key for metacopy (metadata-only copy-up, data stays in lower)
	metacopyXattr = "trusted.overlay.metacopy"
)

// FileDiff represents a single file or directory change in the overlay.
type FileDiff struct {
	Path      string
	Mode      uint32
	IsDir     bool
	IsUpdated bool
	IsDeleted bool
	MovedFrom string
	Timestamp time.Time
	Size      int64
}

// mergedEntry represents an entry in the merged view of upper and lower layers.
type mergedEntry struct {
	Name         string      // Name in the merged view
	UpperAbsPath string      // Absolute path in upper layer (empty if not in upper)
	UpperInfo    os.FileInfo // File info from upper layer
	LowerAbsPath string      // Absolute path in lower layer (empty if not in lower)
	LowerInfo    os.FileInfo // File info from lower layer
	IsWhiteout   bool        // Upper entry is a whiteout
	IsOpaque     bool        // Upper directory is opaque
	Redirect     string      // Upper entry's redirect xattr value
}

// lowerFileEntry represents a file entry in the lower (read-only) layer,
// typically used for tracking deleted files.
type lowerFileEntry struct {
	RelPath   string      // Relative path in the lower layer
	AbsPath   string      // Absolute path on disk in the lower layer
	Info      os.FileInfo // File info from the lower layer
	Timestamp time.Time   // Deletion timestamp (inherited from whiteout or opaque dir)
}

// OverlayDiffAnalyzerResult holds the result of overlay diff analysis.
type OverlayDiffAnalyzerResult struct {
	FileDiff []FileDiff
}

// OverlayDiffAnalyzer analyzes overlay filesystem changes.
type OverlayDiffAnalyzer struct {
	upperDir string
	lowerDir string
}

// NewOverlayDiffAnalyzer creates a new OverlayDiffAnalyzer with the given directories.
func NewOverlayDiffAnalyzer(upperDir, lowerDir string) (*OverlayDiffAnalyzer, error) {
	return &OverlayDiffAnalyzer{
		upperDir: upperDir,
		lowerDir: lowerDir,
	}, nil
}

// Close cleans up the analyzer's resources.
func (a *OverlayDiffAnalyzer) Close() error {
	return nil
}

// mergeDir computes the merged view of a single directory level from upper and lower layers.
// upperDirPath and lowerDirPath are absolute paths. Either may be empty if the directory
// doesn't exist in that layer.
func (a *OverlayDiffAnalyzer) mergeDir(upperDirPath, lowerDirPath string, isOpaque bool) ([]mergedEntry, error) {
	// Read upper entries
	upperMap := make(map[string]*mergedEntry)
	whiteoutMap := make(map[string]*mergedEntry) // target name -> whiteout entry
	whiteoutTargets := make(map[string]bool)     // names deleted by whiteouts

	if upperDirPath != "" {
		upperDirEntries, err := os.ReadDir(upperDirPath)
		if err != nil && !os.IsNotExist(err) {
			return nil, fmt.Errorf("reading upper dir %s: %w", upperDirPath, err)
		}

		for _, de := range upperDirEntries {
			absPath := filepath.Join(upperDirPath, de.Name())
			info, err := os.Lstat(absPath)
			if err != nil {
				continue
			}

			// Check if whiteout
			if a.isWhiteout(absPath, info) {
				target := a.getWhiteoutTarget(absPath, info)
				whiteoutTargets[target] = true
				whiteoutMap[target] = &mergedEntry{
					Name:         target,
					UpperAbsPath: absPath,
					UpperInfo:    info,
					IsWhiteout:   true,
				}
				continue
			}

			entry := &mergedEntry{
				Name:         de.Name(),
				UpperAbsPath: absPath,
				UpperInfo:    info,
			}

			// Check opaque and redirect xattrs
			if info.IsDir() {
				entry.IsOpaque = a.isOpaqueDir(absPath)
			}
			entry.Redirect = a.getRedirect(absPath)

			upperMap[de.Name()] = entry
		}
	}

	// Read lower entries and merge
	var result []mergedEntry

	if lowerDirPath != "" && !isOpaque {
		lowerDirEntries, err := os.ReadDir(lowerDirPath)
		if err != nil && !os.IsNotExist(err) {
			return nil, fmt.Errorf("reading lower dir %s: %w", lowerDirPath, err)
		}

		matchedWhiteouts := make(map[string]bool)
		for _, de := range lowerDirEntries {
			name := de.Name()

			// Deleted by whiteout
			if whiteoutTargets[name] {
				matchedWhiteouts[name] = true
				// Add as whiteout entry (lower exists, deleted by upper)
				lowerAbsPath := filepath.Join(lowerDirPath, name)
				lowerInfo, err := os.Lstat(lowerAbsPath)
				if err != nil {
					continue
				}
				result = append(result, mergedEntry{
					Name:         name,
					LowerAbsPath: lowerAbsPath,
					LowerInfo:    lowerInfo,
					IsWhiteout:   true,
				})
				continue
			}

			if upper, exists := upperMap[name]; exists {
				// Both layers have this entry
				lowerAbsPath := filepath.Join(lowerDirPath, name)
				lowerInfo, err := os.Lstat(lowerAbsPath)
				if err == nil {
					upper.LowerAbsPath = lowerAbsPath
					upper.LowerInfo = lowerInfo
				}
			} else {
				// Only in lower, not covered by upper
				// Include in result so mergeWalk can detect moves when paths differ (redirect)
				lowerAbsPath := filepath.Join(lowerDirPath, name)
				lowerInfo, err := os.Lstat(lowerAbsPath)
				if err == nil {
					result = append(result, mergedEntry{
						Name:         name,
						LowerAbsPath: lowerAbsPath,
						LowerInfo:    lowerInfo,
					})
				}
			}
		}

		// Whiteouts that didn't match any lower entry are stale
		for target := range whiteoutTargets {
			if !matchedWhiteouts[target] {
				if woEntry, ok := whiteoutMap[target]; ok {
					woEntry.LowerAbsPath = "" // confirm no lower
					result = append(result, *woEntry)
				}
			}
		}
	} else if lowerDirPath != "" && isOpaque {
		// Opaque directory: all lower entries are considered deleted
		lowerDirEntries, err := os.ReadDir(lowerDirPath)
		if err != nil && !os.IsNotExist(err) {
			return nil, fmt.Errorf("reading lower dir %s: %w", lowerDirPath, err)
		}

		for _, de := range lowerDirEntries {
			name := de.Name()
			lowerAbsPath := filepath.Join(lowerDirPath, name)
			lowerInfo, err := os.Lstat(lowerAbsPath)
			if err != nil {
				continue
			}

			if upper, exists := upperMap[name]; exists {
				// Upper covers this entry — set lower info on existing upper entry
				upper.LowerAbsPath = lowerAbsPath
				upper.LowerInfo = lowerInfo
			} else {
				// Lower entry not covered by upper — deleted by opaque
				result = append(result, mergedEntry{
					Name:         name,
					LowerAbsPath: lowerAbsPath,
					LowerInfo:    lowerInfo,
					IsWhiteout:   true, // Effectively deleted by opaque
				})
			}
		}
	}

	// If no lower dir was processed, all whiteouts are stale
	if lowerDirPath == "" && len(whiteoutTargets) > 0 {
		for target := range whiteoutTargets {
			if woEntry, ok := whiteoutMap[target]; ok {
				woEntry.LowerAbsPath = ""
				result = append(result, *woEntry)
			}
		}
	}

	// Add all upper entries to result
	for _, entry := range upperMap {
		result = append(result, *entry)
	}

	return result, nil
}

// mergeWalkResult holds the collected data from a merge walk.
type mergeWalkResult struct {
	creates      []FileDiff
	updates      []FileDiff
	deletedFiles map[string]*lowerFileEntry
}

// mergeWalk recursively walks the merged view of upper and lower layers,
// collecting creates, deletes, updates, and stale entries.
func (a *OverlayDiffAnalyzer) mergeWalk(relDir string, lowerRelDir string, upperDirPath, lowerDirPath string, isOpaque bool, res *mergeWalkResult) error {
	entries, err := a.mergeDir(upperDirPath, lowerDirPath, isOpaque)
	if err != nil {
		return err
	}

	for _, entry := range entries {
		entryRelPath := filepath.Join(relDir, entry.Name)
		lowerEntryRelPath := filepath.Join(lowerRelDir, entry.Name)

		if entry.IsWhiteout {
			if entry.LowerAbsPath == "" {
				// Whiteout for nonexistent lower entry — skip
			} else {
				// Deleted entry — collect it and all children
				a.collectDeleted(lowerEntryRelPath, entry.LowerAbsPath, entry.LowerInfo, res)
			}
			continue
		}

		hasUpper := entry.UpperAbsPath != ""
		hasLower := entry.LowerAbsPath != ""
		pathDiffers := entryRelPath != lowerEntryRelPath

		if hasUpper && !hasLower {
			// Only in upper — created
			if entry.Redirect != "" {
				if entry.UpperInfo.IsDir() {
					// Directory with redirect — resolve lower path for children
					resolvedLowerPath := a.resolveRedirect(entry.Redirect, lowerDirPath)
					// Check if redirect target exists; if not, skip
					if _, err := os.Lstat(resolvedLowerPath); os.IsNotExist(err) {
						continue
					}
					// Emit directory itself as a new entry
					res.creates = append(res.creates, FileDiff{
						Path:      entryRelPath,
						Mode:      uint32(entry.UpperInfo.Mode().Perm()),
						IsDir:     true,
						Timestamp: entry.UpperInfo.ModTime(),
					})
					resolvedLowerRelDir := a.resolveRedirectRel(entry.Redirect, lowerRelDir)
					if err := a.mergeWalk(entryRelPath, resolvedLowerRelDir, entry.UpperAbsPath, resolvedLowerPath, entry.IsOpaque, res); err != nil {
						return err
					}
					continue
				}
				// File with redirect — treat as move
				resolvedLowerRelPath := a.resolveRedirectRel(entry.Redirect, lowerRelDir)
				// Check if this is a metacopy (pure move, no content change)
				isMetacopy := a.hasMetacopyXattr(entry.UpperAbsPath)
				diff := FileDiff{
					Path:      entryRelPath,
					Mode:      uint32(entry.UpperInfo.Mode().Perm()),
					IsDir:     false,
					IsUpdated: !isMetacopy,
					MovedFrom: resolvedLowerRelPath,
					Timestamp: entry.UpperInfo.ModTime(),
					Size:      entry.UpperInfo.Size(),
				}
				res.creates = append(res.creates, diff)
				continue
			}

			diff := FileDiff{
				Path:      entryRelPath,
				Mode:      uint32(entry.UpperInfo.Mode().Perm()),
				IsDir:     entry.UpperInfo.IsDir(),
				Timestamp: entry.UpperInfo.ModTime(),
				Size:      entry.UpperInfo.Size(),
			}
			res.creates = append(res.creates, diff)

			// If it's a directory, walk children (all will be creates)
			if entry.UpperInfo.IsDir() {
				if err := a.mergeWalk(entryRelPath, lowerEntryRelPath, entry.UpperAbsPath, "", entry.IsOpaque, res); err != nil {
					return err
				}
			}
		} else if hasUpper && hasLower {
			// Both layers have this entry
			if entry.Redirect != "" {
				// Has redirect — resolve actual lower path
				resolvedLowerPath := a.resolveRedirect(entry.Redirect, lowerDirPath)
				// Check if redirect target exists; if not, skip
				if _, err := os.Lstat(resolvedLowerPath); os.IsNotExist(err) {
					continue
				}
				resolvedLowerRelDir := a.resolveRedirectRel(entry.Redirect, lowerRelDir)

				if entry.UpperInfo.IsDir() {
					// Directory with redirect — recurse with resolved lower
					if err := a.mergeWalk(entryRelPath, resolvedLowerRelDir, entry.UpperAbsPath, resolvedLowerPath, entry.IsOpaque, res); err != nil {
						return err
					}
					// The original lower directory's children are implicitly deleted
					// because the directory now points to a different source.
					// Collect them as deletions (moves will be subtracted later in Analyze).
					if entry.LowerInfo.IsDir() {
						a.collectDeletedChildren(entry.LowerAbsPath, entry.UpperInfo.ModTime(), res)
					}
				} else {
					// File with redirect — treat as move
					diff := FileDiff{
						Path:      entryRelPath,
						Mode:      uint32(entry.UpperInfo.Mode().Perm()),
						IsDir:     false,
						MovedFrom: resolvedLowerRelDir,
						Timestamp: entry.UpperInfo.ModTime(),
						Size:      entry.UpperInfo.Size(),
					}
					res.creates = append(res.creates, diff)
				}
			} else if pathDiffers {
				// Path differs due to parent redirect — this is a move
				if entry.UpperInfo.IsDir() && entry.LowerInfo.IsDir() {
					// Directory move — recurse to detect child moves
					childOpaque := entry.IsOpaque || isOpaque
					if err := a.mergeWalk(entryRelPath, lowerEntryRelPath, entry.UpperAbsPath, entry.LowerAbsPath, childOpaque, res); err != nil {
						return err
					}
				} else if !entry.UpperInfo.IsDir() && !entry.LowerInfo.IsDir() {
					// File move — file is in upper, assume modified
					diff := FileDiff{
						Path:      entryRelPath,
						Mode:      uint32(entry.UpperInfo.Mode().Perm()),
						IsDir:     false,
						IsUpdated: true,
						MovedFrom: lowerEntryRelPath,
						Timestamp: entry.UpperInfo.ModTime(),
						Size:      entry.UpperInfo.Size(),
					}
					res.creates = append(res.creates, diff)
				} else {
					// Type changed during move — treat as delete old + create new
					a.collectDeleted(lowerEntryRelPath, entry.LowerAbsPath, entry.LowerInfo, res)
					diff := FileDiff{
						Path:      entryRelPath,
						Mode:      uint32(entry.UpperInfo.Mode().Perm()),
						IsDir:     entry.UpperInfo.IsDir(),
						Timestamp: entry.UpperInfo.ModTime(),
						Size:      entry.UpperInfo.Size(),
					}
					res.creates = append(res.creates, diff)
				}
			} else if entry.UpperInfo.IsDir() && entry.LowerInfo.IsDir() {
				// Both are directories — recurse
				// Propagate opaque: if parent is opaque, children are also effectively opaque
				childOpaque := entry.IsOpaque || isOpaque
				if err := a.mergeWalk(entryRelPath, lowerEntryRelPath, entry.UpperAbsPath, entry.LowerAbsPath, childOpaque, res); err != nil {
					return err
				}
			} else if entry.UpperInfo.IsDir() != entry.LowerInfo.IsDir() {
				// Type mismatch
				diff := FileDiff{
					Path:      entryRelPath,
					Mode:      uint32(entry.UpperInfo.Mode().Perm()),
					IsDir:     entry.UpperInfo.IsDir(),
					IsUpdated: true,
					Timestamp: entry.UpperInfo.ModTime(),
					Size:      entry.UpperInfo.Size(),
				}
				res.updates = append(res.updates, diff)

				if entry.LowerInfo.IsDir() && !entry.UpperInfo.IsDir() {
					// Lower was a directory — its children are deleted
					a.collectDeletedChildren(entry.LowerAbsPath, entry.UpperInfo.ModTime(), res)
				} else if !entry.LowerInfo.IsDir() && entry.UpperInfo.IsDir() {
					// Upper is a directory — recurse into its children as creates
					if err := a.mergeWalk(entryRelPath, lowerEntryRelPath, entry.UpperAbsPath, "", false, res); err != nil {
						return err
					}
				}
			} else {
				// Both are files — file is in upper, assume modified
				diff := FileDiff{
					Path:      entryRelPath,
					Mode:      uint32(entry.UpperInfo.Mode().Perm()),
					IsDir:     false,
					IsUpdated: true,
					Timestamp: entry.UpperInfo.ModTime(),
					Size:      entry.UpperInfo.Size(),
				}
				res.updates = append(res.updates, diff)
			}
		} else if !hasUpper && hasLower && pathDiffers {
			// Only in lower, but path differs due to parent redirect — this is a move
			if entry.LowerInfo.IsDir() {
				// Directory only in lower under redirected parent — recurse to report child moves
				if err := a.mergeWalk(entryRelPath, lowerEntryRelPath, "", entry.LowerAbsPath, false, res); err != nil {
					return err
				}
			} else {
				// File only in lower under redirected parent — moved (unchanged content)
				diff := FileDiff{
					Path:      entryRelPath,
					Mode:      uint32(entry.LowerInfo.Mode().Perm()),
					IsDir:     false,
					MovedFrom: lowerEntryRelPath,
					Timestamp: entry.LowerInfo.ModTime(),
					Size:      entry.LowerInfo.Size(),
				}
				res.creates = append(res.creates, diff)
			}
		}
		// hasUpper==false && hasLower==true && !pathDiffers: only in lower, no change — skip
	}

	return nil
}

// resolveRedirectRel resolves a redirect xattr value to a relative lower path.
func (a *OverlayDiffAnalyzer) resolveRedirectRel(redirect string, currentLowerRelDir string) string {
	if strings.HasPrefix(redirect, "/") {
		return strings.TrimPrefix(redirect, "/")
	}
	return filepath.Join(currentLowerRelDir, redirect)
}

// resolveRedirect resolves a redirect xattr value to an absolute lower path.
func (a *OverlayDiffAnalyzer) resolveRedirect(redirect string, currentLowerDir string) string {
	if strings.HasPrefix(redirect, "/") {
		// Absolute redirect from overlay root
		return filepath.Join(a.lowerDir, strings.TrimPrefix(redirect, "/"))
	}
	// Relative redirect — replace name component in current lower dir
	return filepath.Join(currentLowerDir, redirect)
}

// collectDeleted adds a lower entry and all its children as deletions.
func (a *OverlayDiffAnalyzer) collectDeleted(relPath string, absPath string, info os.FileInfo, res *mergeWalkResult) {
	res.deletedFiles[relPath] = &lowerFileEntry{
		RelPath: relPath,
		AbsPath: absPath,
		Info:    info,
	}

	if info.IsDir() {
		filepath.Walk(absPath, func(path string, childInfo os.FileInfo, walkErr error) error {
			if walkErr != nil || path == absPath {
				return nil
			}
			childRelPath, err := filepath.Rel(a.lowerDir, path)
			if err != nil {
				return nil
			}
			res.deletedFiles[childRelPath] = &lowerFileEntry{
				RelPath: childRelPath,
				AbsPath: path,
				Info:    childInfo,
			}
			return nil
		})
	}
}

// collectDeletedChildren adds all children of a lower directory as deletions.
func (a *OverlayDiffAnalyzer) collectDeletedChildren(lowerDirPath string, timestamp time.Time, res *mergeWalkResult) {
	filepath.Walk(lowerDirPath, func(path string, info os.FileInfo, walkErr error) error {
		if walkErr != nil || path == lowerDirPath {
			return nil
		}
		relPath, err := filepath.Rel(a.lowerDir, path)
		if err != nil {
			return nil
		}
		res.deletedFiles[relPath] = &lowerFileEntry{
			RelPath: relPath,
			AbsPath: path,
			Info:    info,
		}
		return nil
	})
}

// Analyze walks the merged view of upper and lower layers and produces a list of file changes.
func (a *OverlayDiffAnalyzer) Analyze() (*OverlayDiffAnalyzerResult, error) {
	overallStart := time.Now()
	defer func() {
		log.Printf("OverlayDiffAnalyzer.Analyze took %v", time.Since(overallStart))
	}()

	// Step 1: Merge walk to collect creates, deletes, updates, and stale entries
	walkStart := time.Now()
	res := &mergeWalkResult{
		deletedFiles: make(map[string]*lowerFileEntry),
	}

	if err := a.mergeWalk("", "", a.upperDir, a.lowerDir, false, res); err != nil {
		return nil, fmt.Errorf("merge walk: %w", err)
	}
	log.Printf("OverlayDiffAnalyzer.Analyze: mergeWalk took %v (creates=%d, deletes=%d, updates=%d)",
		time.Since(walkStart), len(res.creates), len(res.deletedFiles), len(res.updates))

	// Step 2: Collect all changes
	var changes []FileDiff
	changes = append(changes, res.creates...)
	changes = append(changes, res.updates...)

	// Remove deletes that were already matched as moves (via redirect)
	for i := range changes {
		if changes[i].MovedFrom != "" {
			delete(res.deletedFiles, changes[i].MovedFrom)
		}
	}

	// Add remaining deletes
	for relPath, entry := range res.deletedFiles {
		changes = append(changes, FileDiff{
			Path:      relPath,
			IsDir:     entry.Info.IsDir(),
			IsDeleted: true,
			Timestamp: entry.Timestamp,
		})
	}

	// Sort by path for consistent output
	sort.Slice(changes, func(i, j int) bool {
		return changes[i].Path < changes[j].Path
	})

	return &OverlayDiffAnalyzerResult{
		FileDiff: changes,
	}, nil
}

// isWhiteout checks if a file is a whiteout entry.
// A whiteout can be:
// 1. A character device with major:minor = 0:0 (Linux overlayfs standard)
// 2. A zero-byte file with trusted.overlay.whiteout xattr set to "y" (for testing on macOS)
func (a *OverlayDiffAnalyzer) isWhiteout(path string, info os.FileInfo) bool {
	// Check for character device whiteout (major:minor = 0:0)
	if info.Mode()&os.ModeCharDevice != 0 {
		if stat, ok := info.Sys().(*syscall.Stat_t); ok {
			major := unix.Major(uint64(stat.Rdev))
			minor := unix.Minor(uint64(stat.Rdev))
			if major == 0 && minor == 0 {
				return true
			}
		}
	}

	// Check for xattr-based whiteout (zero-byte file with whiteout xattr)
	if info.Mode().IsRegular() && info.Size() == 0 {
		val := make([]byte, 1)
		size, err := unix.Getxattr(path, whiteoutXattr, val)
		if err == nil && size > 0 && val[0] == 'y' {
			return true
		}
	}

	return false
}

// getWhiteoutTarget returns the original file name that the whiteout represents.
// For character device whiteouts, the file name is the target name directly.
func (a *OverlayDiffAnalyzer) getWhiteoutTarget(path string, _ os.FileInfo) string {
	return filepath.Base(path)
}

// getRedirect reads the trusted.overlay.redirect xattr from a file or directory.
// Returns empty string if no redirect is set.
// Uses Lgetxattr to read xattrs from symlinks themselves (not their targets).
func (a *OverlayDiffAnalyzer) getRedirect(path string) string {
	buf := make([]byte, 512)
	size, err := unix.Lgetxattr(path, redirectXattr, buf)
	if err != nil || size == 0 {
		return ""
	}
	return string(buf[:size])
}

// isOpaqueDir checks if a directory is marked as opaque.
func (a *OverlayDiffAnalyzer) isOpaqueDir(path string) bool {
	val := make([]byte, 1)
	size, err := unix.Getxattr(path, opaqueXattr, val)
	if err != nil || size == 0 {
		return false
	}
	return val[0] == 'y'
}

// hasMetacopyXattr checks if a file has the metacopy xattr set.
// Uses Lgetxattr to read xattrs from symlinks themselves (not their targets).
func (a *OverlayDiffAnalyzer) hasMetacopyXattr(path string) bool {
	buf := make([]byte, 1)
	_, err := unix.Lgetxattr(path, metacopyXattr, buf)
	return err == nil
}
