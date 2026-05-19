package types

import (
	"time"
)

// FileDiff represents a single file or directory change detected by the overlay analyzer.
// This provides richer change detection including move detection and update detection.
type FileDiff struct {
	Path      string    `json:"path"`
	Mode      uint32    `json:"mode"`
	IsDir     bool      `json:"isDir"`
	IsUpdated bool      `json:"isUpdated"`
	IsDeleted bool      `json:"isDeleted"`
	MovedFrom string    `json:"movedFrom,omitempty"`
	Timestamp time.Time `json:"timestamp"`
	Size      int64     `json:"size"`
}

// SandboxState represents the current state of a sandbox (file modifications).
type SandboxState struct {
	SandboxID        string     `json:"sandboxId"`
	EnvironmentID    string     `json:"environmentId"`
	EnvironmentLabel string     `json:"environmentLabel"`
	FileDiff         []FileDiff `json:"fileDiff"` // Flat list of file changes with richer info
}

// ExportDiffResult represents the result of ExportDiff operation.
// Contains filtered file changes and the paths that were used as filter.
type ExportDiffResult struct {
	Changes []FileDiff `json:"changes"`         // Filtered file changes
	Paths   []string   `json:"paths,omitempty"` // Paths used for filtering (empty if no filter)
}

// AcceptChangesResult holds the outcome of accepting sandbox changes.
type AcceptChangesResult struct {
	AcceptedCount int           `json:"acceptedCount"`
	RejectedCount int           `json:"rejectedCount"`
	State         *SandboxState `json:"state,omitempty"`
	ReviewDiff    []FileDiff    `json:"reviewDiff,omitempty"`
	Summary       string        `json:"summary,omitempty"`
}

// DiscardAllChangesResult holds the outcome of discarding all sandbox changes.
type DiscardAllChangesResult struct {
	TotalFiles int           `json:"totalFiles"`
	State      *SandboxState `json:"state,omitempty"`
	Summary    string        `json:"summary,omitempty"`
}
