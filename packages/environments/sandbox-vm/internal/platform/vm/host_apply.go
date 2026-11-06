package vm

import (
	"archive/tar"
	"context"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"log"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/nora/sandbox-vm/pkg/types"
)

// The sandbox exists because code inside the VM is untrusted, so every path, link target,
// and mode it reports is untrusted too. Diff paths are absolute VM paths, and tar entries
// carry the same paths without the leading "/". Each one must fall inside a directory this
// VM mounted. It is then applied through an os.Root opened at that mount's host directory,
// so ".." and symlinks cannot leave the mount.

// hostMount is one mounted directory and the root that confines host writes to it.
type hostMount struct {
	vmPath string
	root   *os.Root
}

// hostMounts holds the mounted directories that accepted changes may touch.
type hostMounts struct {
	mounts []hostMount
}

// openHostMounts opens a root for each mount's host directory under hostBaseDir.
func openHostMounts(hostBaseDir string, mounts []Mount) (*hostMounts, error) {
	opened := &hostMounts{}
	for _, mount := range mounts {
		hostPath := filepath.Clean(strings.TrimSpace(mount.HostPath))
		vmPath := filepath.Clean(strings.TrimSpace(mount.VMPath))
		if mount.VMPath == "" {
			vmPath = hostPath
		}
		if !filepath.IsAbs(hostPath) || !filepath.IsAbs(vmPath) {
			opened.Close()
			return nil, fmt.Errorf("mount paths must be absolute: host %q, vm %q", mount.HostPath, mount.VMPath)
		}
		root, err := os.OpenRoot(filepath.Join(hostBaseDir, hostPath))
		if err != nil {
			opened.Close()
			return nil, fmt.Errorf("open mount %s: %w", hostPath, err)
		}
		opened.mounts = append(opened.mounts, hostMount{vmPath: vmPath, root: root})
	}
	// Prefer the most specific mount when mounts nest.
	sort.Slice(opened.mounts, func(i, j int) bool {
		return len(opened.mounts[i].vmPath) > len(opened.mounts[j].vmPath)
	})
	return opened, nil
}

func (h *hostMounts) Close() {
	for _, mount := range h.mounts {
		mount.root.Close()
	}
}

// relativeWithin returns path relative to base when path is base or inside it.
func relativeWithin(base, path string) (string, bool) {
	rel, err := filepath.Rel(base, path)
	if err != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
		return "", false
	}
	return rel, true
}

// locate maps an absolute VM path to its mount and the path relative to that mount. The
// relative path is "." for the mount directory itself.
func (h *hostMounts) locate(vmPath string) (*hostMount, string, error) {
	clean := filepath.Clean(filepath.FromSlash(strings.TrimSpace(vmPath)))
	if !filepath.IsAbs(clean) {
		return nil, "", fmt.Errorf("path %q is not an absolute VM path", vmPath)
	}
	for index := range h.mounts {
		if rel, ok := relativeWithin(h.mounts[index].vmPath, clean); ok {
			return &h.mounts[index], rel, nil
		}
	}
	return nil, "", fmt.Errorf("path %q is outside every mounted directory", vmPath)
}

// locateEntry locates a mounted path that is not the mount directory itself.
func (h *hostMounts) locateEntry(vmPath string) (*hostMount, string, error) {
	mount, rel, err := h.locate(vmPath)
	if err != nil {
		return nil, "", err
	}
	if rel == "." {
		return nil, "", fmt.Errorf("path %q is a mounted directory itself", vmPath)
	}
	return mount, rel, nil
}

// tarEntryPath turns a tar entry name, which omits the leading "/", into an absolute VM path.
func tarEntryPath(name string) string {
	return string(filepath.Separator) + strings.TrimLeft(filepath.FromSlash(name), string(filepath.Separator))
}

// symlinkTarget validates a symlink at rel inside mount and returns the target to create.
// Relative targets must resolve inside the mount. Absolute targets must point inside the
// same mount and are rewritten as relative, so they stay correct when the host path differs.
func symlinkTarget(mount *hostMount, rel, target string) (string, error) {
	if target == "" {
		return "", errors.New("symlink target is empty")
	}
	if filepath.IsAbs(target) {
		targetRel, ok := relativeWithin(mount.vmPath, filepath.Clean(target))
		if !ok {
			return "", fmt.Errorf("symlink target %q is outside the mount", target)
		}
		relative, err := filepath.Rel(filepath.Dir(rel), targetRel)
		if err != nil {
			return "", err
		}
		return relative, nil
	}
	resolved := filepath.Join(filepath.Dir(rel), filepath.FromSlash(target))
	if _, ok := relativeWithin(".", resolved); !ok {
		return "", fmt.Errorf("symlink target %q leaves the mount", target)
	}
	return target, nil
}

