//! Parser for the markdown plans under `plans/` (see `plans/PLAN-005-plan-status/`).
//!
//! Plans and tasks live in git as markdown, hand-written. This module turns that text into a
//! graph: pure parsing functions (`parse_plan_doc`, `parse_task_doc`) plus a thin disk-loading
//! layer (`load_plans`). No new dependencies: the format is parsed line by line.
//!
//! The parser is tolerant on purpose (PLAN-007): real plans write the status as `**Status:**`,
//! `Estado: x`, front matter or a `## Estado` section, with emoji and prose around the value.
//! It looks for the status where plans put it, but never guesses: a value with no recognizable
//! keyword stays `Unknown` and warns.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

/// Normalized task/plan status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "text")]
pub enum Status {
    Pending,
    InProgress,
    Done,
    Blocked,
    /// Dropped or superseded on purpose. It resolves dependencies like `Done`, but is reported
    /// apart: it was not done.
    Skipped,
    Unknown(String),
}

impl Status {
    pub fn is_done(&self) -> bool {
        matches!(self, Status::Done)
    }

    /// `Done` or `Skipped`: nothing is left to do, so a task depending on it is not waiting.
    pub fn is_resolved(&self) -> bool {
        matches!(self, Status::Done | Status::Skipped)
    }

    /// Parse a field value (`` `done` ``, `✅ Completed — …`, `Pendiente`) into a `Status`.
    /// A value with no recognizable keyword is `Unknown`.
    fn parse(raw: &str) -> Status {
        status_from_value(raw).unwrap_or_else(|| Status::Unknown(raw.trim().to_string()))
    }
}

/// A file reference mentioned in a task (inline code span containing a path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileRef {
    pub path: String,
    pub line: Option<u32>,
}

impl FileRef {
    /// Whether `self` matches a lookup `query`, per TASK-007's rule:
    /// - exact match, or
    /// - both have >= 2 path components and one is a component-wise suffix of the other.
    ///
    /// A bare single-component ref (e.g. `lib.rs`) only matches an identical bare query.
    pub fn matches(&self, query: &str) -> bool {
        if self.path == query {
            return true;
        }
        let a: Vec<&str> = self.path.split('/').collect();
        let b: Vec<&str> = query.split('/').collect();
        if a.len() < 2 || b.len() < 2 {
            return false;
        }
        let (shorter, longer) = if a.len() <= b.len() {
            (&a, &b)
        } else {
            (&b, &a)
        };
        longer.ends_with(shorter.as_slice())
    }
}

/// A single task within a plan.
#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub plan_id: String,
    pub status: Status,
    pub depends_on: Vec<String>,
    /// Dependencies on another plan's tasks (`PLAN-120/TASK-031`). Reported only: they neither
    /// block nor count as missing, since resolving them would mean loading the other plan.
    pub external_deps: Vec<String>,
    pub files: Vec<FileRef>,
    pub path: PathBuf,
}

/// A plan: its doc, its tasks (if any), and the table of task statuses in the plan doc (used
/// only to detect disagreement with the task files — never to override them).
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub id: String,
    pub title: String,
    pub dir: PathBuf,
    pub doc_path: Option<PathBuf>,
    pub tasks: Vec<Task>,
    pub table_status: BTreeMap<String, Status>,
    #[serde(skip)]
    pub mtime: Option<SystemTime>,
    pub warnings: Vec<String>,
}

/// Result of analyzing a plan's dependency graph.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Analysis {
    pub ready: Vec<String>,
    pub in_progress: Vec<String>,
    /// (task id, pending dependency ids)
    pub blocked: Vec<(String, Vec<String>)>,
    /// (task id, missing dependency id)
    pub missing: Vec<(String, String)>,
    pub cycles: Vec<Vec<String>>,
}

fn lowered_no_accents(s: &str) -> String {
    s.to_lowercase()
        .replace('á', "a")
        .replace('é', "e")
        .replace('í', "i")
        .replace('ó', "o")
        .replace('ú', "u")
}

fn strip_accents_lower(s: &str) -> String {
    lowered_no_accents(s)
}

/// Words of a status value, lowercase and without accents. Emoji, `*`, backticks and
/// punctuation split words; `_` and `-` stay inside them (`in_progress`, `in-progress`).
fn value_words(s: &str) -> Vec<String> {
    lowered_no_accents(s)
        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .map(|w| w.trim_matches('-'))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect()
}

/// The status a single word names (ES + EN synonyms), if any.
fn keyword_status(word: &str) -> Option<Status> {
    match word {
        "pending" | "pendiente" | "todo" => Some(Status::Pending),
        "in_progress" | "in-progress" | "inprogress" | "wip" => Some(Status::InProgress),
        "done" | "completed" | "complete" | "completada" | "completado" | "hecho" | "hecha"
        | "terminada" | "terminado" | "cerrada" | "cerrado" | "implemented" | "implementada"
        | "implementado" | "ejecutada" | "ejecutado" | "merged" => Some(Status::Done),
        "blocked" | "bloqueada" | "bloqueado" => Some(Status::Blocked),
        "skipped" | "omitida" | "omitido" | "descartada" | "descartado" | "cancelled"
        | "canceled" | "cancelada" | "cancelado" | "superseded" | "postponed" | "pospuesta"
        | "pospuesto" => Some(Status::Skipped),
        _ => None,
    }
}

/// The status named at `words[i]`, with how many words it used (`en progreso` is two).
fn status_at(words: &[String], i: usize) -> Option<(Status, usize)> {
    let next = words.get(i + 1).map(|w| w.as_str());
    match (words[i].as_str(), next) {
        ("en", Some("progreso" | "curso")) | ("in", Some("progress")) => {
            Some((Status::InProgress, 2))
        }
        (w, _) => keyword_status(w).map(|s| (s, 1)),
    }
}

/// Reads a status out of a field value, or `None` when it has no recognizable keyword.
/// A backtick span that is exactly a keyword wins over the prose around it; otherwise the first
/// keyword of the text wins; a bare `✅` means done.
fn status_from_value(raw: &str) -> Option<Status> {
    for (idx, span) in raw.split('`').enumerate() {
        if idx % 2 == 0 {
            continue;
        }
        let words = value_words(span);
        if words.is_empty() {
            continue;
        }
        if let Some((status, used)) = status_at(&words, 0) {
            if used == words.len() {
                return Some(status);
            }
        }
    }
    let words = value_words(raw);
    for i in 0..words.len() {
        if let Some((status, _)) = status_at(&words, i) {
            return Some(status);
        }
    }
    if raw.contains('✅') {
        return Some(Status::Done);
    }
    None
}

/// Parses one header field like `- **Campo:** valor`, `**Campo**: valor`, `**Campo: valor**` or
/// a plain `Campo: valor`. Returns `(field_name_lowercase_no_accents, value)`; the name is
/// normalized (`depends_on` → `depends on`) so callers compare it exactly.
fn parse_field_line(segment: &str) -> Option<(String, String)> {
    let trimmed = segment.trim();
    let trimmed = trimmed
        .strip_prefix('-')
        .or_else(|| trimmed.strip_prefix("* "))
        .map(|s| s.trim())
        .unwrap_or(trimmed);
    if trimmed.starts_with('#') || trimmed.starts_with('|') {
        return None;
    }
    let (name, value) = if let Some(rest) = trimmed.strip_prefix("**") {
        let end = rest.find("**")?;
        let field_raw = &rest[..end];
        let after = &rest[end + 2..];
        if let Some(name) = field_raw.strip_suffix(':') {
            // "**Campo:** valor"
            (name, after.trim_start_matches(':').trim())
        } else if let Some(value) = after.strip_prefix(':') {
            // "**Campo**: valor"
            (field_raw, value.trim())
        } else {
            // "**Campo: valor**": the colon is inside the bold, with the value.
            field_raw.split_once(':')?
        }
    } else {
        // "Campo: valor" with no bold. Keep it to short plain names so prose with a colon in
        // it is not mistaken for a field.
        let (name, value) = trimmed.split_once(':')?;
        if name.is_empty() || name.len() > 30 || name.contains(['*', '`', '|']) {
            return None;
        }
        (name, value.trim())
    };
    let key = strip_accents_lower(name.trim().trim_matches(|c: char| c == '*' || c == '_'))
        .replace('_', " ");
    Some((key, value.trim().to_string()))
}

/// The fields of one header line. A line may carry several, separated by ` · `
/// (`**Plan:** PLAN-111 · **Estado:** PENDIENTE · **Depende de:** —`).
fn field_segments(line: &str) -> Vec<(String, String)> {
    line.split(" · ").filter_map(parse_field_line).collect()
}

/// Splits leading YAML front matter (`---` … `---` at the very top) from the rest of the text.
fn split_front_matter(text: &str) -> (Option<&str>, &str) {
    let mut lines = text.split_inclusive('\n');
    let Some(first) = lines.next() else {
        return (None, text);
    };
    if first.trim() != "---" {
        return (None, text);
    }
    let mut offset = first.len();
    for line in lines {
        if line.trim() == "---" {
            return (
                Some(&text[first.len()..offset]),
                &text[offset + line.len()..],
            );
        }
        offset += line.len();
    }
    (None, text)
}

