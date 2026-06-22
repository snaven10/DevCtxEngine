package cmd

import "strings"
import "testing"

func TestInitConfigTemplate_OmitsStateDir(t *testing.T) {
	cfg := renderInitConfig("MyProj", "/home/u/repo")
	if strings.Contains(cfg, "state_dir:") {
		t.Fatalf("generated config must not pin a per-repo state_dir; got:\n%s", cfg)
	}
	if !strings.Contains(cfg, "name: MyProj") {
		t.Fatalf("config should contain project name; got:\n%s", cfg)
	}
	if !strings.Contains(cfg, "path: /home/u/repo") {
		t.Fatalf("config should contain project path; got:\n%s", cfg)
	}
	if !strings.Contains(cfg, "model: minilm-l6") {
		t.Fatalf("config should keep default model; got:\n%s", cfg)
	}
}
