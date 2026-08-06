//! `devctx-tui` — an interactive terminal UI for DevCtxEngine (ratatui).
//!
//! Three views, switched with F1/F2/F3:
//!   - **Search**: type a query, switch retrieval mode (vector/keyword/hybrid),
//!     browse ranked results and preview the selected chunk.
//!   - **Graph**: type a symbol, see its transitive callers (upstream) and
//!     callees (downstream) from the call-graph.
//!   - **Memories**: recall memories by query, or browse the most recent.
//!
//! Reranking is skipped for responsiveness. See `docs/rust-rewrite-plan.md`.

use std::path::PathBuf;

use devctx_core::config::ProjectConfig;
use devctx_core::{SearchFilter, SearchResult};
use devctx_embed::{create_provider, EmbedSettings, EmbeddingProvider};
use devctx_index::GitRepo;
use devctx_search::{search as run_search, SearchMode};
use devctx_store::Store;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

/// Results fetched per query.
const LIMIT: usize = 20;

/// Transitive callers and callees of a symbol, each `(symbol, depth)`.
type ImpactLists = (Vec<(String, usize)>, Vec<(String, usize)>);

/// A memory row, unified across recall (scored) and recent (unscored).
#[derive(Clone)]
struct MemoryItem {
    title: String,
    mtype: String,
    tags: String,
    content: String,
    score: Option<f32>,
}

/// The engine backing the UI (embedder + store; no reranker for speed).
struct Engine {
    store: Store,
    embedder: Box<dyn EmbeddingProvider>,
    filter: SearchFilter,
    repo: String,
    branch: String,
    project: String,
}

impl Engine {
    fn build(cfg: &ProjectConfig) -> anyhow::Result<Self> {
        let embedder = create_provider(&EmbedSettings::from_config(&cfg.embeddings))?;
        let store = Store::open(&cfg.db_path(), embedder.dimension())?;
        let root = if cfg.project.path.is_empty() {
            std::env::current_dir()?
        } else {
            PathBuf::from(&cfg.project.path)
        };
        let (repo, branch) = match GitRepo::open(&root) {
            Ok(git) => (git.short_name(), git.state().branch),
            Err(_) => (String::new(), String::new()),
        };
        let project = if cfg.project.name.is_empty() {
            "default".to_string()
        } else {
            cfg.project.name.clone()
        };
        Ok(Self {
            store,
            embedder,
            filter: SearchFilter {
                exclude_deletions: true,
                ..Default::default()
            },
            repo,
            branch,
            project,
        })
    }

    fn search(&self, query: &str, mode: SearchMode) -> anyhow::Result<Vec<SearchResult>> {
        let embedder = if mode == SearchMode::Keyword {
            None
        } else {
            Some(self.embedder.as_ref())
        };
        Ok(run_search(
            &self.store,
            query,
            &self.filter,
            LIMIT,
            mode,
            embedder,
            None,
        )?)
    }

    /// Transitive callers (upstream) and callees (downstream) of a symbol.
    fn graph(&self, symbol: &str) -> anyhow::Result<ImpactLists> {
        let im = self
            .store
            .impact_analysis(&self.repo, &self.branch, symbol, 3)?;
        Ok((im.upstream, im.downstream))
    }

    fn recall(&self, query: &str) -> anyhow::Result<Vec<MemoryItem>> {
        let hits = devctx_memory::recall(
            &self.store,
            self.embedder.as_ref(),
            query,
            Some(&self.project),
            LIMIT,
        )?;
        Ok(hits
            .into_iter()
            .map(|h| MemoryItem {
                title: h.memory.title,
                mtype: h.memory.memory_type,
                tags: h.memory.tags,
                content: h.memory.content,
                score: Some(h.score),
            })
            .collect())
    }

    fn recent_memories(&self) -> anyhow::Result<Vec<MemoryItem>> {
        let mems = devctx_memory::memory_context(&self.store, &self.project, LIMIT)?;
        Ok(mems
            .into_iter()
            .map(|m| MemoryItem {
                title: m.title,
                mtype: m.memory_type,
                tags: m.tags,
                content: m.content,
                score: None,
            })
            .collect())
    }
}