/// Reads a top-level `key: value` from front matter, matching `keys` exactly after
/// normalization. An empty value followed by a `- item` block yields the items joined by `, `.
fn front_matter_value(front: &str, keys: &[&str]) -> Option<String> {
    let lines: Vec<&str> = front.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = strip_accents_lower(k.trim()).replace('_', " ");
        if !keys.contains(&key.as_str()) {
            continue;
        }
        let value = v
            .trim()
            .trim_matches(|c: char| c == '"' || c == '\'')
            .trim();
        if !value.is_empty() {
            return Some(value.to_string());
        }
        let items: Vec<String> = lines[idx + 1..]
            .iter()
            .take_while(|l| l.trim_start().starts_with('-'))
            .map(|l| l.trim_start().trim_start_matches('-').trim().to_string())
            .collect();
        if !items.is_empty() {
            return Some(items.join(", "));
        }
    }
    None
}

/// First non-empty line under a `## Estado` / `## Status` heading (exact: `## Estado final`
/// does not count).
fn section_status_value(body: &str) -> Option<String> {
    let mut in_section = false;
    for line in body.lines() {
        let t = line.trim();
        if let Some(heading) = t.strip_prefix("## ") {
            if in_section {
                return None;
            }
            let h = strip_accents_lower(heading.trim().trim_end_matches(':'));
            in_section = matches!(h.as_str(), "estado" | "status" | "state");
        } else if in_section && !t.is_empty() {
            if t.starts_with('#') {
                return None;
            }
            return Some(t.to_string());
        }
    }
    None
}

/// Extracts the header block: lines between the first H1 (`# `) and the first `## ` section,
/// after any front matter. Without an H1 it starts right after the front matter.
fn header_block(text: &str) -> &str {
    let (_, body) = split_front_matter(text);
    let mut start = 0usize;
    let mut end = body.len();
    let mut offset = 0usize;
    let mut found_h1 = false;
    for line in body.split_inclusive('\n') {
        let t = line.trim_start();
        if !found_h1 && t.starts_with("# ") {
            found_h1 = true;
            start = offset + line.len();
        } else if t.starts_with("## ") {
            end = offset;
            break;
        }
        offset += line.len();
    }
    &body[start.min(end)..end]
}

/// Extracts the H1 title (text after `# ` or `# PREFIX — `).
fn h1_title(text: &str) -> Option<String> {
    let (_, body) = split_front_matter(text);
    for line in body.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("# ") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Header keys that carry a task's dependencies (normalized: lowercase, no accents).
const DEPENDS_KEYS: &[&str] = &[
    "depende de",
    "depends on",
    "dependencies",
    "dependencias",
    "bloqueada por",
    "blocked by",
];

/// Whether a `Depende de` value says "none": empty, a dash, `ninguna`, `none`, `n/a`, `[]`.
fn is_none_mark(value: &str) -> bool {
    let t = value.trim_start_matches(['`', '*', ' ']);
    if t.is_empty() {
        return true;
    }
    let lowered = strip_accents_lower(t);
    if lowered.starts_with(['—', '–', '-']) || lowered.starts_with("[]") {
        return true;
    }
    ["ninguna", "ninguno", "none", "n/a"].iter().any(|w| {
        lowered
            .strip_prefix(*w)
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric()))
    })
}

