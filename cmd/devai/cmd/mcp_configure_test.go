package cmd

import (
	"path/filepath"
	"testing"
)

func TestParseEnvPairs(t *testing.T) {
	got, err := parseEnvPairs([]string{"DEVAI_EMBEDDING_MODEL=ml-mpnet", "X=a=b"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if got["DEVAI_EMBEDDING_MODEL"] != "ml-mpnet" {
		t.Errorf("model = %q, want ml-mpnet", got["DEVAI_EMBEDDING_MODEL"])
	}
	if got["X"] != "a=b" {
		t.Errorf("X = %q, want a=b (only first = splits)", got["X"])
	}
	if _, err := parseEnvPairs([]string{"NOEQUALS"}); err == nil {
		t.Error("expected error for pair without '='")
	}
	if _, err := parseEnvPairs([]string{"=value"}); err == nil {
		t.Error("expected error for empty key")
	}
}

// filepath is used by TestResolveClaudeTarget (added in Task 3)
var _ = filepath.Join
