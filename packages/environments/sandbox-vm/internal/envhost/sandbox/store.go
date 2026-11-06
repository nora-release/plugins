package sandbox

import (
	"encoding/json"
	"log"
	"os"
	"path/filepath"
	"sync"
)

type environmentStore struct {
	path    string
	mu      sync.RWMutex
	records map[string]string
}

type environmentStoreFile struct {
	Records map[string]string `json:"records"`
}

func newEnvironmentStore(path string) *environmentStore {
	store := &environmentStore{
		path:    path,
		records: make(map[string]string),
	}
	if err := store.load(); err != nil {
		log.Printf("sandbox environment store: failed to load %s: %v", path, err)
	}
	return store
}

func (s *environmentStore) Put(envID, backendID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.records[envID] = backendID
	return s.saveLocked()
}

func (s *environmentStore) Get(envID string) (string, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	backendID, ok := s.records[envID]
	return backendID, ok
}

func (s *environmentStore) Delete(envID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.records, envID)
	return s.saveLocked()
}

func (s *environmentStore) Single() (envID string, backendID string, ok bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	if len(s.records) != 1 {
		return "", "", false
	}
	for envID, backendID := range s.records {
		return envID, backendID, true
	}
	return "", "", false
}

func (s *environmentStore) Records() map[string]string {
	s.mu.RLock()
	defer s.mu.RUnlock()
	records := make(map[string]string, len(s.records))
	for envID, backendID := range s.records {
		records[envID] = backendID
	}
	return records
}

func (s *environmentStore) ReplaceSingle(envID, backendID string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.records = map[string]string{
		envID: backendID,
	}
	return s.saveLocked()
}

func (s *environmentStore) load() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.loadLocked()
}

func (s *environmentStore) loadLocked() error {
	if s.path == "" {
		return nil
	}
	data, err := os.ReadFile(s.path)
	if err != nil {
		if os.IsNotExist(err) {
			s.records = make(map[string]string)
			return nil
		}
		return err
	}

	var file environmentStoreFile
	if err := json.Unmarshal(data, &file); err != nil {
		return err
	}
	if file.Records == nil {
		file.Records = make(map[string]string)
	}
	s.records = file.Records
	return nil
}

func (s *environmentStore) saveLocked() error {
	if s.path == "" {
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(s.path), 0o755); err != nil {
		return err
	}
	data, err := json.MarshalIndent(environmentStoreFile{Records: s.records}, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(s.path, data, 0o644)
}