/// Which view is active.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Search,
    Graph,
    Memories,
}

/// UI state.
struct App {
    view: View,
    query: String,
    mode: SearchMode,
    // search
    results: Vec<SearchResult>,
    list: ListState,
    // graph
    upstream: Vec<(String, usize)>,
    downstream: Vec<(String, usize)>,
    // memories
    memories: Vec<MemoryItem>,
    mem_list: ListState,
    status: String,
}

impl App {
    fn new() -> Self {
        Self {
            view: View::Search,
            query: String::new(),
            mode: SearchMode::Vector,
            results: Vec::new(),
            list: ListState::default(),
            upstream: Vec::new(),
            downstream: Vec::new(),
            memories: Vec::new(),
            mem_list: ListState::default(),
            status: "Type a query and press Enter".to_string(),
        }
    }

    fn mode_name(&self) -> &'static str {
        match self.mode {
            SearchMode::Vector => "vector",
            SearchMode::Keyword => "keyword",
            SearchMode::Hybrid => "hybrid",
        }
    }

    fn cycle_mode(&mut self) {
        self.mode = match self.mode {
            SearchMode::Vector => SearchMode::Keyword,
            SearchMode::Keyword => SearchMode::Hybrid,
            SearchMode::Hybrid => SearchMode::Vector,
        };
    }

    /// The list state + length of the active, navigable view.
    fn active_list(&mut self) -> Option<(&mut ListState, usize)> {
        match self.view {
            View::Search => Some((&mut self.list, self.results.len())),
            View::Memories => Some((&mut self.mem_list, self.memories.len())),
            View::Graph => None,
        }
    }

    fn next(&mut self) {
        if let Some((list, len)) = self.active_list() {
            if len == 0 {
                return;
            }
            let i = list.selected().map(|i| (i + 1).min(len - 1)).unwrap_or(0);
            list.select(Some(i));
        }
    }

    fn prev(&mut self) {
        if let Some((list, len)) = self.active_list() {
            if len == 0 {
                return;
            }
            let i = list.selected().map(|i| i.saturating_sub(1)).unwrap_or(0);
            list.select(Some(i));
        }
    }

    fn selected(&self) -> Option<&SearchResult> {
        self.list.selected().and_then(|i| self.results.get(i))
    }

    fn selected_memory(&self) -> Option<&MemoryItem> {
        self.mem_list.selected().and_then(|i| self.memories.get(i))
    }
}

/// Launch the TUI, restoring the terminal on exit.
pub fn run(cfg: ProjectConfig) -> anyhow::Result<()> {
    eprintln!("Loading the embedding model…");
    let engine = Engine::build(&cfg)?;
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &engine);
    ratatui::restore();
    result
}

fn event_loop(terminal: &mut DefaultTerminal, engine: &Engine) -> anyhow::Result<()> {
    let mut app = App::new();
    loop {
        terminal.draw(|f| ui(f, &mut app))?;
        if let Event::Key(k) = event::read()? {
            if k.kind != KeyEventKind::Press {
                continue;
            }
            if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
                break;
            }
            match k.code {
                KeyCode::Esc => break,
                KeyCode::F(1) => app.view = View::Search,
                KeyCode::F(2) => app.view = View::Graph,
                KeyCode::F(3) => {
                    app.view = View::Memories;
                    if app.memories.is_empty() && app.query.is_empty() {
                        load_recent(&mut app, engine);
                    }
                }
                KeyCode::Enter => {
                    terminal.draw(|f| ui(f, &mut app))?;
                    submit(&mut app, engine);
                }
                KeyCode::Tab => app.cycle_mode(),
                KeyCode::Down => app.next(),
                KeyCode::Up => app.prev(),
                KeyCode::Backspace => {
                    app.query.pop();
                }
                KeyCode::Char(c) => app.query.push(c),
                _ => {}
            }
        }
    }
    Ok(())
}

