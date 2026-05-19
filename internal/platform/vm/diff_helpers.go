package vm

import (
	"archive/tar"
	"bytes"
	"context"
	"fmt"
	"io"
	"log"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/nora/sandbox-vm/pkg/types"
)

// tarBuffer is a simple in-memory tar buffer.
type tarBuffer struct {
	bytes.Buffer
}

// applyMovesOnHost executes pure move operations (rename without content change) on the host filesystem.
func applyMovesOnHost(hostBaseDir string, changes []types.FileDiff) error {
	var moveCount int
	for _, change := range changes {
		if change.MovedFrom == "" || change.IsUpdated || change.IsDeleted {
			continue
		}

		srcPath := filepath.Join(hostBaseDir, change.MovedFrom)
		dstPath := filepath.Join(hostBaseDir, change.Path)

		if _, err := os.Stat(srcPath); os.IsNotExist(err) {
			log.Printf("Move source %s not found on host, skipping rename", change.MovedFrom)
			continue
		}

		dstDir := filepath.Dir(dstPath)
		if err := os.MkdirAll(dstDir, 0755); err != nil {
			log.Printf("Warning: failed to create directory %s: %v", dstDir, err)
			continue
		}

		if err := os.Rename(srcPath, dstPath); err != nil {
			log.Printf("Warning: failed to move %s -> %s: %v", change.MovedFrom, change.Path, err)
		} else {
			moveCount++
		}
	}

	if moveCount > 0 {
		log.Printf("Applied %d moves", moveCount)
	}
	return nil
}

// applyDeletionsOnHost executes deletion operations from the diff on the host filesystem.
func applyDeletionsOnHost(hostBaseDir string, changes []types.FileDiff) error {
	var deletions []string
	for _, change := range changes {
		if change.IsDeleted {
			deletions = append(deletions, change.Path)
		} else if change.MovedFrom != "" {
			deletions = append(deletions, change.MovedFrom)
		}
	}

	if len(deletions) == 0 {
		return nil
	}

	sort.Slice(deletions, func(i, j int) bool {
		depthI := strings.Count(deletions[i], "/")
		depthJ := strings.Count(deletions[j], "/")
		return depthI > depthJ
	})

	for _, del := range deletions {
		hostPath := filepath.Join(hostBaseDir, del)
		if err := os.RemoveAll(hostPath); err != nil && !os.IsNotExist(err) {
			log.Printf("Warning: failed to delete %s: %v", del, err)
		}
	}

	log.Printf("Applied %d deletions", len(deletions))
	return nil
}

// extractTarToHost extracts a tar archive to the target host directory.
func extractTarToHost(ctx context.Context, tarball io.Reader, targetHostDir string) error {
	if err := os.MkdirAll(targetHostDir, 0755); err != nil {
		return fmt.Errorf("create target directory %s: %w", targetHostDir, err)
	}

	tr := tar.NewReader(tarball)
	var fileCount, skipCount int

	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		default:
		}

		header, err := tr.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			return fmt.Errorf("read tar header: %w", err)
		}

		cleanPath := filepath.Clean(header.Name)
		if strings.HasPrefix(cleanPath, "..") || filepath.IsAbs(cleanPath) {
			log.Printf("Warning: skipping potentially unsafe path: %s", header.Name)
			skipCount++
			continue
		}

		targetPath := filepath.Join(targetHostDir, cleanPath)

		switch header.Typeflag {
		case tar.TypeDir:
			if err := os.MkdirAll(targetPath, os.FileMode(header.Mode)); err != nil {
				log.Printf("Warning: failed to create directory %s: %v", targetPath, err)
				skipCount++
			}

		case tar.TypeReg:
			parentDir := filepath.Dir(targetPath)
			if err := os.MkdirAll(parentDir, 0755); err != nil {
				log.Printf("Warning: failed to create parent directory %s: %v", parentDir, err)
				skipCount++
				continue
			}
			outFile, err := os.OpenFile(targetPath, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, os.FileMode(header.Mode))
			if err != nil {
				log.Printf("Warning: failed to create file %s: %v", targetPath, err)
				skipCount++
				continue
			}
			if _, err := io.Copy(outFile, tr); err != nil {
				outFile.Close()
				log.Printf("Warning: failed to write file %s: %v", targetPath, err)
				skipCount++
				continue
			}
			outFile.Close()
			fileCount++

		case tar.TypeSymlink:
			os.Remove(targetPath)
			parentDir := filepath.Dir(targetPath)
			if err := os.MkdirAll(parentDir, 0755); err != nil {
				log.Printf("Warning: failed to create parent directory %s: %v", parentDir, err)
				skipCount++
				continue
			}
			if err := os.Symlink(header.Linkname, targetPath); err != nil {
				log.Printf("Warning: failed to create symlink %s -> %s: %v", targetPath, header.Linkname, err)
				skipCount++
			}

		case tar.TypeLink:
			linkTarget := filepath.Join(targetHostDir, header.Linkname)
			parentDir := filepath.Dir(targetPath)
			if err := os.MkdirAll(parentDir, 0755); err != nil {
				log.Printf("Warning: failed to create parent directory %s: %v", parentDir, err)
				skipCount++
				continue
			}
			os.Remove(targetPath)
			if err := os.Link(linkTarget, targetPath); err != nil {
				log.Printf("Warning: failed to create hard link %s -> %s: %v", targetPath, linkTarget, err)
				skipCount++
			}

		default:
			log.Printf("Skipping unsupported tar entry type %c: %s", header.Typeflag, header.Name)
			skipCount++
		}
	}

	if skipCount > 0 {
		log.Printf("Extracted %d files to %s (skipped %d entries)", fileCount, targetHostDir, skipCount)
	} else {
		log.Printf("Successfully extracted %d files to: %s", fileCount, targetHostDir)
	}
	return nil
}