/// `001, 002` — a value that is only a list of numbers (bare numbers never count inside prose).
fn bare_number_list(value: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for tok in value.split(|c: char| c == ',' || c.is_whitespace()) {
        let tok = tok.trim_matches(|c: char| matches!(c, '(' | ')' | '.' | '`' | ';'));
        if tok.is_empty() {
            continue;
        }
        if matches!(strip_accents_lower(tok).as_str(), "y" | "and" | "e") {
            continue;
        }
        if !tok.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let n: u32 = tok.parse().ok()?;
        if n == 0 {
            return None;
        }
        out.push(format!("TASK-{n:03}"));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Parses a task id that starts `s` (the text right after `TASK-`, already uppercase):
/// `001`, `1`, `001B`, `R01`, `DB-001`. Returns the normalized id and the bytes consumed.
/// A numeric id is padded to 3 digits and keeps a lowercase letter suffix (`TASK-001b` is not
/// `TASK-001`); a prefixed id (`R01`, `DB-001`) keeps its digits as written.
fn parse_id_at(s: &str) -> Option<(String, usize)> {
    let b = s.as_bytes();
    let mut i = 0usize;
    let mut prefix = "";
    if b.first().is_some_and(|c| c.is_ascii_alphabetic()) {
        while i < b.len() && i < 3 && b[i].is_ascii_alphabetic() {
            i += 1;
        }
        let mut j = i;
        if j < b.len() && b[j] == b'-' {
            j += 1;
        }
        if !b.get(j).is_some_and(|c| c.is_ascii_digit()) {
            return None;
        }
        prefix = &s[..j];
        i = j;
    }
    let digits_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits_start {
        return None;
    }
    let digits = &s[digits_start..i];
    if !prefix.is_empty() {
        return Some((format!("TASK-{prefix}{digits}"), i));
    }
    let n: u32 = digits.parse().ok()?;
    let mut id = format!("TASK-{n:03}");
    // One letter right after the digits is a suffix only when nothing alphanumeric follows it
    // (`001b-algo` yes, `001DESIGN` no).
    if b.get(i).is_some_and(|c| c.is_ascii_alphabetic())
        && !b.get(i + 1).is_some_and(|c| c.is_ascii_alphanumeric())
    {
        id.push(b[i].to_ascii_lowercase() as char);
        i += 1;
    }
    Some((id, i))
}

/// A plain numeric id (`TASK-007`, no suffix or prefix) as its number.
fn plain_number(id: &str) -> Option<u32> {
    id.strip_prefix("TASK-")
        .filter(|d| d.chars().all(|c| c.is_ascii_digit()))
        .and_then(|d| d.parse().ok())
}

/// Finds every task reference in `value`, ignoring the prose around them. Returns
/// `(local, external)`: ids of this plan, and `PLAN-<n>/TASK-<id>` references to other plans.
/// Expands `TASK-001..004`, `TASK-012 … TASK-018` (capped at 50) and `TASK-033/034/035`.
fn scan_task_refs(value: &str, plan_num: Option<u32>) -> (Vec<String>, Vec<String>) {
    let up = value.to_ascii_uppercase();
    let mut local: Vec<String> = Vec::new();
    let mut external: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < up.len() {
        if !up.is_char_boundary(i) {
            i += 1;
            continue;
        }
        let rest = &up[i..];
        let boundary = up[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        if boundary && rest.starts_with("PLAN-") {
            let digits: String = rest[5..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if !digits.is_empty() {
                let after = &rest[5 + digits.len()..];
                let skipped = after.trim_start_matches(['`', ' ', '/', ':']);
                let same_plan = plan_num.is_some() && digits.parse::<u32>().ok() == plan_num;
                if let Some((id, used)) = skipped.strip_prefix("TASK-").and_then(parse_id_at) {
                    if same_plan {
                        local.push(id);
                    } else {
                        external.push(format!("PLAN-{digits}/{id}"));
                    }
                    i += (rest.len() - skipped.len()) + 5 + used;
                } else {
                    if !same_plan {
                        external.push(format!("PLAN-{digits}"));
                    }
                    i += 5 + digits.len();
                }
                continue;
            }
        }
        if boundary {
            if let Some((id, used)) = rest.strip_prefix("TASK-").and_then(parse_id_at) {
                let mut tail = &rest[5 + used..];
                let mut last = plain_number(&id);
                local.push(id);
                // TASK-033/034/035
                while let Some(after_slash) = tail.strip_prefix('/') {
                    let digits: String = after_slash
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    let Ok(n) = digits.parse::<u32>() else { break };
                    if last.is_none() {
                        break;
                    }
                    local.push(format!("TASK-{n:03}"));
                    last = Some(n);
                    tail = &after_slash[digits.len()..];
                }
                // TASK-001..004, TASK-012 … TASK-018
                let t = tail.trim_start();
                let after_op = t
                    .strip_prefix("...")
                    .or_else(|| t.strip_prefix(".."))
                    .or_else(|| t.strip_prefix('…'));
                if let (Some(start), Some(after_op)) = (last, after_op) {
                    let r = after_op.trim_start();
                    let r = r.strip_prefix("TASK-").unwrap_or(r);
                    let digits: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
                    if let Ok(end) = digits.parse::<u32>() {
                        if end > start {
                            local
                                .extend((start + 1..=end).take(50).map(|n| format!("TASK-{n:03}")));
                        }
                        tail = &r[digits.len()..];
                    }
                }
                i = up.len() - tail.len();
                continue;
            }
        }
        i += rest.chars().next().map_or(1, |c| c.len_utf8());
    }
    (local, external)
}

/// Parses the `Depende de` field value. Returns `(deps, external_deps, warning)`.
///
/// A "none" mark at the start (`—`, `ninguna`, `none`, `n/a`, `[]`) means no dependencies, even if
/// prose after it mentions tasks. Otherwise only `TASK-<id>` tokens count; prose is ignored.
/// A value with no id, no cross-plan reference and no "none" mark gets one warning.
fn parse_depends_on(
    raw: &str,
    self_id: &str,
    plan_id: &str,
) -> (Vec<String>, Vec<String>, Option<String>) {
    let trimmed = raw.trim();
    if is_none_mark(trimmed) {
        return (Vec::new(), Vec::new(), None);
    }
    let plan_num = plan_id
        .strip_prefix("PLAN-")
        .and_then(|d| d.parse::<u32>().ok());
    let (found, mut external) = match bare_number_list(trimmed) {
        Some(ids) => (ids, Vec::new()),
        None => scan_task_refs(trimmed, plan_num),
    };
    let saw_ids = !found.is_empty() || !external.is_empty();
    let mut deps: Vec<String> = Vec::new();
    for id in found {
        // A self-dependency is dropped (no warning); so are repeats.
        if id != self_id && !deps.contains(&id) {
            deps.push(id);
        }
    }
    let mut seen = HashSet::new();
    external.retain(|e| seen.insert(e.clone()));
    let warning = if saw_ids {
        None
    } else {
        let shown: String = trimmed.chars().take(60).collect();
        Some(format!("Depende de sin TASK reconocible '{shown}'"))
    };
    (deps, external, warning)
}

/// Normalizes a table cell like `TASK-001`, `~~TASK-001~~`, `TASK-001b` or a bare `001` into a
/// task id (see [`parse_id_at`]).
fn normalize_task_id(tok: &str) -> Option<String> {
    let t = tok.trim_matches(|c: char| matches!(c, '~' | '*' | '`' | ' '));
    let up = t.to_ascii_uppercase();
    if let Some(rest) = up.strip_prefix("TASK-") {
        return parse_id_at(rest).map(|(id, _)| id);
    }
    // bare number, only if the whole token is digits
    if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) {
        let n: u32 = t.parse().ok()?;
        return Some(format!("TASK-{n:03}"));
    }
    None
}

/// Known file extensions recognized when scanning inline code spans for file references.
const KNOWN_EXTS: &[&str] = &[
    "rs", "md", "toml", "yaml", "yml", "json", "html", "js", "ts", "sh",
];

/// Extracts file references (`FileRef`) from inline single-backtick code spans, skipping any
/// text inside triple-backtick fenced blocks.
fn extract_files(text: &str) -> Vec<FileRef> {
    let mut in_fence = false;
    let mut refs: Vec<FileRef> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        let spans: Vec<&str> = line
            .split('`')
            .enumerate()
            .filter_map(|(idx, part)| if idx % 2 == 1 { Some(part) } else { None })
            .collect();
        for span in spans {
            let candidate = span.trim();
            if candidate.is_empty() || candidate.contains(' ') {
                continue;
            }
            if candidate.starts_with("..") || candidate.starts_with("http") {
                continue;
            }
            let has_slash = candidate.contains('/');
            let ext_ok = candidate
                .rsplit_once('.')
                .map(|(_, ext)| {
                    let ext_only = ext.split(':').next().unwrap_or(ext);
                    KNOWN_EXTS.contains(&ext_only)
                })
                .unwrap_or(false);
            if !has_slash && !ext_ok {
                continue;
            }
            // Split off optional ":line"
            let (path_part, line_part) = match candidate.rsplit_once(':') {
                Some((p, l)) if l.chars().all(|c| c.is_ascii_digit()) && !l.is_empty() => {
                    (p.to_string(), l.parse::<u32>().ok())
                }
                _ => (candidate.to_string(), None),
            };
            if path_part.starts_with("..") || path_part.starts_with("http") {
                continue;
            }
            if seen.insert(path_part.clone()) {
                refs.push(FileRef {
                    path: path_part,
                    line: line_part,
                });
            }
        }
    }
    refs
}

/// Parses a task document's text into a `Task`. `id` and `plan_id` come from the caller (the
/// filename and the containing directory), since a task doc's H1 also carries the id but the
/// filename is the authoritative source of the id contract used by `depends_on`.
///
/// The status is looked up in order: a header field (`estado`/`status`/`state`), YAML front
/// matter, then a `## Estado` section. The dependencies come from a header field or front matter.
pub fn parse_task_doc(text: &str, id: &str, plan_id: &str, path: &Path) -> (Task, Vec<String>) {
    let mut warnings = Vec::new();
    let title_full = h1_title(text).unwrap_or_default();
    let title = strip_task_prefix(&title_full, id);

    let (front, body) = split_front_matter(text);
    let header = header_block(text);
    let mut status_raw: Option<String> = None;
    let mut depends_raw: Option<String> = None;
    for line in header.lines() {
        for (field, value) in field_segments(line) {
            match field.as_str() {
                "estado" | "status" | "state" => {
                    if status_raw.is_none() && !value.is_empty() {
                        status_raw = Some(value);
                    }
                }
                k if DEPENDS_KEYS.contains(&k) && depends_raw.is_none() => {
                    depends_raw = Some(value);
                }
                _ => {}
            }
        }
    }
    if let Some(front) = front {
        if status_raw.is_none() {
            status_raw = front_matter_value(front, &["estado", "status", "state"]);
        }
        if depends_raw.is_none() {
            depends_raw = front_matter_value(front, DEPENDS_KEYS);
        }
    }
    if status_raw.is_none() {
        status_raw = section_status_value(body);
    }

    let mut depends_on = Vec::new();
    let mut external_deps = Vec::new();
    if let Some(raw) = depends_raw {
        let (deps, external, warn) = parse_depends_on(&raw, id, plan_id);
        depends_on = deps;
        external_deps = external;
        if let Some(w) = warn {
            warnings.push(format!("{id}: {w}"));
        }
    }

    let status = match status_raw.map(|raw| Status::parse(&raw)) {
        Some(Status::Unknown(text)) => {
            warnings.push(format!("{id}: Estado no reconocido '{text}'"));
            Status::Unknown(text)
        }
        Some(s) => s,
        None => {
            warnings.push(format!("{id}: falta el campo Estado, se asume pending"));
            Status::Pending
        }
    };

    let files = extract_files(text);

    (
        Task {
            id: id.to_string(),
            title,
            plan_id: plan_id.to_string(),
            status,
            depends_on,
            external_deps,
            files,
            path: path.to_path_buf(),
        },
        warnings,
    )
}

fn strip_task_prefix(title: &str, id: &str) -> String {
    let t = title.trim();
    if let Some(rest) = t.strip_prefix(id) {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('—').or_else(|| rest.strip_prefix('-')) {
            return rest.trim().to_string();
        }
        return rest.to_string();
    }
    t.to_string()
}

/// Whether a table header cell names the status column (`Estado`, `Status`, `State`).
fn is_status_header(cell: &str) -> bool {
    let c = cell.trim_matches(|c: char| c == '*' || c == '`' || c == ' ');
    matches!(
        strip_accents_lower(c).as_str(),
        "estado" | "status" | "state"
    )
}

/// Parses a plan doc's text (the table of tasks) into `table_status`, keyed by normalized task
/// id. Only a table whose header row has an `Estado`/`Status`/`State` column counts, and the
/// status is read from that column (a rollback table with ids in the first column is ignored).
/// A struck-through id (`~~TASK-011~~`) with no readable status is `Skipped`; a cell with no
/// keyword is prose, not a status, so it adds nothing.
fn parse_table_status(text: &str) -> BTreeMap<String, Status> {
    let mut map = BTreeMap::new();
    // None: outside a table. Some(None): inside one with no status column. Some(Some(i)): the
    // index of the status column.
    let mut table: Option<Option<usize>> = None;
    for line in text.lines() {
        let t = line.trim();
        if !t.starts_with('|') {
            table = None;
            continue;
        }
        let cells: Vec<&str> = t.trim_matches('|').split('|').map(|c| c.trim()).collect();
        let Some(column) = table else {
            // First row of a table: the header.
            table = Some(cells.iter().position(|c| is_status_header(c)));
            continue;
        };
        let Some(col) = column else {
            continue;
        };
        if cells
            .iter()
            .all(|c| c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')))
        {
            continue; // the `|---|---|` separator
        }
        let Some(id) = cells.first().and_then(|c| normalize_task_id(c)) else {
            continue;
        };
        let struck = cells[0].contains("~~");
        let cell = cells.get(col).copied().unwrap_or("");
        let status = match status_from_value(cell) {
            Some(s) => s,
            None if struck => Status::Skipped,
            None => continue,
        };
        map.insert(id, status);
    }
    map
}

/// Extracts a plan id (`PLAN-NNN`) from a directory name like `PLAN-005-plan-status`.
pub fn plan_id_from_dir(dir_name: &str) -> Option<String> {
    if !dir_name.to_uppercase().starts_with("PLAN-") {
        return None;
    }
    let rest = &dir_name[5..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    Some(format!("PLAN-{digits}"))
}

/// Where [`resolve_plans_root`] found the directory that holds `plans/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlansRootSource {
    /// The workspace root: where a group descent started, or the cwd outside any project.
    Workspace,
    /// The project's own root.
    Project,
    /// An ancestor of a project that declares `group:` (the workspace it lives in).
    Ancestor,
    /// Nowhere: the root is only the fallback, and the plan list will be empty.
    None,
}

impl PlansRootSource {
    pub fn as_str(self) -> &'static str {
        match self {
            PlansRootSource::Workspace => "workspace",
            PlansRootSource::Project => "project",
            PlansRootSource::Ancestor => "ancestor",
            PlansRootSource::None => "none",
        }
    }
}

/// The directory whose `plans/` is read, and why that one (PLAN-007 DD-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlansRoot {
    pub root: PathBuf,
    pub source: PlansRootSource,
}

/// Whether `dir/plans/` holds at least one `PLAN-<n>*` directory.
fn has_plan_dirs(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir.join("plans")) else {
        return false;
    };
    entries
        .filter_map(|e| e.ok())
        .any(|e| e.path().is_dir() && plan_id_from_dir(&e.file_name().to_string_lossy()).is_some())
}

