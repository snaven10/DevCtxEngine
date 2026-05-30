package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/spf13/cobra"
)

var hooksCmd = &cobra.Command{
	Use:   "hooks",
	Short: "Manage git hooks for auto-indexing",
}

var hooksInstallCmd = &cobra.Command{
	Use:   "install [repo-path]",
	Short: "Install post-commit hook for auto-indexing",
	Long: `Installs (or updates) a git post-commit hook that triggers ` + "`devai index`" + ` after
each commit, in the background. The hook is written as a clearly delimited block so it
can coexist with other post-commit hooks and be updated/removed cleanly.`,
	Args: cobra.MaximumNArgs(1),
	RunE: runHooksInstall,
}

var hooksUninstallCmd = &cobra.Command{
	Use:   "uninstall [repo-path]",
	Short: "Remove devai post-commit hook",
	Args:  cobra.MaximumNArgs(1),
	RunE:  runHooksUninstall,
}

func init() {
	hooksCmd.AddCommand(hooksInstallCmd)
	hooksCmd.AddCommand(hooksUninstallCmd)
	rootCmd.AddCommand(hooksCmd)
}

const (
	// Delimited block markers — let us update/remove only our section without
	// touching other post-commit logic.
	hookBeginMarker = "# >>> DEVAI_AUTO_INDEX >>>"
	hookEndMarker   = "# <<< DEVAI_AUTO_INDEX <<<"
	// Legacy single-line marker from older installs (pre-delimited-block). Kept
	// so `uninstall` and re-`install` can clean up hooks written by old versions.
	legacyHookMarker = "# DEVAI_AUTO_INDEX"
)

// hookBlock builds the delimited block injected into the post-commit hook.
//
// It cd's to the repo top-level first so `devai index` resolves the real repo
// name (it sends repo_path="."), and redirects output + backgrounds the run so
// the commit is never blocked or polluted by indexing logs.
func hookBlock(devaiBinary, stateDir string) string {
	return fmt.Sprintf(`%s
# Auto-index after each commit. Managed by 'devai hooks install/uninstall' — do not edit by hand.
( cd "$(git rev-parse --show-toplevel)" && DEVAI_STATE_DIR=%q %q index --incremental ) >/dev/null 2>&1 &
%s`, hookBeginMarker, stateDir, devaiBinary, hookEndMarker)
}

func runHooksInstall(cmd *cobra.Command, args []string) error {
	repoPath := "."
	if len(args) > 0 {
		repoPath = args[0]
	}

	absPath, err := filepath.Abs(repoPath)
	if err != nil {
		return fmt.Errorf("resolving path: %w", err)
	}

	gitDir := filepath.Join(absPath, ".git")
	if info, err := os.Stat(gitDir); err != nil || !info.IsDir() {
		return fmt.Errorf("%s is not a git repository", absPath)
	}

	devaiBinary, err := os.Executable()
	if err != nil {
		return fmt.Errorf("finding devai binary: %w", err)
	}
	devaiBinary, _ = filepath.Abs(devaiBinary)

	stateDir := os.Getenv("DEVAI_STATE_DIR")
	if stateDir == "" {
		stateDir = filepath.Join(absPath, ".devai", "state")
	}

	hooksDir := filepath.Join(gitDir, "hooks")
	if err := os.MkdirAll(hooksDir, 0o755); err != nil {
		return fmt.Errorf("creating hooks directory: %w", err)
	}
	hookPath := filepath.Join(hooksDir, "post-commit")

	block := hookBlock(devaiBinary, stateDir)

	existing, _ := os.ReadFile(hookPath) // ignore error: missing file => fresh install
	content := string(existing)

	var out, action string
	switch {
	case strings.TrimSpace(content) == "":
		// Fresh hook.
		out = "#!/bin/sh\n\n" + block + "\n"
		action = "installed"
	case strings.Contains(content, hookBeginMarker):
		// Our delimited block is already present → replace it in place,
		// preserving everything else in the file.
		out = replaceHookBlock(content, block)
		action = "updated"
	case strings.Contains(content, legacyHookMarker):
		// Older single-line install → strip the legacy lines, then append the
		// new delimited block.
		out = strings.TrimRight(stripLegacyHook(content), "\n") + "\n\n" + block + "\n"
		action = "migrated"
	default:
		// Foreign post-commit hook with no devai section → append ours.
		out = strings.TrimRight(content, "\n") + "\n\n" + block + "\n"
		action = "appended to existing hook"
	}

	if err := os.WriteFile(hookPath, []byte(out), 0o755); err != nil {
		return fmt.Errorf("writing hook: %w", err)
	}

	fmt.Printf("Post-commit hook %s: %s\n", action, hookPath)
	fmt.Printf("  Binary: %s\n", devaiBinary)
	fmt.Printf("  State:  %s\n", stateDir)
	fmt.Println("\nAuto-indexing will run after each commit (background, output suppressed).")
	return nil
}

