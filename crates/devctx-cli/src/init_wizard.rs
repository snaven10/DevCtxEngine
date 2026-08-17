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

use std::io::IsTerminal as _;

use anyhow::Result;
use devctx_core::config::{
    Embeddings, Indexing, Language, Offline, Reranking, Storage, Summarization,
};

use crate::models;
use crate::prompt_ui::{self, Choice};
use crate::wizard_text::Text;

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

    // Asked first, and in both languages, because everything after it is
    // printed in whatever this answers.
    let lang = if prompt_ui::select(
        Text::language_question(),
        &[
            Choice::new("en", Text::language_en(), Text::language_note()),
            Choice::new("es", Text::language_es(), Text::language_note()),
        ],
        0,
    ) == "es"
    {
        Language::Es
    } else {
        Language::En
    };
    let t = Text::new(lang);
    prompt_ui::set_hint(t.keys_hint());

    // Then: copying, because answering it answers everything else. Setting up
    // the fourth repository of one product should not mean walking a
    // questionnaire whose right answers are all "the same as the last three".
    if !projects.is_empty() {
        let mut choices = vec![Choice::new("", t.copy_configure(), "")];
        for p in projects {
            choices.push(Choice::new(p, p, t.copy_note()));
        }
        let picked = prompt_ui::select(t.copy_question(), &choices, 0);
        if !picked.is_empty() {
            return Ok(Answers {
                copy_from: Some(picked),
                language: Some(lang),
                ..Default::default()
            });
        }
    }

    let model = models::prompt(&defaults.model, in_use, &t)?;

    println!("\n{}", t.storage_heading());
    println!("{}", t.index_dir_note());
    println!("{}", t.index_dir_example());
    let state_dir = {
        let d = prompt_ui::input(t.index_dir_question(), "");
        (!d.trim().is_empty()).then_some(d)
    };

    let d = Storage::default();
    let hnsw = prompt_ui::select(
        t.hnsw_question(),
        &[
            Choice::new("y", t.hnsw_on(), t.hnsw_note()),
            Choice::new("n", t.hnsw_off(), t.hnsw_note()),
        ],
        if d.hnsw { 0 } else { 1 },
    ) == "y";
    let metric = if hnsw {
        prompt_ui::select(
            t.metric_question(),
            &[
                Choice::new("cosine", "cosine", t.metric_cosine_note()),
                Choice::new("ip", "ip (inner product)", t.metric_ip_note()),
            ],
            0,
        )
    } else {
        d.metric.clone()
    };
    let fts = prompt_ui::confirm(t.fts_question(), d.fts, t.on(), t.off());
    if fts {
        println!("  {}", t.fts_note());
    }

    println!("\n{}", t.indexing_heading());
    println!("{}", t.exclude_note());
    println!("{}", t.exclude_example());
    let exclude: Vec<String> = prompt_ui::input(t.exclude_question(), "")
        .split(',')
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();

    println!("\n{}", t.memories_heading());
    let group = {
        let mut choices = vec![Choice::new("", t.group_none(), "")];
        for (g, n) in groups {
            choices.push(Choice::new(g, &format!("{g} ({n})"), t.group_note()));
        }
        choices.push(Choice::new("\u{1}new", t.group_new(), t.group_note()));
        let picked = prompt_ui::select(t.group_question(), &choices, 0);
        match picked.as_str() {
            "" => None,
            "\u{1}new" => {
                let name = prompt_ui::input(t.group_name_question(), "");
                (!name.trim().is_empty()).then_some(name)
            }
            g => Some(g.to_string()),
        }
    };

    println!("\n{}", t.rerank_heading());
    let r = Reranking::default();
    let rerank_enabled = prompt_ui::select(
        t.rerank_question(),
        &[
            Choice::new("n", t.rerank_off(), t.rerank_note()),
            Choice::new("y", t.rerank_on(), t.rerank_note()),
        ],
        if r.enabled { 1 } else { 0 },
    ) == "y";
    let (rerank_model, rerank_pool) = if rerank_enabled {
        let m = prompt_ui::select(
            t.rerank_model_question(),
            &[
                Choice::new("bge-base", "bge-base", "English"),
                Choice::new("bge-v2-m3", "bge-v2-m3", "multilingual"),
                Choice::new("jina-turbo", "jina-turbo", "fastest"),
            ],
            0,
        );
        let pool = prompt_ui::input(t.rerank_pool_question(), &r.pool.to_string())
            .parse()
            .unwrap_or(r.pool);
        (m, pool)
    } else {
        (r.model.clone(), r.pool)
    };

    println!("\n{}", t.summary_heading());
    let s = Summarization::default();
    let sum_provider = prompt_ui::select(
        t.summarizer_question(),
        &[
            Choice::new(
                "extractive",
                t.summarizer_extractive(),
                t.summarizer_extractive_note(),
            ),
            Choice::new("openai", t.summarizer_openai(), ""),
            Choice::new("noop", t.summarizer_noop(), ""),
        ],
        0,
    );
    let sum_target = prompt_ui::input(t.target_tokens_question(), &s.target_tokens.to_string())
        .parse()
        .unwrap_or(s.target_tokens);

    let offline = match prompt_ui::select(
        t.offline_question(),
        &[
            Choice::new("auto", t.offline_auto(), ""),
            Choice::new("true", t.offline_never(), ""),
            Choice::new("false", t.offline_always(), ""),
        ],
        0,
    )
    .as_str()
    {
        "true" => Some(Offline::True),
        "false" => Some(Offline::False),
        _ => Some(Offline::Auto),
    };

    Ok(Answers {
        copy_from: None,
        model,
        state_dir,
        group,
        language: Some(lang),
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
            require_local: s.require_local,
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
    let t = Text::new(a.language.unwrap_or_default());
    if let Some(src) = &a.copy_from {
        return format!(
            "  {} {name}\n  config   {} `{src}`",
            t.label_project(),
            t.copied_from()
        );
    }
    let group = match &a.group {
        Some(g) => format!("{g}  {}", t.shared_with_group()),
        None => t.none().to_string(),
    };
    let index = match &a.state_dir {
        Some(d) => d.clone(),
        None => "./.devctx/state/index.duckdb".to_string(),
    };
    let st = a.storage.clone().unwrap_or_default();
    let rr = a.reranking.clone().unwrap_or_default();
    let sm = a.summarization.clone().unwrap_or_default();
    let ix = a.indexing.clone().unwrap_or_default();
    let tiers = if a.group.is_some() {
        t.tiers_with_group()
    } else {
        t.tiers_without_group()
    };
    let exclude = if ix.exclude.is_empty() {
        t.none().to_string()
    } else {
        ix.exclude.join(", ")
    };
    let rerank = if rr.enabled {
        format!("{} (pool {})", rr.model, rr.pool)
    } else {
        t.off().to_string()
    };
    let onoff = |b: bool| if b { t.on() } else { t.off() };
    format!(
        "  {} {name}\n  \
         {} {group}\n  \
         {} {model}\n  \
         {} {index}\n  \
         hnsw     {} ({})\n  \
         {} {}\n  \
         {} {exclude}\n  \
         {} {rerank}\n  \
         {} {} · {} tokens\n  \
         {} {tiers}",
        t.label_project(),
        t.label_group(),
        t.label_model(),
        t.label_index(),
        onoff(st.hnsw),
        st.metric,
        t.label_keyword(),
        onoff(st.fts),
        t.label_exclude(),
        t.label_rerank(),
        t.label_summary(),
        sm.provider,
        sm.target_tokens,
        t.label_memories(),
    )
}

/// Ask for confirmation.
///
/// `true` without a terminal: there is nobody to ask, and the caller asked for
/// this by running the command.
pub fn confirm(lang: Language) -> bool {
    if !std::io::stdin().is_terminal() {
        return true;
    }
    let t = Text::new(lang);
    prompt_ui::confirm(t.write_question(), true, t.write_yes(), t.write_no())
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

    /// The summary follows the language that was chosen. Answering in Spanish
    /// and being shown an English summary would leave the choice looking
    /// ignored at the one moment it is being checked.
    #[test]
    fn the_summary_is_written_in_the_chosen_language() {
        let a = Answers {
            language: Some(Language::Es),
            group: Some("REVFA".into()),
            ..Default::default()
        };
        let s = summary("demo", &a, "ml-granite");
        assert!(s.contains("memorias"), "{s}");
        assert!(s.contains("compartidas"), "{s}");
        assert!(!s.contains("shared with"), "{s}");
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
