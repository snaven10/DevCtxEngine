//! What the setup wizard says, in the language it was asked to say it in.
//!
//! Kept apart from the questions themselves so the flow reads as a flow rather
//! than as a table of translations, and so adding a language is one `match` arm
//! rather than an edit to every prompt.
//!
//! The strings state consequences, not categories: "1–2 s becomes 180 s" rather
//! than "reranking". Someone is choosing between options they have not
//! measured, and the number is the argument.

use devctx_core::config::Language;

/// Every line the wizard can print, resolved for one language.
pub struct Text {
    pub lang: Language,
}

impl Text {
    pub fn new(lang: Language) -> Self {
        Self { lang }
    }

    fn pick(&self, en: &'static str, es: &'static str) -> &'static str {
        match self.lang {
            Language::En => en,
            Language::Es => es,
        }
    }

    // ── the language question itself ──────────────────────────────────────
    pub fn language_question() -> &'static str {
        "Language · Idioma"
    }
    pub fn language_en() -> &'static str {
        "English"
    }
    pub fn language_es() -> &'static str {
        "Español"
    }
    pub fn language_note() -> &'static str {
        "for these questions, and for summaries · para estas preguntas y los resúmenes"
    }

    /// How to drive a list, shown beside every question.
    pub fn keys_hint(&self) -> &'static str {
        self.pick(
            "(↑↓ to move, Enter to choose)",
            "(↑↓ para moverte, Enter para elegir)",
        )
    }
    /// The line above the model table, in the reader's language.
    pub fn models_in_use_prefix(&self) -> &'static str {
        self.pick("Already in use here:", "Ya en uso en esta máquina:")
    }
    pub fn models_in_use_note(&self) -> &'static str {
        self.pick(
            "Matching one keeps a single model in memory for anything that touches both a project and the shared memories.",
            "Elegir el mismo mantiene un solo modelo en memoria para todo lo que toque un proyecto y las memorias compartidas.",
        )
    }
    pub fn model_question(&self) -> &'static str {
        self.pick(
            "Embedding model — cannot change after indexing",
            "Modelo de embeddings — no se puede cambiar después de indexar",
        )
    }

    // ── copying an existing configuration ─────────────────────────────────
    pub fn copy_question(&self) -> &'static str {
        self.pick(
            "Start from another project's configuration?",
            "¿Partir de la configuración de otro proyecto?",
        )
    }
    pub fn copy_configure(&self) -> &'static str {
        self.pick(
            "Configure this one from scratch",
            "Configurar este desde cero",
        )
    }
    pub fn copy_note(&self) -> &'static str {
        self.pick(
            "copies everything but the name, path and group",
            "copia todo menos el nombre, la ruta y el grupo",
        )
    }

    // ── storage ───────────────────────────────────────────────────────────
    pub fn storage_heading(&self) -> &'static str {
        self.pick("── Storage ──", "── Almacenamiento ──")
    }
    pub fn index_dir_question(&self) -> &'static str {
        self.pick(
            "Index directory — an absolute path, or Enter to keep it in the repository",
            "Directorio del índice — una ruta absoluta, o Enter para dejarlo en el repositorio",
        )
    }
    /// An example, because the shape of an answer is the part a prompt usually
    /// leaves out: someone who has never seen this cannot tell whether it wants
    /// a path, a name, or a flag.
    pub fn index_dir_example(&self) -> &'static str {
        self.pick(
            "e.g. /mnt/disk/devctx/myproject — useful when the repository sits on a small disk, or when several worktrees would each carry a copy.",
            "ej. /mnt/disco/devctx/miproyecto — sirve cuando el repositorio está en un disco chico, o cuando varios worktrees cargarían cada uno su copia.",
        )
    }
    pub fn index_dir_note(&self) -> &'static str {
        self.pick(
            "The index is a build artefact — large, binary, rebuilt from the repository — so it lives inside it by default, and is git-ignored.",
            "El índice es un artefacto de build — grande, binario, se reconstruye desde el repositorio — así que vive adentro por defecto, y está en el gitignore.",
        )
    }
    pub fn hnsw_question(&self) -> &'static str {
        self.pick("Vector index", "Índice vectorial")
    }
    pub fn hnsw_on(&self) -> &'static str {
        self.pick("HNSW — 49 ms per search", "HNSW — 49 ms por búsqueda")
    }
    pub fn hnsw_off(&self) -> &'static str {
        self.pick("None — 84 ms, exact", "Ninguno — 84 ms, exacta")
    }
    pub fn hnsw_note(&self) -> &'static str {
        self.pick(
            "measured on 17k vectors; recall was unchanged",
            "medido sobre 17k vectores; el recall no cambió",
        )
    }
    pub fn metric_question(&self) -> &'static str {
        self.pick("Distance metric", "Métrica de distancia")
    }
    pub fn metric_cosine_note(&self) -> &'static str {
        self.pick("always correct", "siempre correcta")
    }
    pub fn metric_ip_note(&self) -> &'static str {
        self.pick(
            "cheaper, but only equivalent for normalized embeddings",
            "más barata, pero equivalente solo con embeddings normalizados",
        )
    }
    pub fn fts_question(&self) -> &'static str {
        self.pick(
            "Also search by exact words, not only by meaning?",
            "¿Buscar también por palabra exacta, no solo por significado?",
        )
    }
    /// Named by what it is for, not by its algorithm. Someone deciding here has
    /// no reason to know what BM25 is, and every reason to know that searching
    /// for a symbol by name is otherwise poor.
    pub fn fts_note(&self) -> &'static str {
        self.pick(
            "Semantic search finds code that means what you asked; it is weak at finding a name you already know. This adds `--mode keyword` and `--mode hybrid`, which fuses both. Costs disk and a few seconds per index.",
            "La búsqueda semántica encuentra código que significa lo que pediste; es floja para encontrar un nombre que ya conocés. Esto agrega `--mode keyword` y `--mode hybrid`, que fusiona las dos. Cuesta disco y unos segundos por indexado.",
        )
    }

    // ── indexing ──────────────────────────────────────────────────────────
    pub fn indexing_heading(&self) -> &'static str {
        self.pick("── Indexing ──", "── Indexado ──")
    }
    pub fn branches_heading(&self) -> &'static str {
        self.pick(
            "── Branches to keep indexed ─────────────────────────────",
            "── Ramas que se mantienen indexadas ─────────────────────",
        )
    }
    pub fn branches_detected(&self, branch: &str) -> String {
        match self.lang {
            Language::En => format!(
                "  Trunk detected: {branch}. It is indexed by default, and searches
                   from a branch that has never been indexed fall back to it."
            ),
            Language::Es => format!(
                "  Trunk detectado: {branch}. Se indexa por defecto, y las búsquedas
                   desde una rama nunca indexada caen a ella."
            ),
        }
    }
    pub fn branches_none(&self) -> &'static str {
        self.pick(
            "  No `main` or `master` here, so the checked-out branch is indexed —
               which is how this has always worked. Set `indexing.branches` later
               if you want several branches live at once.",
            "  No hay `main` ni `master`, así que se indexa la rama checkouteada —
               que es como funcionó siempre. Podés poner `indexing.branches` después
               si querés varias ramas vivas a la vez.",
        )
    }
    pub fn branches_question(&self) -> &'static str {
        self.pick(
            "Other branches to keep indexed, comma-separated — or Enter for none",
            "Otras ramas a mantener indexadas, separadas por coma — o Enter para ninguna",
        )
    }
    pub fn exclude_question(&self) -> &'static str {
        self.pick(
            "Exclude patterns, comma-separated — or Enter for none",
            "Patrones a excluir, separados por coma — o Enter para ninguno",
        )
    }
    /// Real patterns rather than a description of patterns.
    pub fn exclude_example(&self) -> &'static str {
        self.pick(
            "e.g.  dist/, *.generated.ts, docs/vendor/**, **/__snapshots__/**\n     A trailing / means a directory; ** crosses directories; * does not.",
            "ej.  dist/, *.generated.ts, docs/vendor/**, **/__snapshots__/**\n     La / final indica un directorio; ** cruza carpetas; * no.",
        )
    }
    pub fn exclude_note(&self) -> &'static str {
        self.pick(
            "Anything git already ignores is excluded. This is for code git tracks but that is not worth searching — .gitignore syntax, comma-separated.",
            "Lo que git ya ignora queda afuera. Esto es para código que git sí versiona pero que no vale la pena buscar — sintaxis de .gitignore, separados por coma.",
        )
    }

    // ── memories ──────────────────────────────────────────────────────────
    pub fn memories_heading(&self) -> &'static str {
        self.pick("── Memories ──", "── Memorias ──")
    }
    pub fn group_question(&self) -> &'static str {
        self.pick("Group for this repository", "Grupo de este repositorio")
    }
    pub fn group_none(&self) -> &'static str {
        self.pick(
            "None — this repository alone",
            "Ninguno — solo este repositorio",
        )
    }
    pub fn group_new(&self) -> &'static str {
        self.pick("A new group…", "Un grupo nuevo…")
    }
    pub fn group_note(&self) -> &'static str {
        self.pick(
            "memories shared between the repositories of one product",
            "memorias compartidas entre los repositorios de un producto",
        )
    }
    pub fn group_name_question(&self) -> &'static str {
        self.pick("Name of the new group", "Nombre del grupo nuevo")
    }

    // ── search quality ────────────────────────────────────────────────────
    pub fn rerank_heading(&self) -> &'static str {
        self.pick("── Search quality ──", "── Calidad de búsqueda ──")
    }
    pub fn rerank_question(&self) -> &'static str {
        self.pick("Reranking", "Reordenamiento")
    }
    pub fn rerank_off(&self) -> &'static str {
        self.pick("Off — 1–2 s per search", "Apagado — 1–2 s por búsqueda")
    }
    pub fn rerank_on(&self) -> &'static str {
        self.pick("On — 180 s per search", "Encendido — 180 s por búsqueda")
    }
    pub fn rerank_note(&self) -> &'static str {
        self.pick(
            "a cross-encoder reorders results the retriever mostly had right",
            "un cross-encoder reordena resultados que el buscador ya traía casi bien",
        )
    }
    pub fn rerank_model_question(&self) -> &'static str {
        self.pick("Reranker model", "Modelo de reordenamiento")
    }
    pub fn rerank_pool_question(&self) -> &'static str {
        self.pick(
            "Candidates it sees (this is the cost)",
            "Candidatos que evalúa (ahí está el costo)",
        )
    }

    // ── summarization ─────────────────────────────────────────────────────
    pub fn summary_heading(&self) -> &'static str {
        self.pick("── Summarization ──", "── Resumen ──")
    }
    pub fn summarizer_question(&self) -> &'static str {
        self.pick("Summarizer", "Resumidor")
    }
    pub fn summarizer_extractive(&self) -> &'static str {
        self.pick("extractive — offline, free", "extractivo — offline, gratis")
    }
    pub fn summarizer_extractive_note(&self) -> &'static str {
        self.pick(
            "ranks sentences with the embedding model",
            "ordena oraciones con el modelo de embeddings",
        )
    }
    pub fn summarizer_openai(&self) -> &'static str {
        self.pick(
            "openai — sends the text away",
            "openai — manda el texto afuera",
        )
    }
    pub fn summarizer_noop(&self) -> &'static str {
        self.pick("noop — truncates", "noop — trunca")
    }
    pub fn target_tokens_question(&self) -> &'static str {
        self.pick("Summary length in tokens", "Largo del resumen en tokens")
    }

    // ── offline ───────────────────────────────────────────────────────────
    pub fn offline_question(&self) -> &'static str {
        self.pick("Downloading models", "Descarga de modelos")
    }
    pub fn offline_auto(&self) -> &'static str {
        self.pick("Automatic", "Automática")
    }
    pub fn offline_never(&self) -> &'static str {
        self.pick("Never — offline", "Nunca — offline")
    }
    pub fn offline_always(&self) -> &'static str {
        self.pick("Always allowed", "Siempre permitida")
    }

    // ── confirmation ──────────────────────────────────────────────────────
    pub fn write_question(&self) -> &'static str {
        self.pick("Write this configuration?", "¿Escribir esta configuración?")
    }
    pub fn write_yes(&self) -> &'static str {
        self.pick("Yes", "Sí")
    }
    pub fn write_no(&self) -> &'static str {
        self.pick("No, cancel", "No, cancelar")
    }
    pub fn nothing_written(&self) -> &'static str {
        self.pick("Nothing written.", "No se escribió nada.")
    }

    // ── summary labels ────────────────────────────────────────────────────
    pub fn label_project(&self) -> &'static str {
        self.pick("project ", "proyecto")
    }
    pub fn label_group(&self) -> &'static str {
        self.pick("group   ", "grupo   ")
    }
    pub fn label_model(&self) -> &'static str {
        self.pick("model   ", "modelo  ")
    }
    pub fn label_index(&self) -> &'static str {
        self.pick("index   ", "índice  ")
    }
    pub fn label_keyword(&self) -> &'static str {
        self.pick("keyword ", "keyword ")
    }
    pub fn label_exclude(&self) -> &'static str {
        self.pick("exclude ", "excluir ")
    }
    pub fn label_rerank(&self) -> &'static str {
        self.pick("rerank  ", "reorden ")
    }
    pub fn label_summary(&self) -> &'static str {
        self.pick("summary ", "resumen ")
    }
    pub fn label_memories(&self) -> &'static str {
        self.pick("memories", "memorias")
    }
    pub fn none(&self) -> &'static str {
        self.pick("none", "ninguno")
    }
    pub fn on(&self) -> &'static str {
        self.pick("on", "sí")
    }
    pub fn off(&self) -> &'static str {
        self.pick("off", "no")
    }
    pub fn copied_from(&self) -> &'static str {
        self.pick("copied from", "copiada de")
    }
    pub fn shared_with_group(&self) -> &'static str {
        self.pick(
            "→ memories shared with that product's repositories",
            "→ memorias compartidas con los repositorios de ese producto",
        )
    }
    pub fn tiers_with_group(&self) -> &'static str {
        self.pick(
            "local → this repository · group → central store · global → central store",
            "local → este repositorio · grupo → store central · global → store central",
        )
    }
    pub fn tiers_without_group(&self) -> &'static str {
        self.pick(
            "local → this repository · global → central store",
            "local → este repositorio · global → store central",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both languages must actually differ. A translation that fell back to
    /// English would look like a working choice and read as an ignored one.
    #[test]
    fn the_two_languages_say_different_things() {
        let en = Text::new(Language::En);
        let es = Text::new(Language::Es);
        assert_ne!(en.storage_heading(), es.storage_heading());
        assert_ne!(en.group_question(), es.group_question());
        assert_ne!(en.rerank_on(), es.rerank_on());
        assert_ne!(en.write_question(), es.write_question());
        assert_ne!(en.tiers_with_group(), es.tiers_with_group());
    }

    /// The measured numbers are the argument, so they survive translation.
    #[test]
    fn the_measurements_appear_in_both_languages() {
        for t in [Text::new(Language::En), Text::new(Language::Es)] {
            assert!(t.rerank_on().contains("180"), "{}", t.rerank_on());
            assert!(t.rerank_off().contains("1–2"), "{}", t.rerank_off());
            assert!(t.hnsw_on().contains("49"), "{}", t.hnsw_on());
            assert!(t.hnsw_off().contains("84"), "{}", t.hnsw_off());
        }
    }

    /// The language question itself is bilingual: it is asked before anyone has
    /// said which language they read.
    #[test]
    fn the_language_question_is_asked_in_both() {
        assert!(Text::language_question().contains("Language"));
        assert!(Text::language_question().contains("Idioma"));
        assert!(Text::language_note().contains("summaries"));
        assert!(Text::language_note().contains("resúmenes"));
    }
}
