package cmd

import (
	"strings"
	"testing"
)

func TestHookBlock_InjectsModelAndEmbedCap(t *testing.T) {
	block := hookBlock("/bin/devai", "/central/state", "ml-granite", "2048")
	for _, want := range []string{
		`DEVAI_STATE_DIR="/central/state"`,
		`DEVAI_EMBEDDING_MODEL="ml-granite"`,
		`DEVAI_EMBED_MAX_CHARS="2048"`,
		"index --incremental",
		hookBeginMarker,
		hookEndMarker,
	} {
		if !strings.Contains(block, want) {
			t.Fatalf("hook block missing %q; got:\n%s", want, block)
		}
	}
}

func TestHookBlock_OmitsModelWhenUnset(t *testing.T) {
	block := hookBlock("/bin/devai", "/central/state", "", "2048")
	if strings.Contains(block, "DEVAI_EMBEDDING_MODEL=") {
		t.Fatalf("model env must be omitted when empty; got:\n%s", block)
	}
}