/// Run the active view's query.
fn submit(app: &mut App, engine: &Engine) {
    match app.view {
        View::Search => {
            app.status = "Searching…".to_string();
            match engine.search(&app.query, app.mode) {
                Ok(r) => {
                    app.results = r;
                    app.list.select((!app.results.is_empty()).then_some(0));
                    app.status = format!("{} results", app.results.len());
                }
                Err(e) => {
                    app.results.clear();
                    app.list.select(None);
                    app.status = format!("error: {e}");
                }
            }
        }
        View::Graph => {
            let sym = app.query.trim().to_string();
            if sym.is_empty() {
                app.status = "Enter a symbol name".to_string();
                return;
            }
            match engine.graph(&sym) {
                Ok((up, down)) => {
                    app.upstream = up;
                    app.downstream = down;
                    app.status = format!(
                        "{}: {} callers · {} callees",
                        sym,
                        app.upstream.len(),
                        app.downstream.len()
                    );
                }
                Err(e) => {
                    app.upstream.clear();
                    app.downstream.clear();
                    app.status = format!("error: {e}");
                }
            }
        }
        View::Memories => {
            if app.query.trim().is_empty() {
                load_recent(app, engine);
                return;
            }
            match engine.recall(&app.query) {
                Ok(m) => {
                    app.memories = m;
                    app.mem_list.select((!app.memories.is_empty()).then_some(0));
                    app.status = format!("{} recalled", app.memories.len());
                }
                Err(e) => {
                    app.memories.clear();
                    app.mem_list.select(None);
                    app.status = format!("error: {e}");
                }
            }
        }
    }
}

fn load_recent(app: &mut App, engine: &Engine) {
    match engine.recent_memories() {
        Ok(m) => {
            app.memories = m;
            app.mem_list.select((!app.memories.is_empty()).then_some(0));
            app.status = format!("{} recent memories", app.memories.len());
        }
        Err(e) => {
            app.memories.clear();
            app.status = format!("error: {e}");
        }
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(f.area());

    f.render_widget(tabs_line(app), chunks[0]);

    let prompt = match app.view {
        View::Search => format!("Query — mode: {} (Tab to change)", app.mode_name()),
        View::Graph => "Symbol (Enter for callers/callees)".to_string(),
        View::Memories => "Recall (empty = recent)".to_string(),
    };
    let input = Paragraph::new(app.query.as_str()).block(Block::bordered().title(prompt));
    f.render_widget(input, chunks[1]);

    match app.view {
        View::Search => render_search(f, app, chunks[2]),
        View::Graph => render_graph(f, app, chunks[2]),
        View::Memories => render_memories(f, app, chunks[2]),
    }

    let help = match app.view {
        View::Search => {
            "F1 search  F2 graph  F3 memories | Enter: search  ↑/↓  Tab: mode  Esc: quit"
        }
        View::Graph => "F1 search  F2 graph  F3 memories | Enter: analyze symbol  Esc: quit",
        View::Memories => "F1 search  F2 graph  F3 memories | Enter: recall  ↑/↓  Esc: quit",
    };
    f.render_widget(Paragraph::new(help).dim(), chunks[3]);
}

fn tabs_line(app: &App) -> Line<'static> {
    let tab = |name: &'static str, active: bool| {
        if active {
            Span::styled(
                format!(" {name} "),
                Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD),
            )
        } else {
            Span::raw(format!(" {name} "))
        }
    };
    Line::from(vec![
        tab("F1 Search", app.view == View::Search),
        Span::raw("  "),
        tab("F2 Graph", app.view == View::Graph),
        Span::raw("  "),
        tab("F3 Memories", app.view == View::Memories),
    ])
}

fn render_search(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let mid =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area);

    let items: Vec<ListItem> = app
        .results
        .iter()
        .map(|h| {
            let m = &h.point.metadata;
            let sym = if m.symbol.is_empty() { "-" } else { &m.symbol };
            ListItem::new(format!(
                "{:.3}  {}:{}-{}  {} [{}]",
                h.score, m.file, m.start_line, m.end_line, sym, m.chunk_level
            ))
        })
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title(app.status.clone()))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, mid[0], &mut app.list);

    let preview = app
        .selected()
        .map(|h| h.point.text.clone())
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(preview)
            .block(Block::bordered().title("Preview"))
            .wrap(Wrap { trim: false }),
        mid[1],
    );
}

