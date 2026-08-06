//! `devctx-tui` — an interactive terminal UI for DevCtxEngine (ratatui).
//!
//! A live search browser: type a query, switch retrieval mode (vector / keyword /
//! hybrid), browse ranked results and preview the selected chunk. Reranking is
//! skipped for responsiveness. See `docs/rust-rewrite-plan.md`.

use devctx_core::config::ProjectConfig;
use devctx_core::{SearchFilter, SearchResult};
use devctx_embed::{create_provider, EmbedSettings, EmbeddingProvider};
use devctx_search::{search as run_search, SearchMode};
use devctx_store::Store;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

/// Results fetched per search.
const LIMIT: usize = 20;

/// The search engine backing the UI (embedder + store; no reranker for speed).
struct Engine {
    store: Store,
    embedder: Box<dyn EmbeddingProvider>,
    filter: SearchFilter,
}

impl Engine {
    fn build(cfg: &ProjectConfig) -> anyhow::Result<Self> {
        let embedder = create_provider(&EmbedSettings::from_config(&cfg.embeddings))?;
        let store = Store::open(&cfg.db_path(), embedder.dimension())?;
        Ok(Self {
            store,
            embedder,
            filter: SearchFilter {
                exclude_deletions: true,
                ..Default::default()
            },
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
}

/// UI state.
struct App {
    query: String,
    mode: SearchMode,
    results: Vec<SearchResult>,
    list: ListState,
    status: String,
}

impl App {
    fn new() -> Self {
        Self {
            query: String::new(),
            mode: SearchMode::Vector,
            results: Vec::new(),
            list: ListState::default(),
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

    fn next(&mut self) {
        if self.results.is_empty() {
            return;
        }
        let i = self
            .list
            .selected()
            .map(|i| (i + 1).min(self.results.len() - 1))
            .unwrap_or(0);
        self.list.select(Some(i));
    }

    fn prev(&mut self) {
        if self.results.is_empty() {
            return;
        }
        let i = self
            .list
            .selected()
            .map(|i| i.saturating_sub(1))
            .unwrap_or(0);
        self.list.select(Some(i));
    }

    fn selected(&self) -> Option<&SearchResult> {
        self.list.selected().and_then(|i| self.results.get(i))
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
                KeyCode::Enter => {
                    app.status = "Searching…".to_string();
                    terminal.draw(|f| ui(f, &mut app))?;
                    match engine.search(&app.query, app.mode) {
                        Ok(r) => {
                            app.results = r;
                            let sel = (!app.results.is_empty()).then_some(0);
                            app.list.select(sel);
                            app.status = format!("{} results", app.results.len());
                        }
                        Err(e) => {
                            app.results.clear();
                            app.list.select(None);
                            app.status = format!("error: {e}");
                        }
                    }
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

fn ui(f: &mut Frame, app: &mut App) {
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .split(f.area());

    let input = Paragraph::new(app.query.as_str()).block(
        Block::bordered().title(format!("Query — mode: {} (Tab to change)", app.mode_name())),
    );
    f.render_widget(input, chunks[0]);

    let mid = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(chunks[1]);

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
    let preview = Paragraph::new(preview)
        .block(Block::bordered().title("Preview"))
        .wrap(Wrap { trim: false });
    f.render_widget(preview, mid[1]);

    let help = Paragraph::new("Enter: search   ↑/↓: navigate   Tab: mode   Esc: quit").dim();
    f.render_widget(help, chunks[2]);
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
}
