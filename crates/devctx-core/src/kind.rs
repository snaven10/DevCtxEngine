//! What a file *is for*: code, test, doc or config.
//!
//! A pure function of the path (and, as a fallback, the indexed language), so
//! the search side (ranking penalty, `kind` / `include_tests` filters) and the
//! index side can agree on one classification without either reading file
//! contents. The rules are deliberately conservative: only conventions that
//! name the file's role (`test/`, `*.spec.*`, `README*`, `*.sql`) count, never a
//! guess from the text. A Rust `#[cfg(test)]` module inside `src/lib.rs` is
//! therefore still `Code` — it cannot be told apart from the path.

use serde::{Deserialize, Serialize};

/// The role of a file in a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathKind {
    /// Production source (the default).
    Code,
    /// Tests, specs, fixtures, mocks.
    Test,
    /// Prose: Markdown, READMEs, `docs/`.
    Doc,
    /// Data and configuration: SQL, YAML, JSON, TOML, properties.
    Config,
}

impl PathKind {
    /// Parse the user-facing name (`code`, `test`, `doc`, `config`).
    pub fn parse(s: &str) -> Option<PathKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "code" => Some(PathKind::Code),
            "test" | "tests" => Some(PathKind::Test),
            "doc" | "docs" => Some(PathKind::Doc),
            "config" | "cfg" => Some(PathKind::Config),
            _ => None,
        }
    }

    /// The user-facing name.
    pub fn as_str(self) -> &'static str {
        match self {
            PathKind::Code => "code",
            PathKind::Test => "test",
            PathKind::Doc => "doc",
            PathKind::Config => "config",
        }
    }
}

/// Directory names that mark everything beneath them as tests.
const TEST_DIRS: &[&str] = &[
    "test",
    "tests",
    "__tests__",
    "__mocks__",
    "spec",
    "specs",
    "e2e",
    "testdata",
    "fixtures",
];

/// Directory names that mark everything beneath them as documentation.
const DOC_DIRS: &[&str] = &["docs", "doc", "documentation"];

const DOC_EXTS: &[&str] = &["md", "mdx", "markdown", "rst", "adoc", "txt"];

const CONFIG_EXTS: &[&str] = &[
    "sql",
    "yaml",
    "yml",
    "json",
    "jsonc",
    "toml",
    "properties",
    "ini",
    "cfg",
    "conf",
    // Maven `pom.xml`, JPA `persistence.xml`, Spring and Android resources:
    // XML in a source tree is configuration far more often than code.
    "xml",
];

/// Build and dependency manifests whose extension says "prose" or nothing.
const CONFIG_NAMES: &[&str] = &["cmakelists.txt", "constraints.txt"];

/// Extensions of languages where `FooTest.ext` / `FooTests.ext` / `FooIT.ext`
/// is the test-naming convention (JUnit, xUnit, ScalaTest, PHPUnit...).
const SUFFIX_TEST_EXTS: &[&str] = &["java", "kt", "kts", "scala", "groovy", "cs", "php", "swift"];

