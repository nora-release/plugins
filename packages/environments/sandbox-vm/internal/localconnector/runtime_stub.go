//go:build !darwin

package localconnector

import "fmt"

// New creates one long-lived overlay-backed sandbox rooted at cfg.RootPath.
func New(cfg Config) (*Runtime, error) {
	if _, err := normalizeConfig(cfg); err != nil {
		return nil, err
	}
	return nil, fmt.Errorf("local connector runtime requires darwin")
}

// NewShared creates one shared VM-backed host and provisions one environment per session.
func NewShared(cfg Config) (*SharedRuntime, error) {
	if _, err := normalizeConfig(cfg); err != nil {
		return nil, err
	}
	return nil, fmt.Errorf("local connector shared runtime requires darwin")
}
