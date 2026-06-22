package cmd

import (
	"database/sql"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/snaven10/devai/internal/config"

	_ "modernc.org/sqlite"
)

// makeIndexDB creates a temporary index.db with one index_state row.
func makeIndexDB(t *testing.T, modelName string, dimension int) string {
	t.Helper()
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "index.db")

	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("creating test db: %v", err)
	}
	defer db.Close()

	_, err = db.Exec(`CREATE TABLE index_state (
		repo_path TEXT NOT NULL,
		branch TEXT NOT NULL,
		last_commit TEXT NOT NULL,
		model_name TEXT NOT NULL,
		model_dimension INTEGER NOT NULL,
		file_count INTEGER DEFAULT 0,
		symbol_count INTEGER DEFAULT 0,
		chunk_count INTEGER DEFAULT 0,
		indexed_at TEXT NOT NULL,
		PRIMARY KEY (repo_path, branch)
	)`)
	if err != nil {
		t.Fatalf("creating table: %v", err)
	}

	_, err = db.Exec(
		`INSERT INTO index_state VALUES (?, ?, ?, ?, ?, 0, 0, 0, datetime('now'))`,
		"/repo/foo", "main", "abc123", modelName, dimension,
	)
	if err != nil {
		t.Fatalf("inserting row: %v", err)
	}

	return dir // state dir, not the db file
}

func cfgWithModel(model string) *config.ProjectConfig {
	cfg := &config.ProjectConfig{}
	cfg.Embeddings.Provider = "local"
	cfg.Embeddings.Model = model
	return cfg
}

// TestCheckDimensionMismatch_Mismatch verifies that indexing is aborted when
// the store was built with a different dimension than the configured model.
func TestCheckDimensionMismatch_Mismatch(t *testing.T) {
	// Store built with ml-mpnet (768 dims), config now requests ml-granite (384 dims).
	stateDir := makeIndexDB(t, "ml-mpnet", 768)

	cfg := cfgWithModel("ml-granite") // 384 dims
	cfg.StateDir = stateDir

	err := checkDimensionMismatch(cfg)
	if err == nil {
		t.Fatal("expected error on dimension mismatch, got nil")
	}
	if !strings.Contains(err.Error(), "768") || !strings.Contains(err.Error(), "384") {
		t.Errorf("error message should mention both stored (768) and expected (384) dims; got: %v", err)
	}
	if !strings.Contains(err.Error(), "ml-mpnet") || !strings.Contains(err.Error(), "ml-granite") {
		t.Errorf("error message should mention both stored and configured model names; got: %v", err)
	}
}

// TestCheckDimensionMismatch_Match verifies that no error is returned when
// the stored dimension matches the configured model.
func TestCheckDimensionMismatch_Match(t *testing.T) {
	// Store built with ml-granite (384 dims), config also requests ml-granite.
	stateDir := makeIndexDB(t, "ml-granite", 384)

	cfg := cfgWithModel("ml-granite")
	cfg.StateDir = stateDir

	if err := checkDimensionMismatch(cfg); err != nil {
		t.Fatalf("unexpected error on matching dims: %v", err)
	}
}

// TestCheckDimensionMismatch_NoStore verifies that a missing index.db is
// treated as a first index (no error).
func TestCheckDimensionMismatch_NoStore(t *testing.T) {
	cfg := cfgWithModel("ml-granite")
	cfg.StateDir = t.TempDir() // empty dir — no index.db

	if err := checkDimensionMismatch(cfg); err != nil {
		t.Fatalf("expected nil for missing store, got: %v", err)
	}
}

// TestCheckDimensionMismatch_EmptyStore verifies that a store with no rows
// is treated as a first effective index (no error).
func TestCheckDimensionMismatch_EmptyStore(t *testing.T) {
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "index.db")

	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("creating test db: %v", err)
	}
	db.Exec(`CREATE TABLE index_state (
		repo_path TEXT, branch TEXT, last_commit TEXT, model_name TEXT,
		model_dimension INTEGER, file_count INTEGER, symbol_count INTEGER,
		chunk_count INTEGER, indexed_at TEXT, PRIMARY KEY (repo_path, branch))`)
	db.Close()

	cfg := cfgWithModel("ml-granite")
	cfg.StateDir = dir

	if err := checkDimensionMismatch(cfg); err != nil {
		t.Fatalf("expected nil for empty store, got: %v", err)
	}
}