/// Classify `path` (repo-relative, `/` or `\` separated). `language` is the
/// indexed language name, used only when the extension says nothing.
///
/// Precedence: Test, then Doc, then Config, else Code. A `tests/README.md` is a
/// test file for ranking purposes: it is fixture material, not the project's
/// documentation. An empty path (a memory row, which has no file) is `Code`:
/// nothing about it is test, doc or config noise.
pub fn path_kind(path: &str, language: &str) -> PathKind {
    if path.is_empty() {
        return PathKind::Code;
    }
    let norm = path.replace('\\', "/");
    let mut parts: Vec<&str> = norm.split('/').filter(|p| !p.is_empty()).collect();
    let Some(name) = parts.pop() else {
        return PathKind::Code;
    };
    let dirs = parts;
    let lname = name.to_ascii_lowercase();
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], name[i + 1..].to_ascii_lowercase()),
        _ => (name, String::new()),
    };

    let in_dir = |set: &[&str]| {
        dirs.iter()
            .any(|d| set.contains(&d.to_ascii_lowercase().as_str()))
    };

    // Test
    let lstem = stem.to_ascii_lowercase();
    // Go / Rust / Python / generic: `foo_test.go`, `test_foo.py`, `foo.test.ts`,
    // `foo.spec.ts`, `conftest.py`.
    let by_name = lstem.ends_with("_test")
        || lstem.ends_with("_tests")
        || lstem.starts_with("test_")
        || lstem.ends_with(".test")
        || lstem.ends_with(".spec")
        // Cypress: `login.cy.ts` is a test wherever it lives.
        || lstem.ends_with(".cy")
        || lname == "conftest.py"
        // JUnit and friends: the capital T keeps `Contest.java` and `Latest.cs` out.
        || (SUFFIX_TEST_EXTS.contains(&ext.as_str())
            && (stem.ends_with("Test") || stem.ends_with("Tests") || stem.ends_with("IT"))
            && stem.len() > 2);
    if in_dir(TEST_DIRS) || by_name {
        return PathKind::Test;
    }

    // Manifests named for their role (`requirements.txt`, `CMakeLists.txt`)
    // are configuration even though `.txt` would make them prose.
    if CONFIG_NAMES.contains(&lname.as_str()) || (lstem.starts_with("requirements") && ext == "txt")
    {
        return PathKind::Config;
    }

    // Doc
    if in_dir(DOC_DIRS)
        || DOC_EXTS.contains(&ext.as_str())
        || lstem.starts_with("readme")
        || ["changelog", "license", "contributing", "authors"].contains(&lstem.as_str())
        || matches!(language, "markdown" | "text")
    {
        return PathKind::Doc;
    }

    // Config
    if CONFIG_EXTS.contains(&ext.as_str())
        || matches!(
            language,
            "sql" | "yaml" | "json" | "toml" | "properties" | "ini"
        )
    {
        return PathKind::Config;
    }

    PathKind::Code
}

/// How far a hit is demoted by the kind of file it is in, so noise ranks lower
/// without disappearing. `1.0` disables a penalty; `0.0` buries the kind at
/// the end of the list (use the `kind` / `include_tests` filters to exclude
/// instead).
///
/// The factor is applied to the hit's *rank*, not its score: a penalised hit
/// is placed [`demotion`](Self::demotion) positions lower than it ranked. A
/// score multiplier meant something different in every mode — cosines cluster
/// around 0.6–0.85, so `× 0.6` turned 0.85 into 0.51, below nearly all code
/// (an exclusion in all but name); RRF scores span a few thousandths; a
/// cross-encoder's logits have no fixed scale at all. Positions are the one
/// unit every mode shares (RRF itself fuses by rank for the same reason).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KindPenalty {
    /// Factor for [`PathKind::Test`].
    #[serde(default = "default_penalty")]
    pub test: f32,
    /// Factor for [`PathKind::Doc`].
    #[serde(default = "default_penalty")]
    pub doc: f32,
    /// Factor for [`PathKind::Config`].
    #[serde(default = "default_penalty")]
    pub config: f32,
}

fn default_penalty() -> f32 {
    0.6
}

impl Default for KindPenalty {
    fn default() -> Self {
        KindPenalty {
            test: default_penalty(),
            doc: default_penalty(),
            config: default_penalty(),
        }
    }
}

impl KindPenalty {
    /// No penalty for any kind.
    pub const NONE: KindPenalty = KindPenalty {
        test: 1.0,
        doc: 1.0,
        config: 1.0,
    };

    /// Positions per unit of `1 − factor`: `0.6` demotes by 4, `0.9` by 1.
    pub const POSITIONS: f32 = 10.0;

    /// How many positions a hit of `kind` is moved down: `round((1 − f) ×
    /// POSITIONS)`, `0` for code or a factor of `1.0` or more, and `None` for
    /// a factor of `0.0` or less (bury it after every unpenalised hit).
    pub fn demotion(&self, kind: PathKind) -> Option<usize> {
        let f = self.factor(kind);
        if f.is_nan() || f >= 1.0 {
            return Some(0);
        }
        if f <= 0.0 {
            return None;
        }
        Some(((1.0 - f) * Self::POSITIONS).round() as usize)
    }

    /// What is wrong with these factors, in words for whoever wrote the config.
    /// A factor above `1.0` reads like a boost, and there is none: the penalty
    /// only ever demotes, so it is treated as `1.0` (fixup G).
    pub fn warnings(&self) -> Vec<String> {
        [
            ("test", self.test),
            ("doc", self.doc),
            ("config", self.config),
        ]
        .iter()
        .filter(|(_, f)| *f > 1.0)
        .map(|(k, f)| {
            format!(
                "search.penalty.{k} = {f} is above 1.0: the penalty only demotes, there is no \
                 boost, so it is treated as 1.0 (no penalty)"
            )
        })
        .collect()
    }