/// Decides which directory to read `plans/` from. Pure apart from `is_dir`/`read_dir` probes.
/// In order:
/// 1. `workspace` (the root of a group descent, or the cwd outside any project) if it has `plans/`;
/// 2. `project_root` if it has `plans/`;
/// 3. when the project declares `group:`, the nearest ancestor of `project_root` that is strictly
///    below `home` and has `plans/` with at least one `PLAN-<n>*` — a member of a workspace
///    reads the plans kept at the workspace root. Without `home` this step never runs: an
///    unbounded walk could adopt somebody else's `plans/`;
/// 4. otherwise `project_root` (or `workspace`), as before: an empty list.
pub fn resolve_plans_root(
    project_root: Option<&Path>,
    workspace: Option<&Path>,
    group_declared: bool,
    home: Option<&Path>,
) -> PlansRoot {
    if let Some(ws) = workspace {
        if ws.join("plans").is_dir() {
            return PlansRoot {
                root: ws.to_path_buf(),
                source: PlansRootSource::Workspace,
            };
        }
    }
    if let Some(p) = project_root {
        if p.join("plans").is_dir() {
            return PlansRoot {
                root: p.to_path_buf(),
                source: PlansRootSource::Project,
            };
        }
        if let (true, Some(home)) = (group_declared, home) {
            for anc in p.ancestors().skip(1) {
                if anc == home || !anc.starts_with(home) {
                    break;
                }
                if has_plan_dirs(anc) {
                    return PlansRoot {
                        root: anc.to_path_buf(),
                        source: PlansRootSource::Ancestor,
                    };
                }
            }
        }
    }
    PlansRoot {
        root: project_root
            .or(workspace)
            .map(Path::to_path_buf)
            .unwrap_or_default(),
        source: PlansRootSource::None,
    }
}

fn file_name_of(p: &Path) -> &str {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

/// Whether a file name looks like a plan doc: `PLAN-<n>…`.
fn is_plan_doc_name(name: &str) -> bool {
    name.get(..5)
        .is_some_and(|p| p.eq_ignore_ascii_case("PLAN-"))
        && name[5..].starts_with(|c: char| c.is_ascii_digit())
}

/// Picks the plan doc among the `.md` files directly in `dir` (not `tasks/`): prefer
/// `<dir-name>.md`; else the `PLAN-<n>*.md` candidates not ending in `-design.md`; only if there
/// are none, any `.md` not ending in `-design.md`. If the chosen tier has several, warn and take
/// the first in sorted order.
fn pick_plan_doc(
    dir: &Path,
    dir_name: &str,
    candidates: &[PathBuf],
) -> (Option<PathBuf>, Option<String>) {
    let preferred = dir.join(format!("{dir_name}.md"));
    if candidates.contains(&preferred) {
        return (Some(preferred), None);
    }
    let not_design: Vec<&PathBuf> = candidates
        .iter()
        .filter(|p| !file_name_of(p).ends_with("-design.md"))
        .collect();
    let plan_named: Vec<&PathBuf> = not_design
        .iter()
        .copied()
        .filter(|p| is_plan_doc_name(file_name_of(p)))
        .collect();
    let mut sorted = if plan_named.is_empty() {
        not_design
    } else {
        plan_named
    };
    sorted.sort();
    if sorted.is_empty() {
        return (None, None);
    }
    if sorted.len() == 1 {
        return (Some(sorted[0].clone()), None);
    }
    let warning = Some(format!(
        "múltiples documentos de plan candidatos en {}: se eligió {}",
        dir.display(),
        sorted[0].display()
    ));
    (Some(sorted[0].clone()), warning)
}

/// Parses a plan document's text into `(title, table_status)`.
pub fn parse_plan_doc(text: &str) -> (String, BTreeMap<String, Status>) {
    let title_full = h1_title(text).unwrap_or_default();
    (title_full, parse_table_status(text))
}

/// Loads all plans under `root/plans/PLAN-*`.
pub fn load_plans(root: &Path) -> Vec<Plan> {
    let plans_dir = root.join("plans");
    if !plans_dir.is_dir() {
        return Vec::new();
    }
    let mut plans = Vec::new();
    let Ok(entries) = std::fs::read_dir(&plans_dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    for dir in dirs {
        let dir_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let Some(plan_id) = plan_id_from_dir(&dir_name) else {
            continue;
        };
        plans.push(load_plan(&dir, &dir_name, &plan_id));
    }
    plans
}

fn load_plan(dir: &Path, dir_name: &str, plan_id: &str) -> Plan {
    let mut warnings = Vec::new();
    let mut mtime: Option<SystemTime> = None;

    let mut md_candidates: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Ok(meta) = p.metadata() {
                    if let Ok(m) = meta.modified() {
                        mtime = Some(mtime.map_or(m, |cur| cur.max(m)));
                    }
                }
                md_candidates.push(p);
            }
        }
    }
    md_candidates.sort();

    let (doc_path, pick_warning) = pick_plan_doc(dir, dir_name, &md_candidates);
    if let Some(w) = pick_warning {
        warnings.push(w);
    }

    let mut title = dir_name.to_string();
    let mut table_status = BTreeMap::new();
    if let Some(doc) = &doc_path {
        match std::fs::read_to_string(doc) {
            Ok(text) => {
                let (t, table) = parse_plan_doc(&text);
                if !t.is_empty() {
                    title = t;
                }
                table_status = table;
            }
            Err(e) => warnings.push(format!("no se pudo leer {}: {e}", doc.display())),
        }
    }

    let mut tasks = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let tasks_dir = dir.join("tasks");
    if tasks_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&tasks_dir) {
            let mut files: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("md"))
                .collect();
            files.sort();
            for f in files {
                let name = f.file_stem().and_then(|n| n.to_str()).unwrap_or("");
                let Some(id) = extract_task_id_from_filename(name) else {
                    warnings.push(format!(
                        "nombre de archivo de task no reconocido: {}",
                        f.display()
                    ));
                    continue;
                };
                if !seen_ids.insert(id.clone()) {
                    // Companion docs (`TASK-001-DESIGN.md` next to `TASK-001-verify.md`) share an
                    // id: the first in sorted order is the task.
                    warnings.push(format!("id duplicado: {}", f.display()));
                    continue;
                }
                if let Ok(meta) = f.metadata() {
                    if let Ok(m) = meta.modified() {
                        mtime = Some(mtime.map_or(m, |cur| cur.max(m)));
                    }
                }
                match std::fs::read_to_string(&f) {
                    Ok(text) => {
                        let (task, task_warnings) = parse_task_doc(&text, &id, plan_id, &f);
                        warnings.extend(task_warnings);
                        tasks.push(task);
                    }
                    Err(e) => warnings.push(format!("no se pudo leer {}: {e}", f.display())),
                }
            }
        }
    }

    // Compare table_status vs task file status (TASK-001 Paso 9): task file wins, warn on
    // disagreement.
    for task in &tasks {
        if let Some(table_s) = table_status.get(&task.id) {
            if table_s != &task.status {
                warnings.push(format!(
                    "{}: tabla dice {}, archivo dice {}",
                    task.id,
                    status_label(table_s),
                    status_label(&task.status)
                ));
            }
        }
    }

    let title_final = h1_title_strip_plan_prefix(&title);

    Plan {
        id: plan_id.to_string(),
        title: title_final,
        dir: dir.to_path_buf(),
        doc_path,
        tasks,
        table_status,
        mtime,
        warnings,
    }
}

/// Strips a leading `PLAN-<n>` (any number, not only the directory's: PLAN-008's H1 reads
/// `PLAN-1:`) and the separator after it (`—`, `–`, `-`, `:` or spaces).
fn h1_title_strip_plan_prefix(title: &str) -> String {
    let t = title.trim();
    if t.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("PLAN-")) {
        let after = &t[5..];
        let digits = after.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 {
            return after[digits..]
                .trim_start_matches(|c: char| {
                    c.is_whitespace() || matches!(c, '—' | '–' | '-' | ':')
                })
                .trim()
                .to_string();
        }
    }
    t.to_string()
}

