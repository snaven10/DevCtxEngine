//! A clean `GIT_*` environment for everything devctx spawns.
//!
//! Git exports `GIT_DIR`, `GIT_INDEX_FILE`, `GIT_PREFIX`… to the hooks it runs,
//! pointing at the repository (or linked worktree) that is committing. A
//! `devctx index` launched from `post-commit` auto-spawns a long-lived `serve`
//! that would inherit them; once that worktree is deleted every `git rev-parse`
//! in the server fails. So neither the server nor any `git` devctx runs may
//! see them: each repository is located from the directory it is given, never
//! from the environment.

use std::process::Command;

/// Variables that make git pick a repository, index or object store other than
/// the one in the working directory. Also unset by the managed hook block.
pub const GIT_REPO_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_PREFIX",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_NAMESPACE",
    "GIT_QUARANTINE_PATH",
    "GIT_CEILING_DIRECTORIES",
];

/// Identity variables git sets for `commit` hooks. Harmless for reading, but a
/// server lives long and has no business carrying someone's commit identity.
pub const GIT_IDENTITY_ENV: &[&str] = &[
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_AUTHOR_DATE",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "GIT_COMMITTER_DATE",
];

/// `GIT_*` variables devctx subprocesses keep: they say how to reach a remote
/// or which git to run, never which repository to act on. Everything else is
/// removed (an allowlist, because git adds hook variables over time and
/// `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>` come in unbounded numbers).
const GIT_KEEP_EXACT: &[&str] = &[
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_SSH_VARIANT",
    "GIT_TERMINAL_PROMPT",
    "GIT_ASKPASS",
    "GIT_EXEC_PATH",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_NOSYSTEM",
];

/// Whether a `GIT_*` variable survives [`clean_git_env`]: the allowlist above
/// and the `GIT_TRACE*` debugging switches.
fn keeps(var: &str) -> bool {
    GIT_KEEP_EXACT.contains(&var) || var.starts_with("GIT_TRACE")
}

/// The variables of `vars` that [`clean_git_env`] removes.
pub fn git_vars_to_strip<'a>(vars: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    vars.into_iter()
        .filter(|v| v.starts_with("GIT_") && !keeps(v))
        .collect()
}

/// Remove every `GIT_*` variable from `cmd`'s environment except the allowlist.
///
/// **Behaviour note:** an intentional `GIT_DIR`/`GIT_WORK_TREE` setup (a
/// dotfiles repository driven with `--git-dir`) is no longer honoured by the
/// `git` and `devctx` processes devctx spawns: each repository is located from
/// the directory it is given.
pub fn clean_git_env(cmd: &mut Command) -> &mut Command {
    let ambient: Vec<String> = std::env::vars().map(|(k, _)| k).collect();
    for var in git_vars_to_strip(ambient.iter().map(String::as_str)) {
        cmd.env_remove(var);
    }
    // Also what is a hook-exported name even when this process does not have it
    // set (a caller may have `env()`-ed it on `cmd` itself).
    for var in GIT_REPO_ENV.iter().chain(GIT_IDENTITY_ENV) {
        cmd.env_remove(var);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)]
    fn cleaned_vars_do_not_reach_the_child() {
        let mut cmd = Command::new("env");
        for var in GIT_REPO_ENV.iter().chain(GIT_IDENTITY_ENV) {
            cmd.env(var, "/nonexistent");
        }
        cmd.env("DEVCTX_KEEP_ME", "1");
        clean_git_env(&mut cmd);
        let out = cmd.output().expect("running env");
        let text = String::from_utf8_lossy(&out.stdout);
        // Only ours: the ambient environment may carry unrelated `GIT_*`.
        for var in GIT_REPO_ENV.iter().chain(GIT_IDENTITY_ENV) {
            assert!(
                !text.lines().any(|l| l.starts_with(&format!("{var}="))),
                "{var} leaked: {text}"
            );
        }
        assert!(text.contains("DEVCTX_KEEP_ME=1"));
    }

    /// M-7: what `git -c` and quarantined pushes export to hooks, which a fixed
    /// list missed, is stripped; transport settings stay.
    #[test]
    fn a_hook_like_environment_is_stripped_but_transport_settings_stay() {
        let hook_env = [
            "GIT_DIR",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_NAMESPACE",
            "GIT_QUARANTINE_PATH",
            "GIT_CEILING_DIRECTORIES",
            "GIT_FUTURE_THING",
        ];
        let keep = [
            "GIT_SSH_COMMAND",
            "GIT_ASKPASS",
            "GIT_TRACE",
            "GIT_TRACE_PACK",
            "PATH",
            "HOME",
        ];
        let all: Vec<&str> = hook_env.iter().chain(keep.iter()).copied().collect();
        let stripped = git_vars_to_strip(all);
        for v in hook_env {
            assert!(stripped.contains(&v), "{v} should be stripped");
        }
        for v in keep {
            assert!(!stripped.contains(&v), "{v} should stay");
        }
    }
}