    /// The factor for `kind` (`Code` is always `1.0`).
    pub fn factor(&self, kind: PathKind) -> f32 {
        match kind {
            PathKind::Code => 1.0,
            PathKind::Test => self.test,
            PathKind::Doc => self.doc,
            PathKind::Config => self.config,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(p: &str) -> PathKind {
        path_kind(p, "")
    }

    #[test]
    fn tests_in_java_ts_python_rust_go() {
        for p in [
            "src/test/java/com/acme/FooServiceTest.java",
            "src/main/java/com/acme/FooServiceTests.java",
            "app/FooIT.java",
            "src/app/foo.component.spec.ts",
            "src/app/foo.test.tsx",
            "web/__tests__/foo.ts",
            "pkg/foo/foo_test.go",
            "crates/x/tests/integration.rs",
            "tests/test_models.py",
            "pkg/test_models.py",
            "pkg/conftest.py",
            "spec/models/user_spec.rb",
            "Tests/Unit/FooTests.cs",
            "e2e/login.ts",
            "src/app/login.cy.ts",
            "cypress/login.cy.js",
        ] {
            assert_eq!(k(p), PathKind::Test, "{p}");
        }
    }

    #[test]
    fn docs() {
        for p in [
            "README.md",
            "readme.txt",
            "crates/x/README",
            "docs/architecture.md",
            "docs/guide/setup.html",
            "CHANGELOG.md",
            "notes/design.rst",
        ] {
            assert_eq!(k(p), PathKind::Doc, "{p}");
        }
    }

    #[test]
    fn config_and_data() {
        for p in [
            "db/migrations/001_init.sql",
            "src/main/resources/application.properties",
            "config/app.yaml",
            ".github/workflows/ci.yml",
            "package.json",
            "Cargo.toml",
            "requirements.txt",
            "backend/requirements-dev.txt",
            "CMakeLists.txt",
            "pom.xml",
            "src/main/resources/META-INF/persistence.xml",
        ] {
            assert_eq!(k(p), PathKind::Config, "{p}");
        }
    }

    #[test]
    fn code_is_everything_else_and_lookalikes_stay_code() {
        for p in [
            "src/main/java/com/acme/FooService.java",
            "src/app/foo.component.ts",
            "crates/devctx-core/src/lib.rs",
            "pkg/attest.go",
            "src/Contest.java",
            "src/Latest.cs",
            "src/testing_utils.py",
            "src/contest_results.py",
            "Makefile",
            "src/prod/main.py",
        ] {
            assert_eq!(k(p), PathKind::Code, "{p}");
        }
    }

    #[test]
    fn test_directories_beat_extensions_and_windows_separators_work() {
        assert_eq!(k("src/test/resources/data.sql"), PathKind::Test);
        assert_eq!(k("tests/README.md"), PathKind::Test);
        assert_eq!(k(r"src\test\java\FooTest.java"), PathKind::Test);
        assert_eq!(k("docs/config.yaml"), PathKind::Doc);
    }

    #[test]
    fn language_decides_only_when_the_name_is_silent() {
        assert_eq!(path_kind("schema", "sql"), PathKind::Config);
        assert_eq!(path_kind("NOTES", "markdown"), PathKind::Doc);
        assert_eq!(path_kind("src/lib.rs", "rust"), PathKind::Code);
        assert_eq!(
            path_kind("", "rust"),
            PathKind::Code,
            "memories have no file"
        );
    }

    #[test]
    fn parse_and_factor() {
        assert_eq!(PathKind::parse("Test"), Some(PathKind::Test));
        assert_eq!(PathKind::parse("nope"), None);
        let p = KindPenalty::default();
        assert_eq!(p.factor(PathKind::Code), 1.0);
        assert!(p.factor(PathKind::Test) < 1.0);
        assert_eq!(KindPenalty::NONE.factor(PathKind::Doc), 1.0);
        assert_eq!(p.demotion(PathKind::Code), Some(0));
        assert_eq!(p.demotion(PathKind::Test), Some(4));
        let custom = KindPenalty {
            test: 0.0,
            doc: 0.9,
            config: 1.5,
        };
        assert_eq!(custom.demotion(PathKind::Test), None, "0.0 buries");
        assert_eq!(custom.demotion(PathKind::Doc), Some(1));
        assert_eq!(custom.demotion(PathKind::Config), Some(0));
    }
}
