//! The questions `devctx init` asks, and the summary it shows before writing.
//!
//! Two of `init`'s decisions are expensive to undo and were previously made in
//! silence: the embedding model, which fixes the width of every vector and
//! cannot change without re-indexing everything, and the group, which decides
//! which repositories can recall what.
//!
//! Every question defaults to what the machine already does, so pressing Enter
//! through all of them reproduces the previous behaviour exactly. The whole
//! thing is skipped without a terminal — an agent following the setup guide
//! runs `init` with no TTY, and a prompt it cannot answer would hang the setup
//! it was told to perform.

use std::io::{IsTerminal as _, Write as _};

use anyhow::Result;
use devctx_core::config::Embeddings;

use crate::models;

/// What the wizard collected. `None` means "leave the default alone".
#[derive(Debug, Default, Clone)]
pub struct Answers {
    pub model: Option<String>,
    pub state_dir: Option<String>,
    pub group: Option<String>,
}

/// One line offering the groups that already exist.
pub fn groups_line(groups: &[(String, usize)]) -> String {
    if groups.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = groups.iter().map(|(g, n)| format!("{g} ({n})")).collect();
    format!("Groups on this machine: {}", parts.join(", "))
}

/// Read one line, returning the trimmed answer or `None` when it was empty.
fn ask_line(question: &str, default: &str) -> Option<String> {
    print!("{question} [{default}]: ");
    std::io::stdout().flush().ok();
    let mut s = String::new();
    if std::io::stdin().read_line(&mut s).is_err() {
        return None;
    }
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Ask everything — or nothing, when there is no terminal.
pub fn ask(
    defaults: &Embeddings,
    in_use: &[(String, usize)],
    groups: &[(String, usize)],
) -> Result<Answers> {
    if !std::io::stdin().is_terminal() {
        return Ok(Answers::default());
    }
    let model = models::prompt(&defaults.model, in_use)?;

    println!(
        "\nThe index is a build artefact — large, binary, rebuilt from the \
         repository — so it lives inside it by default, and is git-ignored."
    );
    let state_dir =
        ask_line("Index directory (blank = inside the repository)", "repo").filter(|s| s != "repo");

    println!("\nMemories can be shared between the repositories of one product.");
    let line = groups_line(groups);
    if !line.is_empty() {
        println!("{line}");
    }
    let group = ask_line("Group for this repository", "none").filter(|g| g != "none");

    Ok(Answers {
        model,
        state_dir,
        group,
    })
}

/// What will be written, in the terms the decisions were made in.
///
/// The last line is the only place the three memory tiers are spelled out at
/// the moment someone is choosing between them.
pub fn summary(name: &str, a: &Answers, model: &str) -> String {
    let group = match &a.group {
        Some(g) => format!("{g}  → memories shared with that product's repositories"),
        None => "none".to_string(),
    };
    let index = match &a.state_dir {
        Some(d) => d.clone(),
        None => "./.devctx/state/index.duckdb  (HNSW on)".to_string(),
    };
    let tiers = match &a.group {
        Some(g) => format!(
            "local → this repository · group ({g}) → central store · global → central store"
        ),
        None => "local → this repository · global → central store".to_string(),
    };
    format!(
        "  project   {name}\n  group     {group}\n  model     {model}\n  index     {index}\n  memories  {tiers}"
    )
}

/// Ask for confirmation.
///
/// `true` without a terminal: there is nobody to ask, and the caller asked for
/// this by running the command.
pub fn confirm() -> bool {
    if !std::io::stdin().is_terminal() {
        return true;
    }
    print!("\nWrite this? [Y/n]: ");
    std::io::stdout().flush().ok();
    let mut s = String::new();
    if std::io::stdin().read_line(&mut s).is_err() {
        return true;
    }
    !matches!(s.trim().to_ascii_lowercase().as_str(), "n" | "no")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The summary is the only place the three memory tiers are explained at
    /// the moment someone is choosing between them.
    #[test]
    fn the_summary_explains_where_each_kind_of_memory_will_live() {
        let a = Answers {
            model: Some("ml-granite".into()),
            state_dir: None,
            group: Some("REVFA".into()),
        };
        let s = summary("demo", &a, "ml-granite");
        assert!(s.contains("REVFA"), "{s}");
        assert!(s.contains("local"), "the tiers must be named: {s}");
        assert!(s.contains("group"), "{s}");
        assert!(s.contains("global"), "{s}");
    }

    /// A project in no group must not be shown a group line pretending it has
    /// one, and must still be told where its memories go.
    #[test]
    fn a_project_without_a_group_says_so() {
        let a = Answers {
            model: Some("minilm-l6".into()),
            state_dir: None,
            group: None,
        };
        let s = summary("solo", &a, "minilm-l6");
        assert!(
            s.contains("none"),
            "the group line must read as absent: {s}"
        );
        assert!(!s.contains("shared with"), "nothing is shared: {s}");
        assert!(s.contains("global"), "but it still has two tiers: {s}");
    }

    /// Groups are offered from what exists, so joining one is a choice from a
    /// list rather than a name that has to be remembered exactly.
    #[test]
    fn known_groups_are_offered() {
        let s = groups_line(&[("REVFA".to_string(), 4)]);
        assert!(s.contains("REVFA"), "{s}");
        assert!(s.contains('4'), "{s}");
        assert!(
            groups_line(&[]).is_empty(),
            "nothing to offer on a fresh machine"
        );
    }

    /// A named directory replaces the default line entirely: showing both would
    /// leave it ambiguous which one is about to be written.
    #[test]
    fn a_named_index_directory_replaces_the_default() {
        let a = Answers {
            model: None,
            state_dir: Some("/mnt/big/devctx".into()),
            group: None,
        };
        let s = summary("demo", &a, "ml-granite");
        assert!(s.contains("/mnt/big/devctx"), "{s}");
        assert!(
            !s.contains(".devctx/state"),
            "only one location is shown: {s}"
        );
    }
}
