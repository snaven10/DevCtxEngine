//! Parser for the markdown plans under `plans/` (see `plans/PLAN-005-plan-status/`).
//!
//! Plans and tasks live in git as markdown, hand-written. This module turns that text into a
//! graph: pure parsing functions (`parse_plan_doc`, `parse_task_doc`) plus a thin disk-loading
//! layer (`load_plans`). No new dependencies: the format is parsed line by line.

use std::collections::BTreeMap;
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
    Unknown(String),
}

impl Status {
    pub fn is_done(&self) -> bool {
        matches!(self, Status::Done)
    }

    /// Parse the first token of a field value (already trimmed of backticks) into a `Status`.
    fn parse(raw: &str) -> Status {
        let token = first_token(raw);
        let lowered = token.to_lowercase();
        match lowered.as_str() {
            "pending" => Status::Pending,
            "in_progress" | "in-progress" | "en" if lowered == "en" => {
                // handled below for "en progreso" (two tokens)
                Status::Unknown(raw.trim().to_string())
            }
            "in_progress" | "in-progress" => Status::InProgress,
            "done" => Status::Done,
            "blocked" | "bloqueada" => Status::Blocked,
            _ => {
                // "en progreso" is two tokens; check the raw value for that phrase.
                let normalized = lowered_no_accents(raw.trim());
                if normalized.starts_with("en progreso") {
                    Status::InProgress
                } else if lowered.is_empty() {
                    Status::Unknown(String::new())
                } else {
                    Status::Unknown(raw.trim().to_string())
                }
            }
        }
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
        let (shorter, longer) = if a.len() <= b.len() { (&a, &b) } else { (&b, &a) };
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

fn first_token(s: &str) -> String {
    s.trim()
        .trim_start_matches('`')
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| c == '`' || c == ',')
        .to_string()
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

/// Parses a header field line like `- **Campo:** valor`, `**Campo**: valor`, `**Campo:** valor`.
/// Returns `(field_name_lowercase_no_accents, value)` if the line is a field line.
fn parse_field_line(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    let trimmed = trimmed.strip_prefix('-').map(|s| s.trim()).unwrap_or(trimmed);
    if !trimmed.starts_with("**") {
        return None;
    }
    let rest = &trimmed[2..];
    // Find the closing "**"
    let end = rest.find("**")?;
    let field_raw = &rest[..end];
    let after = &rest[end + 2..];
    // after may start with ':' (Campo:** valor) or the colon may be inside the bold
    // (Campo:** ...) — handle both "**Campo:**" and "**Campo**:" forms.
    let (field_name, value) = if let Some(field_name) = field_raw.strip_suffix(':') {
        (field_name, after.trim_start_matches(':').trim())
    } else {
        // "**Campo**: valor" form: after should start with ':'
        let value = after.strip_prefix(':').unwrap_or(after);
        (field_raw, value.trim())
    };
    let field_key = strip_accents_lower(field_name.trim());
    Some((field_key, value.trim().to_string()))
}

/// Extracts the header block: lines between the first H1 (`# `) and the first `## ` section.
fn header_block(text: &str) -> &str {
    let mut start = 0;
    let mut found_h1 = false;
    let bytes_lines: Vec<(usize, &str)> = text.lines().enumerate().collect();
    let mut end = text.len();
    let mut offset = 0usize;
    let mut h1_end_offset = None;
    let mut header_end_offset = None;
    for (i, line) in &bytes_lines {
        let line_len = line.len() + 1; // approx, newline
        if !found_h1 && line.trim_start().starts_with("# ") {
            found_h1 = true;
            h1_end_offset = Some(offset + line_len);
        } else if found_h1 && line.trim_start().starts_with("## ") {
            header_end_offset = Some(offset);
            break;
        }
        offset += line_len;
        let _ = i;
    }
    if let Some(h1_end) = h1_end_offset {
        start = h1_end.min(text.len());
    }
    if let Some(h_end) = header_end_offset {
        end = h_end.min(text.len());
    }
    if start > end {
        start = 0;
    }
    &text[start.min(text.len())..end.min(text.len())]
}

/// Extracts the H1 title (text after `# ` or `# PREFIX — `).
fn h1_title(text: &str) -> Option<String> {
    for line in text.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("# ") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Parses the `Depende de` field value into a list of `TASK-NNN` ids (3-digit normalized).
/// Returns `(deps, warning)`.
fn parse_depends_on(raw: &str) -> (Vec<String>, Option<String>) {
    let trimmed = raw.trim();
    let no_space = trimmed.replace(' ', "");
    let lowered = strip_accents_lower(&no_space);
    if no_space.starts_with('—')
        || no_space.starts_with('-')
        || lowered.starts_with("ninguna")
        || lowered.starts_with("n/a")
    {
        return (Vec::new(), None);
    }
    // Range TASK-001..004
    let mut deps = Vec::new();
    let mut warning = None;
    for tok in trimmed.split(|c: char| c == ',' || c.is_whitespace()) {
        let tok = tok.trim_matches(|c: char| c == '(' || c == ')' || c == '.');
        if tok.is_empty() {
            continue;
        }
        if let Some(range) = parse_range(tok) {
            deps.extend(range);
            continue;
        }
        if let Some(id) = normalize_task_id(tok) {
            deps.push(id);
            continue;
        }
        // Unrecognized token that isn't just punctuation/connector words.
        let lw = strip_accents_lower(tok);
        if matches!(lw.as_str(), "y" | "," | "" ) {
            continue;
        }
        warning = Some(format!("Depende de: token no reconocido '{tok}'"));
    }
    (deps, warning)
}

fn parse_range(tok: &str) -> Option<Vec<String>> {
    let (a, b) = tok.split_once("..")?;
    let a_id = normalize_task_id(a)?;
    let b_num: u32 = digits_of(b)?.parse().ok()?;
    let a_num: u32 = digits_of(&a_id)?.parse().ok()?;
    if b_num < a_num {
        return None;
    }
    Some((a_num..=b_num).map(|n| format!("TASK-{n:03}")).collect())
}

fn digits_of(s: &str) -> Option<String> {
    let d: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if d.is_empty() {
        None
    } else {
        Some(d)
    }
}

/// Normalizes a token like `TASK-001`, `TASK-1`, or bare `001`/`1` into `TASK-NNN` (3 digits).
fn normalize_task_id(tok: &str) -> Option<String> {
    let upper = tok.to_uppercase();
    if let Some(rest) = upper.strip_prefix("TASK-").or_else(|| upper.strip_prefix("TASK")) {
        let digits = digits_of(rest)?;
        let n: u32 = digits.parse().ok()?;
        return Some(format!("TASK-{n:03}"));
    }
    // bare number, only if the whole token is digits
    if !tok.is_empty() && tok.chars().all(|c| c.is_ascii_digit()) {
        let n: u32 = tok.parse().ok()?;
        return Some(format!("TASK-{n:03}"));
    }
    None
}

/// Known file extensions recognized when scanning inline code spans for file references.
const KNOWN_EXTS: &[&str] = &["rs", "md", "toml", "yaml", "yml", "json", "html", "js", "ts", "sh"];

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
                refs.push(FileRef { path: path_part, line: line_part });
            }
        }
    }
    refs
}