fn render_graph(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let cols =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
    let fmt = |v: &[(String, usize)]| -> Vec<ListItem> {
        v.iter()
            .map(|(s, d)| ListItem::new(format!("{}{}  (depth {})", "  ".repeat(*d), s, d)))
            .collect()
    };
    f.render_widget(
        List::new(fmt(&app.upstream))
            .block(Block::bordered().title(format!("Callers / upstream ({})", app.upstream.len()))),
        cols[0],
    );
    f.render_widget(
        List::new(fmt(&app.downstream)).block(
            Block::bordered().title(format!("Callees / downstream ({})", app.downstream.len())),
        ),
        cols[1],
    );
}

fn render_memories(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let mid =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).split(area);

    let items: Vec<ListItem> = app
        .memories
        .iter()
        .map(|m| {
            let score = m.score.map(|s| format!("{s:.3}  ")).unwrap_or_default();
            let title = if m.title.is_empty() {
                "(untitled)"
            } else {
                &m.title
            };
            ListItem::new(format!("{score}[{}] {title}", m.mtype))
        })
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title(app.status.clone()))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, mid[0], &mut app.mem_list);

    let body = app
        .selected_memory()
        .map(|m| {
            let tags = if m.tags.is_empty() {
                String::new()
            } else {
                format!("\ntags: {}", m.tags)
            };
            format!("{}{}\n\n{}", m.title, tags, m.content)
        })
        .unwrap_or_default();
    f.render_widget(
        Paragraph::new(body)
            .block(Block::bordered().title("Memory"))
            .wrap(Wrap { trim: false }),
        mid[1],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use devctx_core::{VectorMetadata, VectorPoint};

    fn dummy(id: &str) -> SearchResult {
        SearchResult {
            score: 1.0,
            point: VectorPoint {
                id: id.into(),
                vector: vec![],
                text: id.into(),
                metadata: VectorMetadata::default(),
            },
        }
    }

    fn mem(title: &str) -> MemoryItem {
        MemoryItem {
            title: title.into(),
            mtype: "note".into(),
            tags: String::new(),
            content: title.into(),
            score: None,
        }
    }

    #[test]
    fn cycles_mode() {
        let mut app = App::new();
        assert_eq!(app.mode_name(), "vector");
        app.cycle_mode();
        assert_eq!(app.mode_name(), "keyword");
        app.cycle_mode();
        assert_eq!(app.mode_name(), "hybrid");
        app.cycle_mode();
        assert_eq!(app.mode_name(), "vector");
    }

    #[test]
    fn navigation_clamps() {
        let mut app = App::new();
        // No results: navigation is a no-op.
        app.next();
        assert_eq!(app.list.selected(), None);

        app.results = vec![dummy("a"), dummy("b")];
        app.list.select(Some(0));
        app.next();
        assert_eq!(app.list.selected(), Some(1));
        app.next(); // clamped at last
        assert_eq!(app.list.selected(), Some(1));
        app.prev();
        assert_eq!(app.list.selected(), Some(0));
        app.prev(); // clamped at first
        assert_eq!(app.list.selected(), Some(0));
        assert_eq!(app.selected().unwrap().point.id, "a");
    }

    #[test]
    fn navigation_targets_active_view() {
        let mut app = App::new();
        app.results = vec![dummy("a"), dummy("b")];
        app.memories = vec![mem("m1"), mem("m2"), mem("m3")];

        // In Memories view, navigation moves the memory list, not the search list.
        app.view = View::Memories;
        app.mem_list.select(Some(0));
        app.next();
        assert_eq!(app.mem_list.selected(), Some(1));
        assert_eq!(app.list.selected(), None);
        assert_eq!(app.selected_memory().unwrap().title, "m2");

        // In Graph view, navigation is a no-op.
        app.view = View::Graph;
        app.next();
        assert_eq!(app.mem_list.selected(), Some(1));
    }
}
