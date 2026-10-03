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

/// Remove every hook-exported `GIT_*` variable from `cmd`'s environment.
pub fn clean_git_env(cmd: &mut Command) -> &mut Command {
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
}