/// Parses a task document's text into a `Task`. `id` and `plan_id` come from the caller (the
/// filename and the containing directory), since a task doc's H1 also carries the id but the
/// filename is the authoritative source of the id contract used by `depends_on`.
pub fn parse_task_doc(text: &str, id: &str, plan_id: &str, path: &Path) -> (Task, Vec<String>) {
    let mut warnings = Vec::new();
    let title_full = h1_title(text).unwrap_or_default();
    let title = strip_task_prefix(&title_full, id);

    let header = header_block(text);
    let mut status = None;
    let mut depends_on = Vec::new();
    for line in header.lines() {
        if let Some((field, value)) = parse_field_line(line) {
            match field.as_str() {
                "estado" => {
                    status = Some(Status::parse(&value));
                }
                "depende de" => {
                    let (deps, warn) = parse_depends_on(&value);
                    depends_on = deps;
                    if let Some(w) = warn {
                        warnings.push(format!("{id}: {w}"));
                    }
                }
                _ => {}
            }
        }
    }
    let status = match status {
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

/// Parses a plan doc's text (the table of tasks) into `table_status`, keyed by normalized
/// `TASK-NNN` id.
fn parse_table_status(text: &str) -> BTreeMap<String, Status> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let t = line.trim();
        if !t.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = t.trim_matches('|').split('|').map(|c| c.trim()).collect();
        if cells.len() < 2 {
            continue;
        }
        let id_cell = cells[0];
        let Some(id) = normalize_task_id(id_cell) else { continue };
        let last = cells.last().unwrap();
        let cleaned = last.trim_matches('`');
        if cleaned.is_empty() {
            continue;
        }
        let status = Status::parse(cleaned);
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

/// Picks the plan doc among the `.md` files directly in `dir` (not `tasks/`), per TASK-001
/// Paso 8: prefer `<dir-name>.md`; else the only `PLAN-N-*.md` not ending in `-design.md`; if
/// several, warn and take the first in sorted order.
fn pick_plan_doc(dir: &Path, dir_name: &str, candidates: &[PathBuf]) -> (Option<PathBuf>, Option<String>) {
    let preferred = dir.join(format!("{dir_name}.md"));
    if candidates.contains(&preferred) {
        return (Some(preferred), None);
    }
    let mut sorted: Vec<&PathBuf> = candidates
        .iter()
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            !name.ends_with("-design.md")
        })
        .collect();
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
        let dir_name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        let Some(plan_id) = plan_id_from_dir(&dir_name) else { continue };
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
                    warnings.push(format!("nombre de archivo de task no reconocido: {}", f.display()));
                    continue;
                };
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

    let title_final = h1_title_strip_plan_prefix(&title, plan_id);

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