func runHooksUninstall(cmd *cobra.Command, args []string) error {
	repoPath := "."
	if len(args) > 0 {
		repoPath = args[0]
	}

	absPath, err := filepath.Abs(repoPath)
	if err != nil {
		return fmt.Errorf("resolving path: %w", err)
	}

	hookPath := filepath.Join(absPath, ".git", "hooks", "post-commit")
	data, err := os.ReadFile(hookPath)
	if err != nil {
		fmt.Println("No post-commit hook found")
		return nil
	}
	content := string(data)

	var stripped string
	switch {
	case strings.Contains(content, hookBeginMarker):
		stripped = removeHookBlock(content)
	case strings.Contains(content, legacyHookMarker):
		stripped = stripLegacyHook(content)
	default:
		fmt.Println("No devai hook found in post-commit")
		return nil
	}

	// If only a shebang / whitespace remains, remove the file entirely.
	if isHookEmpty(stripped) {
		if err := os.Remove(hookPath); err != nil {
			return fmt.Errorf("removing hook: %w", err)
		}
		fmt.Println("Post-commit hook removed")
		return nil
	}

	if err := os.WriteFile(hookPath, []byte(strings.TrimRight(stripped, "\n")+"\n"), 0o755); err != nil {
		return fmt.Errorf("writing hook: %w", err)
	}
	fmt.Println("DevAI section removed from post-commit (other hooks preserved)")
	return nil
}

// replaceHookBlock swaps the BEGIN..END block for a new one, keeping the rest.
func replaceHookBlock(content, block string) string {
	return strings.TrimRight(removeHookBlock(content), "\n") + "\n\n" + block + "\n"
}

// removeHookBlock deletes the BEGIN..END block (inclusive) from content.
func removeHookBlock(content string) string {
	start := strings.Index(content, hookBeginMarker)
	if start < 0 {
		return content
	}
	endIdx := strings.Index(content[start:], hookEndMarker)
	if endIdx < 0 {
		// Malformed (no END marker): drop everything from BEGIN onward.
		return content[:start]
	}
	end := start + endIdx + len(hookEndMarker)
	return content[:start] + content[end:]
}

// stripLegacyHook removes the old single-line install: the legacy marker plus
// the immediately-following devai/index/comment lines.
func stripLegacyHook(content string) string {
	lines := strings.Split(content, "\n")
	var kept []string
	skip := false
	for _, line := range lines {
		if strings.Contains(line, legacyHookMarker) {
			skip = true
			continue
		}
		if skip {
			if strings.Contains(line, "devai") || strings.Contains(line, "DEVAI_STATE_DIR") ||
				strings.Contains(line, "Auto-index") || strings.Contains(line, "Remove with") ||
				strings.TrimSpace(line) == "" {
				continue
			}
			skip = false
		}
		kept = append(kept, line)
	}
	return strings.Join(kept, "\n")
}

// isHookEmpty reports whether only a shebang and/or whitespace remains.
func isHookEmpty(content string) bool {
	for _, line := range strings.Split(content, "\n") {
		t := strings.TrimSpace(line)
		if t == "" || t == "#!/bin/sh" || t == "#!/bin/bash" {
			continue
		}
		return false
	}
	return true
}
