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
// and mode it reports is untrusted too. These helpers apply accepted VM changes through an
// os.Root opened at the host workspace: ".." and symlinks cannot reach outside it, and the
// checks below reject the remaining inputs that would still be unsafe inside it.

// openHostRoot opens the host workspace that accepted changes are confined to.
func openHostRoot(hostBaseDir string) (*os.Root, error) {
	if err := os.MkdirAll(hostBaseDir, 0o755); err != nil {
		return nil, fmt.Errorf("create target directory %s: %w", hostBaseDir, err)
	}
	root, err := os.OpenRoot(hostBaseDir)
	if err != nil {
		return nil, fmt.Errorf("open target directory %s: %w", hostBaseDir, err)
	}
	return root, nil
}

// workspaceRelativePath returns name as a cleaned path strictly inside the workspace.
// It rejects absolute paths, "..", and the workspace root itself.
func workspaceRelativePath(name string) (string, error) {
	clean := filepath.Clean(filepath.FromSlash(strings.TrimSpace(name)))
	if clean == "." || filepath.IsAbs(clean) || clean == ".." ||
		strings.HasPrefix(clean, ".."+string(filepath.Separator)) {
		return "", fmt.Errorf("path %q is outside the workspace", name)
	}
	return clean, nil
}

// symlinkTargetInsideWorkspace reports whether a link at linkPath pointing to target
// resolves inside the workspace without leaving it.
func symlinkTargetInsideWorkspace(linkPath, target string) bool {
	if target == "" || filepath.IsAbs(target) {
		return false
	}
	resolved := filepath.Join(filepath.Dir(linkPath), filepath.FromSlash(target))
	_, err := workspaceRelativePath(resolved)
	return err == nil || resolved == "."
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

// applyMovesInRoot performs pure renames (MovedFrom set, content unchanged).
func applyMovesInRoot(root *os.Root, changes []types.FileDiff) {
	var moveCount int
	for _, change := range changes {
		if change.MovedFrom == "" || change.IsUpdated || change.IsDeleted {
			continue
		}
		source, err := workspaceRelativePath(change.MovedFrom)
		if err != nil {
			log.Printf("Warning: skipping move: %v", err)
			continue
		}
		destination, err := workspaceRelativePath(change.Path)
		if err != nil {
			log.Printf("Warning: skipping move: %v", err)
			continue
		}
		if _, err := root.Lstat(source); errors.Is(err, fs.ErrNotExist) {
			// Created and moved inside the VM; the tar carries it as a new file.
			log.Printf("Move source %s not found on host, skipping rename", source)
			continue
		}
		if err := root.MkdirAll(filepath.Dir(destination), 0o755); err != nil {
			log.Printf("Warning: failed to create directory for %s: %v", destination, err)
			continue
		}
		if err := root.Rename(source, destination); err != nil {
			log.Printf("Warning: failed to move %s -> %s: %v", source, destination, err)
			continue
		}
		moveCount++
	}
	if moveCount > 0 {
		log.Printf("Applied %d moves", moveCount)
	}
}

// applyDeletionsInRoot removes deleted paths and move sources, deepest paths first.
func applyDeletionsInRoot(root *os.Root, changes []types.FileDiff) {
	var deletions []string
	for _, change := range changes {
		name := change.Path
		if !change.IsDeleted {
			if change.MovedFrom == "" {
				continue
			}
			name = change.MovedFrom
		}
		relative, err := workspaceRelativePath(name)
		if err != nil {
			log.Printf("Warning: skipping deletion: %v", err)
			continue
		}
		deletions = append(deletions, relative)
	}
	if len(deletions) == 0 {
		return
	}
	sort.Slice(deletions, func(i, j int) bool {
		return strings.Count(deletions[i], string(filepath.Separator)) >
			strings.Count(deletions[j], string(filepath.Separator))
	})
	for _, relative := range deletions {
		if err := root.RemoveAll(relative); err != nil && !errors.Is(err, fs.ErrNotExist) {
			log.Printf("Warning: failed to delete %s: %v", relative, err)
		}
	}
	log.Printf("Applied %d deletions", len(deletions))
}

// extractTarInRoot writes the exported workspace tarball into root.
func extractTarInRoot(ctx context.Context, root *os.Root, tarball io.Reader) error {
	reader := tar.NewReader(tarball)
	var fileCount, skipCount int
	skip := func(format string, args ...any) {
		log.Printf("Warning: "+format, args...)
		skipCount++
	}

	for {
		if err := ctx.Err(); err != nil {
			return err
		}
		header, err := reader.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			return fmt.Errorf("failed to read tar header: %w", err)
		}
		relative, err := workspaceRelativePath(header.Name)
		if err != nil {
			skip("skipping tar entry: %v", err)
			continue
		}
		// Keep only permission bits: setuid, setgid, and sticky never come from the VM.
		mode := os.FileMode(header.Mode).Perm()

		switch header.Typeflag {
		case tar.TypeDir:
			if err := root.MkdirAll(relative, mode|0o700); err != nil {
				skip("failed to create directory %s: %v", relative, err)
			}

		case tar.TypeReg:
			if err := root.MkdirAll(filepath.Dir(relative), 0o755); err != nil {
				skip("failed to create parent directory for %s: %v", relative, err)
				continue
			}
			if err := removeIfSymlink(root, relative); err != nil {
				skip("failed to replace symlink %s: %v", relative, err)
				continue
			}
			output, err := root.OpenFile(relative, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, mode)
			if err != nil {
				skip("failed to create file %s: %v", relative, err)
				continue
			}
			_, copyErr := io.Copy(output, reader)
			closeErr := output.Close()
			if copyErr != nil || closeErr != nil {
				skip("failed to write file %s: %v", relative, errors.Join(copyErr, closeErr))
				continue
			}
			if err := root.Chmod(relative, mode); err != nil {
				skip("failed to set mode on %s: %v", relative, err)
				continue
			}
			fileCount++

		case tar.TypeSymlink:
			if !symlinkTargetInsideWorkspace(relative, header.Linkname) {
				skip("skipping symlink %s: target %q leaves the workspace", relative, header.Linkname)
				continue
			}
			if err := root.MkdirAll(filepath.Dir(relative), 0o755); err != nil {
				skip("failed to create parent directory for %s: %v", relative, err)
				continue
			}
			if err := root.Remove(relative); err != nil && !errors.Is(err, fs.ErrNotExist) {
				skip("failed to replace %s: %v", relative, err)
				continue
			}
			if err := root.Symlink(header.Linkname, relative); err != nil {
				skip("failed to create symlink %s -> %s: %v", relative, header.Linkname, err)
			}

		case tar.TypeLink:
			source, err := workspaceRelativePath(header.Linkname)
			if err != nil {
				skip("skipping hard link %s: %v", relative, err)
				continue
			}
			if err := root.MkdirAll(filepath.Dir(relative), 0o755); err != nil {
				skip("failed to create parent directory for %s: %v", relative, err)
				continue
			}
			if err := root.Remove(relative); err != nil && !errors.Is(err, fs.ErrNotExist) {
				skip("failed to replace %s: %v", relative, err)
				continue
			}
			if err := root.Link(source, relative); err != nil {
				skip("failed to create hard link %s -> %s: %v", relative, source, err)
			}

		default:
			skip("skipping unsupported tar entry type %c: %s", header.Typeflag, header.Name)
		}
	}

	if skipCount > 0 {
		log.Printf("Extracted %d files to %s (skipped %d entries)", fileCount, root.Name(), skipCount)
	} else {
		log.Printf("Successfully extracted %d files to: %s", fileCount, root.Name())
	}
	return nil
}