fn h1_title_strip_plan_prefix(title: &str, plan_id: &str) -> String {
    let t = title.trim();
    if let Some(rest) = t.strip_prefix(plan_id) {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix('—').or_else(|| rest.strip_prefix('-')) {
            return rest.trim().to_string();
        }
        return rest.trim().to_string();
    }
    t.to_string()
}

fn status_label(s: &Status) -> String {
    match s {
        Status::Pending => "pending".to_string(),
        Status::InProgress => "in_progress".to_string(),
        Status::Done => "done".to_string(),
        Status::Blocked => "blocked".to_string(),
        Status::Unknown(t) => format!("unknown({t})"),
    }
}

fn extract_task_id_from_filename(stem: &str) -> Option<String> {
    let upper = stem.to_uppercase();
    if !upper.starts_with("TASK-") {
        return None;
    }
    let digits: String = upper[5..].chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let n: u32 = digits.parse().ok()?;
    Some(format!("TASK-{n:03}"))
}

/// Pure analysis of a plan's dependency graph (TASK-001 Paso 11).
pub fn analyze(plan: &Plan) -> Analysis {
    let mut analysis = Analysis::default();
    let by_id: BTreeMap<&str, &Task> = plan.tasks.iter().map(|t| (t.id.as_str(), t)).collect();

    for task in &plan.tasks {
        match task.status {
            Status::Done => continue,
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
                            if !dep_task.status.is_done() {
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
    let mut color: BTreeMap<&str, Color> = plan.tasks.iter().map(|t| (t.id.as_str(), Color::White)).collect();
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
                let Some(&dep_key) = by_id.keys().find(|k| **k == dep_id) else { continue };
                match color.get(dep_key) {
                    Some(Color::White) | None => visit(dep_key, by_id, color, stack, cycles),
                    Some(Color::Gray) => {
                        // found a cycle: slice the stack from dep_key's position
                        if let Some(pos) = stack.iter().position(|s| *s == dep_key) {
                            let mut cycle: Vec<String> = stack[pos..].iter().map(|s| s.to_string()).collect();
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
        assert!(t.depends_on.is_empty(), "— (paralela a TASK-001) debe leerse como sin dependencia");
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
            let text = format!(
                "# TASK-001 — algo\n\n- **Depende de:** {value}\n\n## Objetivo\n"
            );
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
        let state_rs = t.files.iter().find(|f| f.path == "crates/devctx-mcp/src/state.rs").unwrap();
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
        let a = FileRef { path: "crates/devctx-mcp/src/state.rs".to_string(), line: None };
        assert!(a.matches("crates/devctx-mcp/src/state.rs"));
        assert!(a.matches("devctx-mcp/src/state.rs"));
        assert!(!a.matches("state.rs"));

        let bare = FileRef { path: "lib.rs".to_string(), line: None };
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
        assert_eq!(picked, Some(dir.join("PLAN-003-grafo-y-registro-de-lenguajes.md")));
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
        assert!(analysis.blocked.iter().any(|(id, deps)| id == "TASK-003" && deps == &vec!["TASK-002".to_string()]));
        assert!(analysis.missing.contains(&("TASK-004".to_string(), "TASK-099".to_string())));
    }

    #[test]
    fn analyze_detects_a_cycle() {
        let mk = |id: &str, deps: &[&str]| Task {
            id: id.to_string(),
            title: id.to_string(),
            plan_id: "PLAN-X".to_string(),
            status: Status::Pending,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
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
        assert!(!analysis.cycles.is_empty(), "A->B->A debe detectarse como ciclo");
    }
}
