//! The questions `devctx init` asks, and the summary it shows before writing.
//!
//! Every setting the config file holds is asked for, because every one of them
//! is easier to choose here — with the consequence stated next to it — than to
//! discover later in a file nobody knew to open. The cost of that is a lot of
//! questions, paid down two ways: the first one offers to copy an existing
//! project's configuration, which answers all the rest at once, and every
//! prompt carries the default it would have used, so Enter throughout
//! reproduces the previous silent behaviour exactly.
//!
//! Skipped entirely without a terminal. An agent following the setup guide runs
//! `init` with no TTY, and a prompt it cannot answer would hang the setup it was
//! told to perform.

use std::io::{IsTerminal as _, Write as _};

use anyhow::Result;
use devctx_core::config::{
    Embeddings, Indexing, Language, Offline, Reranking, Storage, Summarization,
};

use crate::models;

/// Everything the wizard collected. `None` means "leave the default alone".
#[derive(Debug, Default, Clone)]
pub struct Answers {
    /// Copy every setting from this registered project instead of asking.
    pub copy_from: Option<String>,
    pub model: Option<String>,
    pub state_dir: Option<String>,
    pub group: Option<String>,
    pub language: Option<Language>,
    pub offline: Option<Offline>,
    pub storage: Option<Storage>,
    pub indexing: Option<Indexing>,
    pub reranking: Option<Reranking>,
    pub summarization: Option<Summarization>,
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

/// A yes/no question. Anything unrecognized keeps the default rather than
/// asking again: a wizard that argues with a typo is worse than one that
/// proceeds sensibly and shows its summary.
fn ask_bool(question: &str, default: bool) -> bool {
    let hint = if default { "Y/n" } else { "y/N" };
    match ask_line(question, hint) {
        None => default,
        Some(s) => match s.to_ascii_lowercase().as_str() {
            "y" | "yes" => true,
            "n" | "no" => false,
            _ => default,
        },
    }
}

/// A number, keeping the default when the answer is not one.
fn ask_usize(question: &str, default: usize) -> usize {
    match ask_line(question, &default.to_string()) {
        None => default,
        Some(s) => s.trim().parse().unwrap_or(default),
    }
}

/// Ask everything — or nothing, when there is no terminal.
pub fn ask(
    defaults: &Embeddings,
    in_use: &[(String, usize)],
    groups: &[(String, usize)],
    projects: &[String],
) -> Result<Answers> {
    if !std::io::stdin().is_terminal() {
        return Ok(Answers::default());
    }

    // First, because answering it answers everything else. Setting up the
    // fourth repository of one product should not mean walking a questionnaire
    // whose right answers are all "the same as the last three".
    if !projects.is_empty() {
        println!("Registered projects: {}", projects.join(", "));
        if let Some(name) = ask_line(
            "Copy configuration from one of them (blank to configure this one)",
            "configure",
        )
        .filter(|s| s != "configure")
        {
            return Ok(Answers {
                copy_from: Some(name),
                ..Default::default()
            });
        }
        println!();
    }

    let model = models::prompt(&defaults.model, in_use)?;

    println!("\n── Storage ──");
    println!(
        "The index is a build artefact — large, binary, rebuilt from the \
         repository — so it lives inside it by default, and is git-ignored."
    );
    let state_dir =
        ask_line("Index directory (blank = inside the repository)", "repo").filter(|s| s != "repo");

    let d = Storage::default();
    println!(
        "\nHNSW is an approximate index: measured 84 ms → 49 ms per search on a \
         17k-vector store, with recall unchanged."
    );
    let hnsw = ask_bool("Build an HNSW index", d.hnsw);
    let metric = if hnsw {
        println!(
            "`cosine` is always correct. `ip` is cheaper but only equivalent \
             when the embeddings are unit-normalized — the local models are."
        );
        ask_line("Distance metric (cosine/ip)", &d.metric).unwrap_or(d.metric.clone())
    } else {
        d.metric.clone()
    };
    println!("\nBM25 lets `search --keyword` match exact identifiers.");
    let fts = ask_bool("Build a keyword index too", d.fts);

    println!("\n── Indexing ──");
    println!(
        "Anything git already ignores is excluded. This is for code git tracks \
         but that is not worth searching — `.gitignore` syntax, comma-separated."
    );
    let exclude: Vec<String> = ask_line("Exclude patterns", "none")
        .filter(|s| s != "none")
        .map(|s| s.split(',').map(|p| p.trim().to_string()).collect())
        .unwrap_or_default();

    println!("\n── Memories ──");
    println!("Memories can be shared between the repositories of one product.");
    let line = groups_line(groups);
    if !line.is_empty() {
        println!("{line}");
    }
    let group = ask_line("Group for this repository", "none").filter(|g| g != "none");

    println!("\n── Search quality ──");
    let r = Reranking::default();
    println!(
        "A cross-encoder reorders results. Measured here: 1–2 s per search \
         becomes 180 s, for an ordering the retriever mostly had right."
    );
    let rerank_enabled = ask_bool("Enable reranking", r.enabled);
    let (rerank_model, rerank_pool) = if rerank_enabled {
        let m = ask_line("Reranker model (bge-base/bge-v2-m3/jina-turbo)", &r.model)
            .unwrap_or(r.model.clone());
        println!("The pool is how many candidates it sees — and the whole cost.");
        (m, ask_usize("Candidate pool", r.pool))
    } else {
        (r.model.clone(), r.pool)
    };

    println!("\n── Summarization ──");
    let s = Summarization::default();
    println!(
        "`extractive` ranks sentences with the embedding model, offline and \
         free. `openai` is abstractive and sends the text away. `noop` truncates."
    );
    let sum_provider =
        ask_line("Summarizer (extractive/openai/noop)", &s.provider).unwrap_or(s.provider.clone());
    let sum_target = ask_usize("Target length in tokens", s.target_tokens);
    let require_local = if sum_provider == "extractive" || sum_provider == "noop" {
        s.require_local
    } else {
        println!("`require_local` blocks any provider that sends text off this machine.");
        ask_bool(
            "Keep require_local on (this will refuse the provider above)",
            s.require_local,
        )
    };

    println!("\n── Language ──");
    let language = match ask_line("Language for summaries and UI (en/es)", "en") {
        Some(l) if l.eq_ignore_ascii_case("es") => Some(Language::Es),
        Some(_) => Some(Language::En),
        None => None,
    };

    println!("\nOffline mode decides whether a missing model may be downloaded.");
    let offline = match ask_line("Offline (auto/true/false)", "auto") {
        Some(o) if o.eq_ignore_ascii_case("true") => Some(Offline::True),
        Some(o) if o.eq_ignore_ascii_case("false") => Some(Offline::False),
        Some(_) => Some(Offline::Auto),
        None => None,
    };

    Ok(Answers {
        copy_from: None,
        model,
        state_dir,
        group,
        language,
        offline,
        storage: Some(Storage {
            db_path: String::new(),
            hnsw,
            metric,
            fts,
        }),
        indexing: Some(Indexing { exclude }),
        reranking: Some(Reranking {
            enabled: rerank_enabled,
            model: rerank_model,
            model_dir: r.model_dir,
            pool: rerank_pool,
        }),
        summarization: Some(Summarization {
            provider: sum_provider,
            require_local,
            target_tokens: sum_target,
            model: s.model,
        }),
    })
}

/// What will be written, in the terms the decisions were made in.
///
/// Shows every section, including the ones nobody changed: the point of the
/// summary is that what lands in the file is visible before it lands, and a
/// setting omitted because it kept its default is exactly the kind that gets
/// discovered months later.
pub fn summary(name: &str, a: &Answers, model: &str) -> String {
    if let Some(src) = &a.copy_from {
        return format!("  project   {name}\n  config    copied from `{src}`");
    }
    let group = match &a.group {
        Some(g) => format!("{g}  → memories shared with that product's repositories"),
        None => "none".to_string(),
    };
    let index = match &a.state_dir {
        Some(d) => d.clone(),
        None => "./.devctx/state/index.duckdb".to_string(),
    };
    let st = a.storage.clone().unwrap_or_default();
    let rr = a.reranking.clone().unwrap_or_default();
    let sm = a.summarization.clone().unwrap_or_default();
    let ix = a.indexing.clone().unwrap_or_default();
    let tiers = match &a.group {
        Some(g) => {
            format!(
                "local → this repository · group ({g}) → central store · global → central store"
            )
        }
        None => "local → this repository · global → central store".to_string(),
    };
    let exclude = if ix.exclude.is_empty() {
        "none".to_string()
    } else {
        ix.exclude.join(", ")
    };
    let rerank = if rr.enabled {
        format!("{} (pool {})", rr.model, rr.pool)
    } else {
        "off".to_string()
    };
    format!(
        "  project   {name}\n  \
         group     {group}\n  \
         model     {model}\n  \
         index     {index}\n  \
         hnsw      {} ({})\n  \
         keyword   {}\n  \
         exclude   {exclude}\n  \
         rerank    {rerank}\n  \
         summary   {} · {} tokens\n  \
         memories  {tiers}",
        if st.hnsw { "on" } else { "off" },
        st.metric,
        if st.fts { "on" } else { "off" },
        sm.provider,
        sm.target_tokens,
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

    /// The summary names every section, including ones left at their default.
    /// A setting that is written but never shown is the kind discovered months
    /// later, which is what asking about it was meant to prevent.
    #[test]
    fn the_summary_shows_every_section() {
        let a = Answers {
            model: Some("ml-granite".into()),
            group: Some("REVFA".into()),
            ..Default::default()
        };
        let s = summary("demo", &a, "ml-granite");
        for expected in [
            "project", "group", "model", "index", "hnsw", "keyword", "exclude", "rerank",
            "summary", "memories",
        ] {
            assert!(s.contains(expected), "`{expected}` missing from:\n{s}");
        }
    }

    /// The three tiers are spelled out where the choice is being made.
    #[test]
    fn the_summary_explains_where_each_kind_of_memory_will_live() {
        let a = Answers {
            group: Some("REVFA".into()),
            ..Default::default()
        };
        let s = summary("demo", &a, "ml-granite");
        assert!(s.contains("REVFA"), "{s}");
        assert!(s.contains("local"), "{s}");
        assert!(s.contains("group"), "{s}");
        assert!(s.contains("global"), "{s}");
    }

    /// A project in no group must not be shown a group line pretending it has
    /// one, and must still be told where its memories go.
    #[test]
    fn a_project_without_a_group_says_so() {
        let s = summary("solo", &Answers::default(), "minilm-l6");
        assert!(s.contains("none"), "{s}");
        assert!(!s.contains("shared with"), "nothing is shared: {s}");
        assert!(s.contains("global"), "but it still has two tiers: {s}");
    }

    /// Copying makes every other line moot, so the summary says only that.
    /// Listing settings that were inherited rather than chosen would read as
    /// though they had been decided here.
    #[test]
    fn copying_a_configuration_is_summarised_as_exactly_that() {
        let a = Answers {
            copy_from: Some("REVFA_BackEnd".into()),
            ..Default::default()
        };
        let s = summary("demo", &a, "ml-granite");
        assert!(s.contains("copied from `REVFA_BackEnd`"), "{s}");
        assert!(!s.contains("rerank"), "nothing here was chosen: {s}");
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
            state_dir: Some("/mnt/big/devctx".into()),
            ..Default::default()
        };
        let s = summary("demo", &a, "ml-granite");
        assert!(s.contains("/mnt/big/devctx"), "{s}");
        assert!(
            !s.contains(".devctx/state"),
            "only one location is shown: {s}"
        );
    }
}
