package cmd

import (
	"database/sql"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/snaven10/devai/internal/config"
	"github.com/snaven10/devai/internal/mlclient"
	"github.com/spf13/cobra"

	// SQLite driver — same driver used by the API server and session store.
	_ "modernc.org/sqlite"
)

// modelDims mirrors the dimension table from ml/devai_ml/embeddings/local.py.
// The "local" provider's known model keys and their output dimensions are
// recorded here so the dimension check can run in pure Go, before the
// Python sidecar is started.
//
// Keep in sync with MODEL_REGISTRY in ml/devai_ml/embeddings/local.py.
var modelDims = map[string]int{
	"minilm-l6":      384,
	"minilm-l12":     384,
	"bge-small":      384,
	"bge-base":       768,
	"ml-minilm":      384,
	"ml-mpnet":       768,
	"ml-granite":     384,
	"ml-granite-lg":  768,
}

// resolveStateDir resolves the absolute path to the state directory using the
// same priority order as the Python ML server and runServerAPI:
//
//  1. DEVAI_STATE_DIR env var
//  2. project config state_dir field
//  3. XDG default (~/.local/share/devai/state)
func resolveStateDir(cfg *config.ProjectConfig) string {
	if v := os.Getenv("DEVAI_STATE_DIR"); v != "" {
		return v
	}
	if cfg != nil && cfg.StateDir != "" {
		return cfg.StateDir
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".local", "share", "devai", "state")
}

// checkDimensionMismatch reads the recorded model_dimension from index_state
// (inside <stateDir>/index.db) and returns an error if it differs from the
// dimension expected by the configured model.
//
// The function is a no-op (returns nil) when:
//   - the store does not yet exist (first index), or
//   - the configured model is unknown / uses a non-local provider (custom/openai/voyage),
//   - no rows exist in index_state yet.
func checkDimensionMismatch(cfg *config.ProjectConfig) error {
	if cfg == nil {
		return nil
	}

	// Only guard "local" provider (or empty = default local).
	provider := cfg.Embeddings.Provider
	if provider != "" && provider != "local" {
		return nil
	}

	modelKey := cfg.Embeddings.Model
	if modelKey == "" {
		modelKey = "minilm-l6" // default, matches Python fallback
	}

	expectedDim, known := modelDims[modelKey]
	if !known {
		// Unknown key — could be a custom model; skip the guard rather than
		// blocking a valid use case.
		return nil
	}

	stateDir := resolveStateDir(cfg)
	dbPath := filepath.Join(stateDir, "index.db")

	if _, err := os.Stat(dbPath); os.IsNotExist(err) {
		// No store yet — first index, nothing to compare.
		return nil
	}

	db, err := sql.Open("sqlite", dbPath+"?mode=ro")
	if err != nil {
		// Can't open: non-fatal, let indexing surface the real error.
		return nil
	}
	defer db.Close()

	var storedDim int
	var storedModel string
	err = db.QueryRow(
		`SELECT model_dimension, model_name FROM index_state
		 WHERE model_dimension > 0
		 ORDER BY indexed_at DESC LIMIT 1`,
	).Scan(&storedDim, &storedModel)
	if err == sql.ErrNoRows {
		// Table exists but is empty — first effective index.
		return nil
	}
	if err != nil {
		// Query error (e.g. table not yet created) — skip guard.
		return nil
	}

	if storedDim != expectedDim {
		return fmt.Errorf(
			"store was built with %d dims (model %q); config requests %d dims (model %q).\n"+
				"To fix: reindex from scratch with `devai index --incremental=false` after confirming\n"+
				"that the model in .devai/config.yaml is correct, or set the matching model.\n"+
				"See docs/12-config-reference.md for details.",
			storedDim, storedModel, expectedDim, modelKey,
		)
	}
	return nil
}

// resolvedClientOpts loads project config and returns mlclient options
// with storage env vars, project config, and state dir resolved.
func resolvedClientOpts() ([]mlclient.Option, error) {
	projectCfg, storageEnv, err := resolvedStorageConfig()
	if err != nil {
		return nil, err
	}
	opts := []mlclient.Option{
		mlclient.WithEnv(storageEnv),
		mlclient.WithConfig(projectCfg),
	}
	return opts, nil
}

var indexCmd = &cobra.Command{
	Use:   "index",
	Short: "Index the current repository",
	Long:  `Index the current repository using git-aware incremental indexing.`,
	RunE:  runIndex,
}

func init() {
	indexCmd.Flags().Bool("incremental", true, "Only index changed files since last index")
	indexCmd.Flags().String("branch", "", "Branch to index (default: current)")
	rootCmd.AddCommand(indexCmd)
}

func runIndex(cmd *cobra.Command, args []string) error {
	incremental, _ := cmd.Flags().GetBool("incremental")
	branch, _ := cmd.Flags().GetString("branch")

	projectCfg, storageEnv, err := resolvedStorageConfig()
	if err != nil {
		return fmt.Errorf("resolving config: %w", err)
	}

	// Guard: abort before starting the Python sidecar if the configured
	// model's dimension differs from what the existing store was built with.
	// Silent dimension mismatches corrupt the LanceDB vector table.
	if err := checkDimensionMismatch(projectCfg); err != nil {
		return err
	}

	opts := []mlclient.Option{
		mlclient.WithEnv(storageEnv),
		mlclient.WithConfig(projectCfg),
	}
	client, err := mlclient.NewStdioClient(opts...)
	if err != nil {
		return fmt.Errorf("connecting to ML service: %w", err)
	}
	defer client.Close()

	params := map[string]interface{}{
		"repo_path":   ".",
		"incremental": incremental,
	}
	if branch != "" {
		params["branch"] = branch
	}

	result, err := client.Call("index_repo", params)
	if err != nil {
		return fmt.Errorf("indexing failed: %w", err)
	}

	// Pretty print result
	formatted, _ := json.MarshalIndent(result, "", "  ")
	fmt.Println(string(formatted))
	return nil
}
