//! The command map printed under `devctx --help`.
//!
//! Clap already lists every subcommand, in the order they are declared, one
//! terse line each. That list is complete and nearly unreadable: twenty-eight
//! entries with no grouping, so finding the command for a job means reading all
//! of them and hoping its one-liner uses the word you were thinking of. It does
//! not always — `impact` is described as "blast radius", and someone looking for
//! impact analysis scanned straight past it and concluded the command did not
//! exist.
//!
//! So the list stays (it is the reference) and this map goes underneath it (it
//! is the index): commands grouped by the job they do, in the reader's own
//! language, with the few that are easy to miss spelled out.
//!
//! Only this map is translated, not the whole help. Clap takes `help` strings as
//! compile-time attributes, so a fully bilingual `--help` means every argument
//! carrying both languages and a `match` to pick one. That is a much larger
//! change than the problem justifies, and this map is where the discovery
//! problem actually lives.

use devctx_core::config::{find_config_file, Language, ProjectConfig};

/// The language to print the map in.
///
/// `DEVCTX_LANG` first so a reader can override without editing anything, then
/// the project's configured language — the wizard already asked, and asking
/// again through an environment variable would be asking twice.
pub fn language() -> Language {
    match std::env::var("DEVCTX_LANG").as_deref() {
        Ok("es") => return Language::Es,
        Ok("en") => return Language::En,
        _ => {}
    }
    std::env::current_dir()
        .ok()
        .and_then(|d| find_config_file(&d))
        .and_then(|p| ProjectConfig::load(&p).ok())
        .map(|c| c.language)
        .unwrap_or_default()
}

/// The map itself.
pub fn text(lang: Language) -> String {
    match lang {
        Language::En => EN.to_string(),
        Language::Es => ES.to_string(),
    }
}

const EN: &str = "\
Commands by job:
  Set up        init · models · mcp configure · projects add
  Index         index · reindex · watch · hooks install · status · repair
  Read code     search · symbol · context · impact · routes · summarize
  Memory        remember · recall · memory-stats · memory-forget · memories export|import
  Interfaces    tui · web · api · mcp · serve

Easy to miss:
  impact <symbol>   Everything a change to that symbol would reach, transitively —
                    what other tools call impact analysis.
  context <query>   One budgeted brief: what is already known, the code that
                    ranks highest, and the memories recorded against those files.
  symbol <name>     A symbol's definition and code, by name.
  routes            The HTTP routes the frameworks in this repo declare.
  memories export   Every memory as JSONL; import only ever adds, never overwrites.
  projects list     What this machine has indexed, and where.

devctx <command> --help explains one command in full.
Set DEVCTX_LANG=es for this summary in Spanish.";

const ES: &str = "\
Comandos por tarea:
  Configurar    init · models · mcp configure · projects add
  Indexar       index · reindex · watch · hooks install · status · repair
  Leer código   search · symbol · context · impact · routes · summarize
  Memoria       remember · recall · memory-stats · memory-forget · memories export|import
  Interfaces    tui · web · api · mcp · serve

Fáciles de pasar por alto:
  impact <símbolo>  Todo lo que alcanzaría un cambio en ese símbolo, de forma
                    transitiva — el análisis de impacto.
  context <query>   Un brief con presupuesto: lo que ya se sabe, el código que
                    mejor rankea, y las memorias registradas sobre esos archivos.
  symbol <nombre>   La definición y el código de un símbolo, por nombre.
  routes            Las rutas HTTP que declaran los frameworks de este repo.
  memories export   Todas las memorias en JSONL; import solo agrega, nunca pisa.
  projects list     Lo que esta máquina tiene indexado, y dónde.

devctx <comando> --help explica un comando en detalle.
Poné DEVCTX_LANG=en para este resumen en inglés.";

#[cfg(test)]
mod tests {
    use super::*;

    /// The map is an index into the command list, so a command that exists and
    /// is not in it is a command the map fails to index. These are the ones the
    /// reader was demonstrably unable to find.
    #[test]
    fn the_map_names_the_commands_that_were_hard_to_find() {
        for lang in [Language::En, Language::Es] {
            let t = text(lang);
            for cmd in [
                "impact",
                "routes",
                "memories export",
                "projects list",
                "symbol",
                "context",
            ] {
                assert!(t.contains(cmd), "{cmd} missing from the {lang:?} map");
            }
        }
    }

    /// Every group of the command list must appear, or a whole class of command
    /// stays invisible.
    #[test]
    fn every_group_of_commands_is_represented() {
        let t = text(Language::En);
        for cmd in [
            "init", "index", "search", "remember", "recall", "tui", "mcp",
        ] {
            assert!(t.contains(cmd), "{cmd} missing");
        }
    }

    /// A translation that quietly fell back to English would read as finished
    /// and be useless.
    #[test]
    fn the_two_languages_differ() {
        assert_ne!(text(Language::En), text(Language::Es));
        assert!(text(Language::Es).contains("Comandos por tarea"));
    }

    /// The override wins over anything on disk: it is the escape hatch for a
    /// reader whose project is configured in a language they do not read.
    #[test]
    fn the_environment_override_is_honoured() {
        // Serialized against other env-touching tests by running in one test.
        let restore = std::env::var("DEVCTX_LANG").ok();
        std::env::set_var("DEVCTX_LANG", "es");
        assert_eq!(language(), Language::Es);
        std::env::set_var("DEVCTX_LANG", "en");
        assert_eq!(language(), Language::En);
        match restore {
            Some(v) => std::env::set_var("DEVCTX_LANG", v),
            None => std::env::remove_var("DEVCTX_LANG"),
        }
    }
}
