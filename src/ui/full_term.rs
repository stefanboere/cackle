//! A fullscreen terminal user interface.

use crate::checker::Checker;
use crate::crate_index::CrateIndex;
use crate::events::AppEvent;
use crate::problem_store::ProblemStoreRef;
use anyhow::Result;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::widgets::Block;
use ratatui::widgets::BorderType;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::List;
use ratatui::widgets::ListItem;
use ratatui::widgets::ListState;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use ratatui::Frame;
use std::io::Stdout;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::mpsc::TryRecvError;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

mod problems_ui;

pub(crate) struct FullTermUi {
    config_path: PathBuf,
    abort_sender: Sender<()>,
    crate_index: Arc<CrateIndex>,
    checker: Arc<Mutex<Checker>>,
}

impl FullTermUi {
    pub(crate) fn new(
        config_path: PathBuf,
        checker: &Arc<Mutex<Checker>>,
        crate_index: Arc<CrateIndex>,
        abort_sender: Sender<()>,
    ) -> Result<Self> {
        Ok(Self {
            config_path,
            abort_sender,
            crate_index,
            checker: checker.clone(),
        })
    }
}

struct Terminal {
    term: ratatui::Terminal<CrosstermBackend<Stdout>>,
    // While our UI is active, we hold a lock on stderr. Our output threads try to acquire stderr
    // before sending through output from cargo and will thus block output while the UI is active.
    _output_lock: std::io::StderrLock<'static>,
}

impl Terminal {
    fn new() -> Result<Terminal> {
        crossterm::terminal::enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
        let backend = ratatui::backend::CrosstermBackend::new(stdout);
        let term = ratatui::Terminal::new(backend)?;
        let output_lock = std::io::stderr().lock();
        Ok(Self {
            term,
            _output_lock: output_lock,
        })
    }
}

impl super::UserInterface for FullTermUi {
    fn run(
        &mut self,
        problem_store: ProblemStoreRef,
        event_receiver: Receiver<AppEvent>,
    ) -> Result<()> {
        let mut screen = problems_ui::ProblemsUi::new(
            problem_store.clone(),
            self.crate_index.clone(),
            self.checker.clone(),
            self.config_path.clone(),
        );
        let mut needs_redraw = true;
        let mut error = None;
        match event_receiver.recv() {
            Ok(AppEvent::ProblemsAdded) => {}
            Err(..) | Ok(AppEvent::Shutdown) => return Ok(()),
        }
        let mut terminal = Terminal::new()?;
        loop {
            if screen.quit_requested() {
                let pstore = &mut problem_store.lock();
                let _ = self.abort_sender.send(());
                // Give cargo a chance to exit before we tell the problem store to abort, otherwise
                // cargo might get to see its subprocesses failing which would pollute our output
                // with confusing messages.
                std::thread::sleep(Duration::from_millis(20));
                pstore.abort();
                // We don't return yet, but rather wait until we get an AppEvent::Shutdown.
            }
            if needs_redraw {
                if screen.needs_cursor() {
                    terminal.term.show_cursor()?;
                } else {
                    terminal.term.hide_cursor()?;
                }
                terminal.term.draw(|f| {
                    screen.render(f);
                    if let Some(e) = error.as_ref() {
                        render_error(f, e);
                    }
                })?;
                needs_redraw = false;
            }
            match event_receiver.try_recv() {
                Ok(AppEvent::ProblemsAdded) => {
                    needs_redraw = true;
                    if let Err(e) = screen.problems_added() {
                        error = Some(e);
                    }
                }
                Ok(AppEvent::Shutdown) => {
                    return Ok(());
                }
                Err(TryRecvError::Disconnected) => return Ok(()),
                Err(TryRecvError::Empty) => {
                    // TODO: Consider spawning a separate thread to read crossterm events, then feed
                    // them into the main event channel. That way we can avoid polling.
                    if crossterm::event::poll(Duration::from_millis(100))? {
                        needs_redraw = true;
                        let Ok(Event::Key(key)) = crossterm::event::read() else {
                            continue;
                        };
                        // When we're displaying an error, any key will dismiss the error popup. The key
                        // should then be ignored.
                        if error.take().is_some() {
                            // But still process the quit key, since if the error came from
                            // rendering, we'd like a way to get out.
                            if key.code == KeyCode::Char('q') {
                                problem_store.lock().abort();
                            }
                            continue;
                        }
                        if let Err(e) = screen.handle_key(key) {
                            error = Some(e);
                        }
                    }
                }
            }
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            self.term.backend_mut(),
            crossterm::terminal::LeaveAlternateScreen
        );
    }
}

fn render_build_progress(f: &mut Frame<CrosstermBackend<Stdout>>, area: Rect) {
    let block = Block::default()
        .title("Building")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let paragraph = Paragraph::new("Build in progress...")
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(Clear, area);
    f.render_widget(paragraph, area);
}

fn render_error(f: &mut Frame<CrosstermBackend<Stdout>>, error: &anyhow::Error) {
    let area = message_area(f.size());
    let block = Block::default()
        .title("Error")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red));
    let paragraph = Paragraph::new(format!("{error:#}"))
        .block(block)
        .wrap(Wrap { trim: false });
    f.render_widget(Clear, area);
    f.render_widget(paragraph, area);
}

fn message_area(area: Rect) -> Rect {
    centre_area(area, 80, 25)
}

fn centre_area(area: Rect, width: u16, height: u16) -> Rect {
    let vertical_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(centre(height, area.height))
        .split(area);

    let horizontal_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(centre(width, area.width))
        .split(vertical_chunks[1]);
    horizontal_chunks[1]
}

fn centre(target: u16, available: u16) -> Vec<Constraint> {
    let actual = target.min(available);
    let margin = (available - actual) / 2;
    vec![
        Constraint::Length(margin),
        Constraint::Length(actual),
        Constraint::Length(margin),
    ]
}

fn render_list(
    f: &mut Frame<CrosstermBackend<Stdout>>,
    title: &str,
    items: impl Iterator<Item = ListItem<'static>>,
    active: bool,
    area: Rect,
    index: usize,
) {
    let items: Vec<_> = items.collect();
    let mut block = Block::default().title(title).borders(Borders::ALL);
    if active {
        block = block
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(Color::Yellow));
    }
    let mut style = Style::default().add_modifier(Modifier::REVERSED);
    if active {
        style = style.fg(Color::Yellow);
    }
    let list = List::new(items).block(block).highlight_style(style);
    let mut list_state = ListState::default();
    list_state.select(Some(index));
    f.render_stateful_widget(list, area, &mut list_state);
}

/// Increment or decrement `counter`, wrapping at `len`. `keycode` must be Down or Up.
fn update_counter(counter: &mut usize, key_code: KeyCode, len: usize) {
    match key_code {
        KeyCode::Up => *counter = (*counter + len - 1) % len,
        KeyCode::Down => *counter = (*counter + len + 1) % len,
        _ => panic!("Invalid call to update_counter"),
    }
}
