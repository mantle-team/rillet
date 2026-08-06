//! A terminal counter demonstrating the rillet view pattern.
//!
//! The service and the terminal share only the view: the service updates
//! its state and publishes snapshots; the terminal draws the latest
//! snapshot, redrawing when a new one arrives or a key is pressed.
//!
//! Because the two sides share nothing else, either can change without
//! affecting the other, and neither can block the other: drawing reads
//! views wait-free, and keys send commands without waiting.

use std::time::Duration;

use crossterm::event::{Event as TermEvent, KeyCode, KeyEventKind};
use futures::FutureExt;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use rillet::{CancellationToken, CheapClone};

/// A snapshot of the counter.
#[derive(Clone, PartialEq, CheapClone)]
struct CounterView {
    count: u32,
    target: u32,
    running: bool,
}

/// A counter stepping toward a target on its own clock.
#[rillet::service(view = CounterView)]
struct Counter {
    #[rillet(default)]
    count: u32,

    #[rillet(default = 30)]
    target: u32,

    #[rillet(default = true)]
    running: bool,
}

impl Counter {
    fn view(&self) -> CounterView {
        CounterView {
            count: self.count,
            target: self.target,
            running: self.running,
        }
    }
}

#[rillet::handlers]
impl Counter {
    #[rillet(command)]
    fn step(&mut self) {
        if self.running {
            self.count = (self.count + 1) % (self.target + 1);
        }
    }

    #[rillet(command)]
    fn toggle(&mut self) {
        self.running = !self.running;
    }

    #[rillet(command)]
    fn reset(&mut self) {
        self.count = 0;
    }

    /// Sends one `step` command every 100 milliseconds until cancelled.
    #[rillet(task)]
    async fn step_periodically(handle: CounterHandle, cancel: CancellationToken) {
        loop {
            let mut cancelled = std::pin::pin!(cancel.cancelled().fuse());
            let mut tick = std::pin::pin!(smol::Timer::after(Duration::from_millis(100)).fuse());
            futures::select! {
                _ = cancelled => break,
                _ = tick => handle.step(),
            }
        }
    }
}

fn main() -> std::io::Result<()> {
    let counter = Counter::new().spawn();
    let mut terminal = ratatui::init();
    let result = run(&mut terminal, &counter);
    ratatui::restore();
    counter.cancel().wait();
    result
}

/// Runs the draw-and-input loop until the user quits.
///
/// Each pass draws if anything changed, applies input, then asks the
/// watcher whether a newer view was published. However many views were
/// published during one pass, the watcher reports them as a single change,
/// so one redraw covers them all. A paused counter publishes nothing, so
/// an idle demo draws nothing.
fn run(terminal: &mut ratatui::DefaultTerminal, counter: &CounterHandle) -> std::io::Result<()> {
    let mut changes = counter.watch_view();
    let mut dirty = true;
    loop {
        if dirty {
            render(terminal, counter)?;
        }
        if let Input::Quit = read_input(counter)? {
            return Ok(());
        }
        dirty = changes.try_changed().is_some();
    }
}

/// Draws the latest view.
///
/// The view load is wait-free, so drawing never contends with the
/// service's mutations, however busy it is.
fn render(terminal: &mut ratatui::DefaultTerminal, counter: &CounterHandle) -> std::io::Result<()> {
    let view = counter.view();
    terminal.draw(|frame| draw(frame, &view))?;
    Ok(())
}

/// The outcome of one round of input.
enum Input {
    Continue,
    Quit,
}

/// Waits briefly for a key and sends the command it maps to.
///
/// Each key becomes one fire-and-forget command, and nothing waits for
/// the service to act on it. The short poll timeout doubles as the
/// loop's pacing while nothing is happening.
fn read_input(counter: &CounterHandle) -> std::io::Result<Input> {
    if crossterm::event::poll(Duration::from_millis(25))?
        && let TermEvent::Key(key) = crossterm::event::read()?
        && key.kind == KeyEventKind::Press
    {
        match key.code {
            KeyCode::Char('q') => return Ok(Input::Quit),
            KeyCode::Char(' ') => counter.toggle(),
            KeyCode::Char('r') => counter.reset(),
            _ => {}
        }
    }
    Ok(Input::Continue)
}

fn draw(frame: &mut Frame, view: &CounterView) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(frame.area());

    let state = if view.running { "running" } else { "paused" };
    let header = Paragraph::new(format!(
        "counter: {} / {} ({state})",
        view.count, view.target
    ))
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(header, rows[0]);

    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL))
        .gauge_style(Style::default().fg(Color::Green))
        .ratio(f64::from(view.count) / f64::from(view.target));
    frame.render_widget(gauge, rows[1]);

    let help = Paragraph::new("space: pause/resume   r: reset   q: quit")
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(help, rows[3]);
}