/// The word a caller sees for a `Status` (`"pending"`, `"in_progress"`, `"done"`, `"blocked"`,
/// `"skipped"`, or `"unknown(...)"`).
pub fn status_label(s: &Status) -> String {
    match s {
        Status::Pending => "pending".to_string(),
        Status::InProgress => "in_progress".to_string(),
        Status::Done => "done".to_string(),
        Status::Blocked => "blocked".to_string(),
        Status::Skipped => "skipped".to_string(),
        Status::Unknown(t) => format!("unknown({t})"),
    }
}

/// The task id a file name carries (`TASK-001-algo` → `TASK-001`, `TASK-001b-algo` →
/// `TASK-001b`, `TASK-R01-algo` → `TASK-R01`), or `None` for any other name.
fn extract_task_id_from_filename(stem: &str) -> Option<String> {
    let upper = stem.to_ascii_uppercase();
    let rest = upper.strip_prefix("TASK-")?;
    parse_id_at(rest).map(|(id, _)| id)
}

/// Pure analysis of a plan's dependency graph (TASK-001 Paso 11). A `Skipped` dependency counts
/// as resolved; `external_deps` are ignored (they neither block nor go to `missing`).
pub fn analyze(plan: &Plan) -> Analysis {
    let mut analysis = Analysis::default();
    let by_id: BTreeMap<&str, &Task> = plan.tasks.iter().map(|t| (t.id.as_str(), t)).collect();

    for task in &plan.tasks {
        match task.status {
            Status::Done | Status::Skipped => continue,
            Status::InProgress => analysis.in_progress.push(task.id.clone()),
            Status::Blocked => {
                let pending: Vec<String> = task.depends_on.clone();
                analysis.blocked.push((task.id.clone(), pending));
            }
            _ => {
                let mut pending_deps = Vec::new();
                for dep in &task.depends_on {
                    match by_id.get(dep.as_str()) {
                        None => {
                            analysis.missing.push((task.id.clone(), dep.clone()));
                            pending_deps.push(dep.clone());
                        }
                        Some(dep_task) => {
                            if !dep_task.status.is_resolved() {
                                pending_deps.push(dep.clone());
                            }
                        }
                    }
                }
                if pending_deps.is_empty() {
                    analysis.ready.push(task.id.clone());
                } else {
                    analysis.blocked.push((task.id.clone(), pending_deps));
                }
            }
        }
    }

    analysis.cycles = detect_cycles(plan);
    analysis
}

fn detect_cycles(plan: &Plan) -> Vec<Vec<String>> {
    #[derive(PartialEq, Clone, Copy)]
    enum Color {
        White,
        Gray,
        Black,
    }
    let by_id: BTreeMap<&str, &Task> = plan.tasks.iter().map(|t| (t.id.as_str(), t)).collect();
    let mut color: BTreeMap<&str, Color> = plan
        .tasks
        .iter()
        .map(|t| (t.id.as_str(), Color::White))
        .collect();
    let mut cycles = Vec::new();
    let mut stack: Vec<&str> = Vec::new();

    fn visit<'a>(
        id: &'a str,
        by_id: &BTreeMap<&'a str, &'a Task>,
        color: &mut BTreeMap<&'a str, Color>,
        stack: &mut Vec<&'a str>,
        cycles: &mut Vec<Vec<String>>,
    ) {
        color.insert(id, Color::Gray);
        stack.push(id);
        if let Some(task) = by_id.get(id) {
            for dep in &task.depends_on {
                let dep_id = dep.as_str();
                let Some(&dep_key) = by_id.keys().find(|k| **k == dep_id) else {
                    continue;
                };
                match color.get(dep_key) {
                    Some(Color::White) | None => visit(dep_key, by_id, color, stack, cycles),
                    Some(Color::Gray) => {
                        // found a cycle: slice the stack from dep_key's position
                        if let Some(pos) = stack.iter().position(|s| *s == dep_key) {
                            let mut cycle: Vec<String> =
                                stack[pos..].iter().map(|s| s.to_string()).collect();
                            cycle.push(dep_key.to_string());
                            cycles.push(cycle);
                        }
                    }
                    Some(Color::Black) => {}
                }
            }
        }
        stack.pop();
        color.insert(id, Color::Black);
    }

    let ids: Vec<&str> = plan.tasks.iter().map(|t| t.id.as_str()).collect();
    for id in ids {
        if color.get(id) == Some(&Color::White) {
            visit(id, &by_id, &mut color, &mut stack, &mut cycles);
        }
    }
    cycles
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn task(text: &str, id: &str, plan_id: &str) -> (Task, Vec<String>) {
        parse_task_doc(text, id, plan_id, &PathBuf::from("test.md"))
    }

    // --- Fixture: PLAN-002/003 header style ---
    const PLAN002_STYLE_TASK: &str = r#"# TASK-002 — `enum Binding` de primera clase (None / Project / Group)

- **Plan:** PLAN-002 — MCP resuelve el proyecto por ruta
- **Especialista:** —
- **Proyecto:** DevCtxEngine (`/home/snaven10/personal/DevCtxEngine`), rama `feature/mcp-auto-bind-por-path`
- **Depende de:** — (paralela a TASK-001)
- **Estado:** `done`

---

## Objetivo

Texto.
"#;

    #[test]
    fn plan002_style_header_parses_status_and_no_dependency() {
        let (t, warnings) = task(PLAN002_STYLE_TASK, "TASK-002", "PLAN-002");
        assert_eq!(t.status, Status::Done);
        assert!(
            t.depends_on.is_empty(),
            "— (paralela a TASK-001) debe leerse como sin dependencia"
        );
        assert!(warnings.is_empty());
    }

    // --- Fixture: PLAN-001 header style (no bullet, colon outside bold) ---
    const PLAN001_STYLE_TASK: &str = r#"# TASK-001 — Estado de progreso observable en `AppState`

**Plan**: PLAN-001
**Modelo sugerido**: sonnet
**Depende de**: —
**Estado**: done

## Objetivo