// TestCheckDimensionMismatch_UnknownModel verifies that an unknown model key
// is not blocked (skip guard for custom/future models).
func TestCheckDimensionMismatch_UnknownModel(t *testing.T) {
	// Store has 768-dim vectors, but configured model is unknown.
	stateDir := makeIndexDB(t, "some-old-model", 768)

	cfg := cfgWithModel("my-custom-model-xyz")
	cfg.StateDir = stateDir

	if err := checkDimensionMismatch(cfg); err != nil {
		t.Fatalf("expected nil for unknown model key, got: %v", err)
	}
}

// TestCheckDimensionMismatch_NonLocalProvider verifies that non-local providers
// (openai, voyage) are not blocked even if there is a stored store.
func TestCheckDimensionMismatch_NonLocalProvider(t *testing.T) {
	stateDir := makeIndexDB(t, "text-embedding-3-small", 1536)

	cfg := &config.ProjectConfig{}
	cfg.Embeddings.Provider = "openai"
	cfg.Embeddings.Model = "text-embedding-3-small"
	cfg.StateDir = stateDir

	if err := checkDimensionMismatch(cfg); err != nil {
		t.Fatalf("expected nil for non-local provider, got: %v", err)
	}
}

// TestCheckDimensionMismatch_DefaultModel verifies that an empty model name
// defaults to minilm-l6 (384 dims) and is correctly compared.
func TestCheckDimensionMismatch_DefaultModel(t *testing.T) {
	// Store built with minilm-l6 (384 dims).
	stateDir := makeIndexDB(t, "minilm-l6", 384)

	cfg := cfgWithModel("") // empty = default minilm-l6
	cfg.StateDir = stateDir

	if err := checkDimensionMismatch(cfg); err != nil {
		t.Fatalf("expected nil for default model match, got: %v", err)
	}
}

// TestCheckDimensionMismatch_EnvOverride verifies that DEVAI_STATE_DIR env
// var overrides the config state_dir for the purpose of locating index.db.
func TestCheckDimensionMismatch_EnvOverride(t *testing.T) {
	// Build the store in a temp dir that we'll expose via env.
	stateDir := makeIndexDB(t, "ml-mpnet", 768)
	t.Setenv("DEVAI_STATE_DIR", stateDir)

	cfg := cfgWithModel("ml-granite") // 384 — mismatch
	// cfg.StateDir intentionally left empty; env var should be used.

	err := checkDimensionMismatch(cfg)
	if err == nil {
		t.Fatal("expected mismatch error via DEVAI_STATE_DIR override, got nil")
	}
	if !strings.Contains(err.Error(), "768") {
		t.Errorf("expected 768 in error message, got: %v", err)
	}
}

// TestResolveStateDir checks all three priority levels.
func TestResolveStateDir(t *testing.T) {
	t.Run("env_var_wins", func(t *testing.T) {
		t.Setenv("DEVAI_STATE_DIR", "/env/state")
		cfg := &config.ProjectConfig{}
		cfg.StateDir = "/cfg/state"
		got := resolveStateDir(cfg)
		if got != "/env/state" {
			t.Errorf("expected /env/state, got %s", got)
		}
	})

	t.Run("config_wins_when_no_env", func(t *testing.T) {
		os.Unsetenv("DEVAI_STATE_DIR")
		cfg := &config.ProjectConfig{}
		cfg.StateDir = "/cfg/state"
		got := resolveStateDir(cfg)
		if got != "/cfg/state" {
			t.Errorf("expected /cfg/state, got %s", got)
		}
	})

	t.Run("xdg_default", func(t *testing.T) {
		os.Unsetenv("DEVAI_STATE_DIR")
		got := resolveStateDir(nil)
		if !strings.HasSuffix(got, "/devai/state") {
			t.Errorf("expected XDG default ending in /devai/state, got %s", got)
		}
	})
}