// removeIfSymlink removes name when it is a symlink, so a write replaces the link instead of
// following it.
func removeIfSymlink(root *os.Root, name string) error {
	info, err := root.Lstat(name)
	if errors.Is(err, fs.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	if info.Mode()&fs.ModeSymlink == 0 {
		return nil
	}
	return root.Remove(name)
}

// applyMovesInMounts performs pure renames (MovedFrom set, content unchanged). It returns the
// sources of pure moves it could not apply: the tar does not carry those files, so deleting
// their sources afterwards would lose them.
func applyMovesInMounts(mounts *hostMounts, changes []types.FileDiff) map[string]bool {
	keep := map[string]bool{}
	var moveCount int
	for _, change := range changes {
		if change.MovedFrom == "" || change.IsUpdated || change.IsDeleted {
			continue
		}
		skip := func(format string, args ...any) {
			log.Printf("Warning: "+format, args...)
			keep[change.MovedFrom] = true
		}
		sourceMount, source, err := mounts.locateEntry(change.MovedFrom)
		if err != nil {
			skip("skipping move: %v", err)
			continue
		}
		destinationMount, destination, err := mounts.locateEntry(change.Path)
		if err != nil {
			skip("skipping move: %v", err)
			continue
		}
		if sourceMount != destinationMount {
			skip("skipping move across mounts: %s -> %s", change.MovedFrom, change.Path)
			continue
		}
		root := sourceMount.root
		if _, err := root.Lstat(source); errors.Is(err, fs.ErrNotExist) {
			// Created and moved inside the VM; the tar carries it as a new file.
			log.Printf("Move source %s not found on host, skipping rename", change.MovedFrom)
			continue
		}
		if err := root.MkdirAll(filepath.Dir(destination), 0o755); err != nil {
			skip("failed to create directory for %s: %v", change.Path, err)
			continue
		}
		if err := root.Rename(source, destination); err != nil {
			skip("failed to move %s -> %s: %v", change.MovedFrom, change.Path, err)
			continue
		}
		moveCount++
	}
	if moveCount > 0 {
		log.Printf("Applied %d moves", moveCount)
	}
	return keep
}

// applyDeletionsInMounts removes deleted paths and move sources, deepest paths first. Sources
// in keep belong to pure moves that were not applied and stay where they are, so nothing is
// deleted at or above them either: a directory rename also deletes the old directory.
func applyDeletionsInMounts(mounts *hostMounts, changes []types.FileDiff, keep map[string]bool) {
	type deletion struct {
		mount *hostMount
		rel   string
	}
	var kept []deletion
	for source := range keep {
		if mount, rel, err := mounts.locateEntry(source); err == nil {
			kept = append(kept, deletion{mount: mount, rel: rel})
		}
	}
	containsKept := func(item deletion) bool {
		for _, k := range kept {
			if k.mount == item.mount &&
				(k.rel == item.rel || strings.HasPrefix(k.rel, item.rel+string(filepath.Separator))) {
				return true
			}
		}
		return false
	}
	var deletions []deletion
	for _, change := range changes {
		name := change.Path
		if !change.IsDeleted {
			if change.MovedFrom == "" || keep[change.MovedFrom] {
				continue
			}
			name = change.MovedFrom
		}
		mount, rel, err := mounts.locateEntry(name)
		if err != nil {
			log.Printf("Warning: skipping deletion: %v", err)
			continue
		}
		deletions = append(deletions, deletion{mount: mount, rel: rel})
	}
	if len(deletions) == 0 {
		return
	}
	sort.Slice(deletions, func(i, j int) bool {
		return strings.Count(deletions[i].rel, string(filepath.Separator)) >
			strings.Count(deletions[j].rel, string(filepath.Separator))
	})
	for _, item := range deletions {
		if containsKept(item) {
			log.Printf("Warning: keeping %s, which holds a file whose move was not applied",
				filepath.Join(item.mount.vmPath, item.rel))
			continue
		}
		if err := item.mount.root.RemoveAll(item.rel); err != nil && !errors.Is(err, fs.ErrNotExist) {
			log.Printf("Warning: failed to delete %s: %v", filepath.Join(item.mount.vmPath, item.rel), err)
		}
	}
	log.Printf("Applied %d deletions", len(deletions))
}

// extractTarInMounts writes the exported workspace tarball into the mounted directories. It
// returns the absolute VM paths it wrote, so callers can tell which changes reached the host.
func extractTarInMounts(ctx context.Context, mounts *hostMounts, tarball io.Reader) (map[string]bool, error) {
	reader := tar.NewReader(tarball)
	written := map[string]bool{}
	var fileCount, skipCount int
	skip := func(format string, args ...any) {
		log.Printf("Warning: "+format, args...)
		skipCount++
	}

	for {
		if err := ctx.Err(); err != nil {
			return written, err
		}
		header, err := reader.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			return written, fmt.Errorf("failed to read tar header: %w", err)
		}
		vmPath := filepath.Clean(tarEntryPath(header.Name))
		mount, rel, err := mounts.locate(vmPath)
		if err != nil {
			skip("skipping tar entry: %v", err)
			continue
		}
		if rel == "." {
			// The mount directory itself already exists on the host.
			continue
		}
		root := mount.root
		// Keep only permission bits: setuid, setgid, and sticky never come from the VM.
		mode := os.FileMode(header.Mode).Perm()

		switch header.Typeflag {
		case tar.TypeDir:
			if err := root.MkdirAll(rel, mode|0o700); err != nil {
				skip("failed to create directory %s: %v", header.Name, err)
				continue
			}
			written[vmPath] = true

		case tar.TypeReg:
			if err := root.MkdirAll(filepath.Dir(rel), 0o755); err != nil {
				skip("failed to create parent directory for %s: %v", header.Name, err)
				continue
			}
			if err := removeIfSymlink(root, rel); err != nil {
				skip("failed to replace symlink %s: %v", header.Name, err)
				continue
			}
			output, err := root.OpenFile(rel, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, mode)
			if err != nil {
				skip("failed to create file %s: %v", header.Name, err)
				continue
			}
			_, copyErr := io.Copy(output, reader)
			closeErr := output.Close()
			if copyErr != nil || closeErr != nil {
				skip("failed to write file %s: %v", header.Name, errors.Join(copyErr, closeErr))
				continue
			}
			if err := root.Chmod(rel, mode); err != nil {
				skip("failed to set mode on %s: %v", header.Name, err)
				continue
			}
			written[vmPath] = true
			fileCount++

		case tar.TypeSymlink:
			target, err := symlinkTarget(mount, rel, header.Linkname)
			if err != nil {
				skip("skipping symlink %s: %v", header.Name, err)
				continue
			}
			if err := root.MkdirAll(filepath.Dir(rel), 0o755); err != nil {
				skip("failed to create parent directory for %s: %v", header.Name, err)
				continue
			}
			if err := root.Remove(rel); err != nil && !errors.Is(err, fs.ErrNotExist) {
				skip("failed to replace %s: %v", header.Name, err)
				continue
			}
			if err := root.Symlink(target, rel); err != nil {
				skip("failed to create symlink %s -> %s: %v", header.Name, target, err)
				continue
			}
			written[vmPath] = true

		case tar.TypeLink:
			sourceMount, source, err := mounts.locateEntry(tarEntryPath(header.Linkname))
			if err != nil || sourceMount != mount {
				skip("skipping hard link %s -> %s: source is outside its mount", header.Name, header.Linkname)
				continue
			}
			if err := root.MkdirAll(filepath.Dir(rel), 0o755); err != nil {
				skip("failed to create parent directory for %s: %v", header.Name, err)
				continue
			}
			if err := root.Remove(rel); err != nil && !errors.Is(err, fs.ErrNotExist) {
				skip("failed to replace %s: %v", header.Name, err)
				continue
			}
			if err := root.Link(source, rel); err != nil {
				skip("failed to create hard link %s -> %s: %v", header.Name, header.Linkname, err)
				continue
			}
			written[vmPath] = true

		default:
			skip("skipping unsupported tar entry type %c: %s", header.Typeflag, header.Name)
		}
	}

	if skipCount > 0 {
		log.Printf("Extracted %d files (skipped %d entries)", fileCount, skipCount)
	} else {
		log.Printf("Successfully extracted %d files", fileCount)
	}
	return written, nil
}

// keepUnwrittenMoveSources adds to keep the sources of moves that also changed content but
// whose new copy did not reach the host. Deleting those sources would lose the file.
func keepUnwrittenMoveSources(changes []types.FileDiff, written, keep map[string]bool) {
	for _, change := range changes {
		if change.MovedFrom == "" || !change.IsUpdated || change.IsDeleted {
			continue
		}
		if !written[filepath.Clean(change.Path)] {
			log.Printf("Warning: keeping %s, because its moved copy %s was not written", change.MovedFrom, change.Path)
			keep[change.MovedFrom] = true
		}
	}
}