Texto.
"#;

    #[test]
    fn plan001_style_header_parses_without_bullets_or_backticks() {
        let (t, warnings) = task(PLAN001_STYLE_TASK, "TASK-001", "PLAN-001");
        assert_eq!(t.status, Status::Done);
        assert!(t.depends_on.is_empty());
        assert!(warnings.is_empty());
        assert_eq!(t.title, "Estado de progreso observable en `AppState`");
    }

    #[test]
    fn estado_with_trailing_commentary_is_ignored() {
        let text = r#"# TASK-001 — algo

**Estado**: done — con un riesgo aceptado que después explotó

## Objetivo
"#;
        let (t, _w) = task(text, "TASK-001", "PLAN-001");
        assert_eq!(t.status, Status::Done);
    }

    #[test]
    fn estado_final_in_resultado_section_does_not_override_header_estado() {
        let text = r#"# TASK-001 — algo

- **Estado:** `pending`

---

## Objetivo

Texto.

## Resultado

- **Estado final:** `done`
"#;
        let (t, _w) = task(text, "TASK-001", "PLAN-002");
        assert_eq!(t.status, Status::Pending);
    }

    #[test]
    fn missing_estado_field_defaults_to_pending_with_warning() {
        let text = r#"# TASK-001 — algo

- **Depende de:** —

## Objetivo
"#;
        let (t, warnings) = task(text, "TASK-001", "PLAN-002");
        assert_eq!(t.status, Status::Pending);
        assert!(warnings.iter().any(|w| w.contains("falta el campo Estado")));
    }

    #[test]
    fn unrecognized_estado_value_is_unknown_with_warning() {
        let text = r#"# TASK-001 — algo

- **Estado:** `en revision`

## Objetivo
"#;
        let (t, warnings) = task(text, "TASK-001", "PLAN-002");
        assert!(matches!(t.status, Status::Unknown(_)));
        assert!(!warnings.is_empty());
    }

    #[test]
    fn depende_de_range_expands() {
        let text = r#"# TASK-005 — algo

- **Depende de:** TASK-001..004

## Objetivo
"#;
        let (t, _w) = task(text, "TASK-005", "PLAN-002");
        assert_eq!(
            t.depends_on,
            vec!["TASK-001", "TASK-002", "TASK-003", "TASK-004"]
        );
    }

    #[test]
    fn depende_de_bare_numbers_normalize_to_task_ids() {
        let text = r#"# TASK-005 — algo

- **Depende de:** 001, 002

## Objetivo
"#;
        let (t, _w) = task(text, "TASK-005", "PLAN-002");
        assert_eq!(t.depends_on, vec!["TASK-001", "TASK-002"]);
    }

    #[test]
    fn ninguna_and_na_mean_no_dependency() {
        for value in ["Ninguna", "N/A", "n/a"] {
            let text = format!("# TASK-001 — algo\n\n- **Depende de:** {value}\n\n## Objetivo\n");
            let (t, _w) = task(&text, "TASK-001", "PLAN-002");
            assert!(t.depends_on.is_empty(), "{value} debe dar sin dependencias");
        }
    }

    #[test]
    fn files_are_extracted_from_inline_code_outside_fences() {
        let text = r#"# TASK-001 — algo

- **Estado:** `pending`

## Archivos

- **Modificar:** `crates/devctx-mcp/src/state.rs:161`

## Contexto

```
crates/devctx-mcp/src/state.rs:999 este no cuenta
```

Referencia pelada: `lib.rs` y relativa `../VERIFICACION.md` (excluida) y `state.rs` pelado.
"#;
        let (t, _w) = task(text, "TASK-001", "PLAN-002");
        let paths: Vec<&str> = t.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"crates/devctx-mcp/src/state.rs"));
        assert!(!paths.contains(&"crates/devctx-mcp/src/state.rs:999"));
        assert!(paths.contains(&"lib.rs"));
        assert!(paths.contains(&"state.rs"));
        assert!(!paths.iter().any(|p| p.starts_with("..")));
        let state_rs = t
            .files
            .iter()
            .find(|f| f.path == "crates/devctx-mcp/src/state.rs")
            .unwrap();
        assert_eq!(state_rs.line, Some(161));
    }

    #[test]
    fn fenced_block_paths_are_excluded() {
        let text = "# TASK-001 — algo\n\n```\n`crates/x/y.rs:42`\n```\n";
        let (t, _w) = task(text, "TASK-001", "PLAN-002");
        assert!(t.files.is_empty());
    }

    #[test]
    fn file_ref_matches_component_suffix_but_not_bare_lib_rs() {
        let a = FileRef {
            path: "crates/devctx-mcp/src/state.rs".to_string(),
            line: None,
        };
        assert!(a.matches("crates/devctx-mcp/src/state.rs"));
        assert!(a.matches("devctx-mcp/src/state.rs"));
        assert!(!a.matches("state.rs"));

        let bare = FileRef {
            path: "lib.rs".to_string(),
            line: None,
        };
        assert!(bare.matches("lib.rs"));
        assert!(!bare.matches("crates/devctx-mcp/src/lib.rs"));
    }

    #[test]
    fn plan_id_from_dir_handles_with_and_without_slug() {
        assert_eq!(plan_id_from_dir("PLAN-001").as_deref(), Some("PLAN-001"));
        assert_eq!(
            plan_id_from_dir("PLAN-003-grafo-y-registro-de-lenguajes").as_deref(),
            Some("PLAN-003")
        );
        assert_eq!(plan_id_from_dir("not-a-plan"), None);
    }

    #[test]
    fn pick_plan_doc_prefers_dir_name_match() {
        let dir = PathBuf::from("plans/PLAN-003-grafo-y-registro-de-lenguajes");
        let candidates = vec![
            dir.join("PLAN-003-design.md"),
            dir.join("PLAN-003-grafo-y-registro-de-lenguajes.md"),
        ];
        let (picked, warning) =
            pick_plan_doc(&dir, "PLAN-003-grafo-y-registro-de-lenguajes", &candidates);
        assert_eq!(
            picked,
            Some(dir.join("PLAN-003-grafo-y-registro-de-lenguajes.md"))
        );
        assert!(warning.is_none());
    }

    #[test]
    fn pick_plan_doc_excludes_design_suffix_when_no_dir_name_match() {
        let dir = PathBuf::from("plans/PLAN-009-x");
        let candidates = vec![dir.join("PLAN-009-design.md"), dir.join("PLAN-009-y.md")];
        let (picked, warning) = pick_plan_doc(&dir, "PLAN-009-x", &candidates);
        assert_eq!(picked, Some(dir.join("PLAN-009-y.md")));
        assert!(warning.is_none());
    }

    #[test]
    fn table_status_parses_and_disagreement_produces_warning() {
        let plan_text = r#"# PLAN-002 — algo

| Task | Qué | Depende de | Estado |
|------|-----|------------|--------|
| TASK-010 | algo | — | `done` |
"#;
        let (_title, table) = parse_plan_doc(plan_text);
        assert_eq!(table.get("TASK-010"), Some(&Status::Done));
    }

    #[test]
    fn analyze_ready_blocked_missing_and_cycle() {
        let mk = |id: &str, status: Status, deps: &[&str]| Task {
            id: id.to_string(),
            title: id.to_string(),
            plan_id: "PLAN-X".to_string(),
            status,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            external_deps: vec![],
            files: vec![],
            path: PathBuf::from(""),
        };
        let plan = Plan {
            id: "PLAN-X".to_string(),
            title: "x".to_string(),
            dir: PathBuf::from(""),
            doc_path: None,
            tasks: vec![
                mk("TASK-001", Status::Done, &[]),
                mk("TASK-002", Status::Pending, &["TASK-001"]),
                mk("TASK-003", Status::Pending, &["TASK-002"]),
                mk("TASK-004", Status::Pending, &["TASK-099"]),
            ],
            table_status: BTreeMap::new(),
            mtime: None,
            warnings: vec![],
        };
        let analysis = analyze(&plan);
        assert_eq!(analysis.ready, vec!["TASK-002".to_string()]);
        assert!(analysis
            .blocked
            .iter()
            .any(|(id, deps)| id == "TASK-003" && deps == &vec!["TASK-002".to_string()]));
        assert!(analysis
            .missing
            .contains(&("TASK-004".to_string(), "TASK-099".to_string())));
    }

    #[test]
    fn analyze_detects_a_cycle() {
        let mk = |id: &str, deps: &[&str]| Task {
            id: id.to_string(),
            title: id.to_string(),
            plan_id: "PLAN-X".to_string(),
            status: Status::Pending,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            external_deps: vec![],
            files: vec![],
            path: PathBuf::from(""),
        };
        let plan = Plan {
            id: "PLAN-X".to_string(),
            title: "x".to_string(),
            dir: PathBuf::from(""),
            doc_path: None,
            tasks: vec![mk("TASK-001", &["TASK-002"]), mk("TASK-002", &["TASK-001"])],
            table_status: BTreeMap::new(),
            mtime: None,
            warnings: vec![],
        };
        let analysis = analyze(&plan);
        assert!(
            !analysis.cycles.is_empty(),
            "A->B->A debe detectarse como ciclo"
        );
    }

    // --- PLAN-007 TASK-001: tolerant `Estado` (fixtures copied from real revfa plans) ---

    /// Status and warnings of a task whose header carries `line`.
    fn status_of(line: &str) -> (Status, Vec<String>) {
        let text = format!("# TASK-001 — algo\n\n{line}\n\n## Objetivo\n\nTexto.\n");
        let (t, w) = task(&text, "TASK-001", "PLAN-002");
        (t.status, w)
    }

    #[test]
    fn estado_variants_from_real_plans_parse_without_warning() {
        let cases: &[(&str, Status)] = &[
            ("**Status:** done", Status::Done),
            ("**Status:** pending", Status::Pending),
            ("- **Estado:** `pending`", Status::Pending),
            ("**Status:** completed", Status::Done),
            ("**Status:** DONE", Status::Done),
            ("Status: pending", Status::Pending),
            ("**Status**: pending", Status::Pending),
            ("**Status:** ✅ Completed — todo aplicado", Status::Done),
            ("**Status:** implemented — pending deploy", Status::Done),
            ("**Estado:** Pendiente", Status::Pending),
            ("**Estado**: PENDING", Status::Pending),
            ("- **Estado:** sigue `pending`.", Status::Pending),
            ("- **Estado:** `skipped`", Status::Skipped),
            ("**Status:** POSTPONED", Status::Skipped),
            ("**Estado:** pospuesta hasta la fase 2", Status::Skipped),
            (
                "- **Estado:** ✅ **`done`** — ejecutada en la rama",
                Status::Done,
            ),
            ("**Estado: `done`.**", Status::Done),
            (
                "🟢 `pending` — **DESBLOQUEADA tras TASK-002**",
                Status::Pending,
            ),
            (
                "**Estado:** 🟨 **`in_progress`** (revisión 13)",
                Status::InProgress,
            ),
            ("**Estado:** en progreso", Status::InProgress),
            ("**Estado:** ✅", Status::Done),
            ("**State:** blocked", Status::Blocked),
        ];
        for (line, expected) in cases {
            // A bare emoji line has no `Campo:`; the field form is what real plans use.
            let line = if line.starts_with("🟢") {
                format!("**Estado:** {line}")
            } else {
                line.to_string()
            };
            let (status, warnings) = status_of(&line);
            assert_eq!(&status, expected, "línea: {line}");
            assert!(warnings.is_empty(), "línea: {line}, warnings: {warnings:?}");
        }
    }

    #[test]
    fn several_fields_on_one_line_split_on_middle_dot() {
        let (t, w) = task(
            "# TASK-001 — algo\n\n**Plan:** PLAN-111 · **Estado:** ⬜ PENDIENTE · **Depende de:** —\n\n## Objetivo\n",
            "TASK-001",
            "PLAN-111",
        );
        assert_eq!(t.status, Status::Pending);
        assert!(t.depends_on.is_empty());
        assert!(w.is_empty(), "{w:?}");

        let (t, w) = task(
            "# TASK-001 — algo\n\n**Specialist**: x · **Proyecto**: y · **Status**: done\n\n## Objetivo\n",
            "TASK-001",
            "PLAN-072",
        );
        assert_eq!(t.status, Status::Done);
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn status_in_yaml_front_matter() {
        let text =
            "---\nstatus: done\ndepends_on: [TASK-002]\n---\n\n# TASK-003 — algo\n\n## Objetivo\n";
        let (t, w) = task(text, "TASK-003", "PLAN-002");
        assert_eq!(t.status, Status::Done);
        assert_eq!(t.depends_on, vec!["TASK-002"]);
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn status_in_estado_section_on_the_next_line() {
        let text = "# TASK-001 — algo\n\n- **Plan:** PLAN-009\n\n## Estado\n\n`completed`\n\n## Objetivo\n";
        let (t, w) = task(text, "TASK-001", "PLAN-009");
        assert_eq!(t.status, Status::Done);
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn estado_final_and_estado_objetivo_are_never_the_status() {
        let text = "# TASK-001 — algo\n\n- **Estado objetivo:** `done`\n- **Estado final:** `done`\n\n## Objetivo\n";
        let (t, w) = task(text, "TASK-001", "PLAN-002");
        assert_eq!(t.status, Status::Pending);
        assert!(w.iter().any(|w| w.contains("falta el campo Estado")));
    }

    #[test]
    fn estado_without_a_keyword_stays_unknown_and_warns() {
        for value in [
            "código aplicado en working tree, sin commit",
            "`ready-for-approval`",
        ] {
            let (status, warnings) = status_of(&format!("- **Estado:** {value}"));
            assert!(matches!(status, Status::Unknown(_)), "{value}");
            assert!(warnings.iter().any(|w| w.contains("Estado no reconocido")));
        }
    }

    #[test]
    fn backtick_keyword_wins_over_prose() {
        let (status, _) = status_of("- **Estado:** pendiente de revisar lo `done` antes");
        assert_eq!(status, Status::Done);
    }

    #[test]
    fn skipped_resolves_dependencies_but_is_not_done() {
        assert!(Status::Skipped.is_resolved());
        assert!(!Status::Skipped.is_done());
        let mk = |id: &str, status: Status, deps: &[&str]| Task {
            id: id.to_string(),
            title: id.to_string(),
            plan_id: "PLAN-X".to_string(),
            status,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            external_deps: vec![],
            files: vec![],
            path: PathBuf::from(""),
        };
        let plan = Plan {
            id: "PLAN-X".to_string(),
            title: "x".to_string(),
            dir: PathBuf::from(""),
            doc_path: None,
            tasks: vec![
                mk("TASK-001", Status::Skipped, &[]),
                mk("TASK-002", Status::Pending, &["TASK-001"]),
            ],
            table_status: BTreeMap::new(),
            mtime: None,
            warnings: vec![],
        };
        let analysis = analyze(&plan);
        assert_eq!(analysis.ready, vec!["TASK-002".to_string()]);
        assert!(analysis.blocked.is_empty());
        assert_eq!(status_label(&Status::Skipped), "skipped");
    }

    #[test]
    fn plan_titles_lose_any_plan_prefix_and_separator() {
        assert_eq!(
            h1_title_strip_plan_prefix("PLAN-129: Actualizar configuración"),
            "Actualizar configuración"
        );
        assert_eq!(
            h1_title_strip_plan_prefix("PLAN-007 — Planes en la raíz"),
            "Planes en la raíz"
        );
        // PLAN-008's H1 names PLAN-1, not its own number.
        assert_eq!(h1_title_strip_plan_prefix("PLAN-1: Auth"), "Auth");
        assert_eq!(h1_title_strip_plan_prefix("Sin prefijo"), "Sin prefijo");
    }

    // --- PLAN-007 TASK-002: ids, `Depende de` by extraction, status table ---

    fn deps_of(value: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
        let text = format!("# TASK-009 — algo\n\n- **Estado:** `pending`\n- **Depende de:** {value}\n\n## Objetivo\n");
        let (t, w) = task(&text, "TASK-009", "PLAN-002");
        (t.depends_on, t.external_deps, w)
    }

    #[test]
    fn depende_de_prose_without_ids_warns_once_and_invents_nothing() {
        for value in [
            "Fase 0",
            "todas",
            "ALL previous tasks",
            "Waves 1-4 completas",
        ] {
            let (deps, ext, w) = deps_of(value);
            assert!(deps.is_empty() && ext.is_empty(), "{value}: {deps:?}");
            assert_eq!(w.len(), 1, "{value}: {w:?}");
            assert!(w[0].contains("sin TASK reconocible"));
        }
    }

    #[test]
    fn depende_de_slash_list_and_ellipsis_range_expand() {
        let (deps, _, w) = deps_of("TASK-033/034/035");
        assert_eq!(deps, vec!["TASK-033", "TASK-034", "TASK-035"]);
        assert!(w.is_empty());

        let (deps, _, w) = deps_of("TASK-012 … TASK-018");
        assert_eq!(deps.len(), 7);
        assert_eq!(deps.first().map(String::as_str), Some("TASK-012"));
        assert_eq!(deps.last().map(String::as_str), Some("TASK-018"));
        assert!(w.is_empty());

        let (deps, _, _) = deps_of("TASK-001...003");
        assert_eq!(deps, vec!["TASK-001", "TASK-002", "TASK-003"]);
    }

    #[test]
    fn depende_de_ignores_prose_around_ids() {
        let (deps, _, w) = deps_of("TASK-001 (`done`), Paso 0b y luego TASK-004");
        assert_eq!(deps, vec!["TASK-001", "TASK-004"]);
        assert!(w.is_empty());

        let (deps, _, w) = deps_of("Fase 0 (y coordinar orden con TASK-006, ver Riesgos)");
        assert_eq!(deps, vec!["TASK-006"]);
        assert!(w.is_empty());
    }

    #[test]
    fn depende_de_cross_plan_refs_are_external_not_missing() {
        for value in [
            "`PLAN-120` TASK-031 (pares duplicados)",
            "PLAN-120/TASK-031",
        ] {
            let (deps, ext, w) = deps_of(value);
            assert!(deps.is_empty(), "{value}");
            assert_eq!(ext, vec!["PLAN-120/TASK-031"], "{value}");
            assert!(w.is_empty(), "{value}: {w:?}");
        }
    }

    #[test]
    fn depende_de_none_marks_win_over_prose() {
        for value in [
            "ninguna (puede arrancar en paralelo con TASK-002 …)",
            "none",
            "[]",
            "–",
            "— (paralela a TASK-001)",
        ] {
            let (deps, ext, w) = deps_of(value);
            assert!(deps.is_empty() && ext.is_empty(), "{value}");
            assert!(w.is_empty(), "{value}: {w:?}");
        }
    }

    #[test]
    fn depende_de_drops_self_dependency() {
        let text = "# TASK-001b — algo\n\n- **Estado:** `pending`\n- **Depende de:** TASK-001b\n";
        let (t, w) = task(text, "TASK-001b", "PLAN-002");
        assert!(t.depends_on.is_empty());
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn dependency_keys_in_english_spanish_and_front_matter() {
        for key in [
            "Dependencies",
            "Depends on",
            "Dependencias",
            "Bloqueada por",
            "Blocked by",
        ] {
            let text = format!(
                "# TASK-002 — algo\n\n**Status:** pending\n**{key}:** TASK-001\n\n## Objetivo\n"
            );
            let (t, w) = task(&text, "TASK-002", "PLAN-002");
            assert_eq!(t.depends_on, vec!["TASK-001"], "{key}");
            assert!(w.is_empty(), "{key}: {w:?}");
        }
        let text = "---\nstatus: pending\nDependencies: none\n---\n\n# TASK-002 — algo\n";
        let (t, w) = task(text, "TASK-002", "PLAN-002");
        assert!(t.depends_on.is_empty());
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn task_ids_keep_letter_suffix_and_accept_prefixed_ids() {
        assert_eq!(
            extract_task_id_from_filename("TASK-001b-algo").as_deref(),
            Some("TASK-001b")
        );
        assert_eq!(
            extract_task_id_from_filename("TASK-001-algo").as_deref(),
            Some("TASK-001")
        );
        assert_eq!(
            extract_task_id_from_filename("TASK-001-DESIGN").as_deref(),
            Some("TASK-001")
        );
        assert_eq!(
            extract_task_id_from_filename("TASK-7").as_deref(),
            Some("TASK-007")
        );
        assert_eq!(
            extract_task_id_from_filename("TASK-R01-algo").as_deref(),
            Some("TASK-R01")
        );
        assert_eq!(
            extract_task_id_from_filename("TASK-DB-001-algo").as_deref(),
            Some("TASK-DB-001")
        );
        assert_eq!(extract_task_id_from_filename("T032-001-algo"), None);
    }

    #[test]
    fn suffixed_task_depending_on_its_base_is_not_a_cycle() {
        let text = "# TASK-001b — algo\n\n- **Estado:** `pending`\n- **Depende de:** TASK-001\n";
        let (b, _) = task(text, "TASK-001b", "PLAN-119");
        assert_eq!(b.depends_on, vec!["TASK-001"]);
        let (a, _) = task(
            "# TASK-001 — x\n\n- **Estado:** `done`\n",
            "TASK-001",
            "PLAN-119",
        );
        let plan = Plan {
            id: "PLAN-119".to_string(),
            title: "x".to_string(),
            dir: PathBuf::from(""),
            doc_path: None,
            tasks: vec![a, b],
            table_status: BTreeMap::new(),
            mtime: None,
            warnings: vec![],
        };
        let analysis = analyze(&plan);
        assert!(analysis.cycles.is_empty(), "{:?}", analysis.cycles);
        assert_eq!(analysis.ready, vec!["TASK-001b".to_string()]);
    }

    #[test]
    fn external_deps_neither_block_nor_go_missing() {
        let text =
            "# TASK-001 — algo\n\n- **Estado:** `pending`\n- **Depende de:** `PLAN-120` TASK-031\n";
        let (t, _) = task(text, "TASK-001", "PLAN-002");
        let plan = Plan {
            id: "PLAN-002".to_string(),
            title: "x".to_string(),
            dir: PathBuf::from(""),
            doc_path: None,
            tasks: vec![t],
            table_status: BTreeMap::new(),
            mtime: None,
            warnings: vec![],
        };
        let analysis = analyze(&plan);
        assert_eq!(analysis.ready, vec!["TASK-001".to_string()]);
        assert!(analysis.missing.is_empty());
    }

    #[test]
    fn table_without_a_status_column_is_ignored() {
        let text = "# PLAN-129 — algo\n\n| Paso | Acción |\n|---|---|\n| TASK-001 | Revertir el commit |\n| TASK-002 | Volver atrás |\n";
        let (_t, table) = parse_plan_doc(text);
        assert!(table.is_empty(), "{table:?}");
    }

    #[test]
    fn table_reads_the_status_column_not_the_last_cell() {
        let text = "# PLAN-005 — algo\n\n| Task | Estado | Notas |\n|---|---|---|\n| TASK-001 | `done` | Revertir el commit |\n| TASK-002 | `pending` | — |\n";
        let (_t, table) = parse_plan_doc(text);
        assert_eq!(table.get("TASK-001"), Some(&Status::Done));
        assert_eq!(table.get("TASK-002"), Some(&Status::Pending));

        let text = "| Task | Status |\n|---|---|\n| TASK-001 | done |\n";
        assert_eq!(parse_plan_doc(text).1.get("TASK-001"), Some(&Status::Done));
    }

    #[test]
    fn table_struck_ids_are_skipped_and_prose_cells_add_nothing() {
        let text = "| ID | Título | Dependencias | Estimación | Estado |\n|---|---|---|---|---|\n| ~~TASK-011~~ | ~~identidad uuidv7~~ | — | — | `skipped` — revisión 2 |\n| ~~TASK-012~~ | ~~otra~~ | — | — | — |\n| TASK-013 | algo | — | 2h | por definir |\n| TASK-014b | algo | — | 2h | `done` |\n";
        let (_t, table) = parse_plan_doc(text);
        assert_eq!(table.get("TASK-011"), Some(&Status::Skipped));
        assert_eq!(table.get("TASK-012"), Some(&Status::Skipped));
        assert_eq!(table.get("TASK-013"), None);
        assert_eq!(table.get("TASK-014b"), Some(&Status::Done));
    }

    #[test]
    fn pick_plan_doc_prefers_plan_prefixed_candidates_over_other_markdown() {
        let dir = PathBuf::from("plans/PLAN-013-x");
        let candidates = vec![
            dir.join("BRIEF-CLAUDE-DESIGN-v5.1.md"),
            dir.join("PLAN-013-algo.md"),
        ];
        let (picked, warning) = pick_plan_doc(&dir, "PLAN-013-x", &candidates);
        assert_eq!(picked, Some(dir.join("PLAN-013-algo.md")));
        assert!(warning.is_none());

        // With no PLAN-<n> candidate, any other markdown is still better than nothing.
        let only = vec![dir.join("BRIEF.md")];
        let (picked, _) = pick_plan_doc(&dir, "PLAN-013-x", &only);
        assert_eq!(picked, Some(dir.join("BRIEF.md")));
    }

    #[test]
    fn load_plans_reports_duplicate_ids_and_keeps_suffixed_tasks_apart() {
        let root = std::env::temp_dir().join(format!("devctx-plans-dup-{}", std::process::id()));
        let tasks = root.join("plans/PLAN-047-x/tasks");
        std::fs::create_dir_all(&tasks).unwrap();
        std::fs::write(
            root.join("plans/PLAN-047-x/PLAN-047-x.md"),
            "# PLAN-047: X\n",
        )
        .unwrap();
        for (name, id) in [
            ("TASK-001-DESIGN.md", "TASK-001"),
            ("TASK-001-verify-algo.md", "TASK-001"),
            ("TASK-001b-algo.md", "TASK-001b"),
        ] {
            std::fs::write(
                tasks.join(name),
                format!("# {id} — t\n\n- **Estado:** `pending`\n"),
            )
            .unwrap();
        }
        let plans = load_plans(&root);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(plans.len(), 1);
        let ids: Vec<&str> = plans[0].tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["TASK-001", "TASK-001b"]);
        assert_eq!(plans[0].title, "X");
        let dups: Vec<&String> = plans[0]
            .warnings
            .iter()
            .filter(|w| w.contains("id duplicado"))
            .collect();
        assert_eq!(dups.len(), 1, "{:?}", plans[0].warnings);
    }

    // --- resolve_plans_root (PLAN-007 TASK-003) ---

    /// A scratch tree under the temp dir, recreated empty on every call.
    fn scratch(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("devctx_plans_root_{label}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `<home>/ws/plans/PLAN-001-x` plus members `<home>/ws/api` (no `plans/`).
    fn workspace_tree(label: &str) -> (PathBuf, PathBuf, PathBuf) {
        let home = scratch(label);
        let ws = home.join("ws");
        std::fs::create_dir_all(ws.join("plans/PLAN-001-x")).unwrap();
        let member = ws.join("api");
        std::fs::create_dir_all(&member).unwrap();
        (home, ws, member)
    }

    #[test]
    fn resolve_workspace_wins_when_it_has_plans() {
        let (home, ws, member) = workspace_tree("ws_wins");
        let r = resolve_plans_root(Some(&member), Some(&ws), true, Some(&home));
        assert_eq!(r.root, ws);
        assert_eq!(r.source, PlansRootSource::Workspace);
    }

    #[test]
    fn resolve_member_with_group_adopts_the_workspace_as_ancestor() {
        let (home, ws, member) = workspace_tree("ancestor");
        let r = resolve_plans_root(Some(&member), None, true, Some(&home));
        assert_eq!(r.root, ws);
        assert_eq!(r.source, PlansRootSource::Ancestor);
    }

    #[test]
    fn resolve_member_without_group_does_not_walk_up() {
        let (home, _ws, member) = workspace_tree("no_group");
        let r = resolve_plans_root(Some(&member), None, false, Some(&home));
        assert_eq!(r.root, member);
        assert_eq!(r.source, PlansRootSource::None);
    }

    #[test]
    fn resolve_member_with_own_plans_beats_the_ancestor() {
        let (home, _ws, member) = workspace_tree("own_plans");
        std::fs::create_dir_all(member.join("plans")).unwrap();
        let r = resolve_plans_root(Some(&member), None, true, Some(&home));
        assert_eq!(r.root, member);
        assert_eq!(r.source, PlansRootSource::Project);
    }

    #[test]
    fn resolve_ignores_an_ancestor_plans_dir_without_plan_entries() {
        let home = scratch("empty_plans");
        let ws = home.join("ws");
        std::fs::create_dir_all(ws.join("plans/notes")).unwrap();
        let member = ws.join("api");
        std::fs::create_dir_all(&member).unwrap();
        let r = resolve_plans_root(Some(&member), None, true, Some(&home));
        assert_eq!(r.source, PlansRootSource::None);
        assert_eq!(r.root, member);
    }

    #[test]
    fn resolve_never_adopts_plans_at_home_itself() {
        let home = scratch("home_plans");
        std::fs::create_dir_all(home.join("plans/PLAN-001-x")).unwrap();
        let member = home.join("api");
        std::fs::create_dir_all(&member).unwrap();
        let r = resolve_plans_root(Some(&member), None, true, Some(&home));
        assert_eq!(r.source, PlansRootSource::None);
    }

    #[test]
    fn resolve_without_home_does_not_walk_up() {
        let (_home, _ws, member) = workspace_tree("no_home");
        let r = resolve_plans_root(Some(&member), None, true, None);
        assert_eq!(r.source, PlansRootSource::None);
    }

    #[test]
    fn resolve_outside_a_project_uses_the_cwd_or_falls_back_to_it() {
        let (home, ws, _member) = workspace_tree("no_project");
        let r = resolve_plans_root(None, Some(&ws), false, Some(&home));
        assert_eq!(
            (r.root.as_path(), r.source),
            (ws.as_path(), PlansRootSource::Workspace)
        );
        let bare = home.join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        let r = resolve_plans_root(None, Some(&bare), false, Some(&home));
        assert_eq!(
            (r.root.as_path(), r.source),
            (bare.as_path(), PlansRootSource::None)
        );
    }
}
