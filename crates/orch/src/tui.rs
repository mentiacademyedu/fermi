//! Terminal UI: run several tasks at once, each in its own profile, and
//! watch accounts and output live. Thin client over `Engine`, like the
//! future web dashboard.

use anyhow::Result;
use orch_core::state::AcctState;
use orch_core::util::{fmt_dur, now};
use orch_core::{Engine, Event, Task};
use ratatui::crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event as Input, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[derive(PartialEq)]
enum St {
    Running,
    Done,
    Failed,
}

#[derive(Clone, Copy)]
enum Kind {
    Meta,
    Text,
    Tool,
    Warn,
    Good,
    Bad,
    Dim,
}

impl Kind {
    fn style(self) -> Style {
        match self {
            Kind::Meta => Style::new().fg(Color::Yellow),
            Kind::Text => Style::new(),
            Kind::Tool => Style::new().fg(Color::Cyan),
            Kind::Warn => Style::new().fg(Color::LightRed),
            Kind::Good => Style::new().fg(Color::Green),
            Kind::Bad => Style::new().fg(Color::Red),
            Kind::Dim => Style::new().fg(Color::DarkGray),
        }
    }
}

struct TaskView {
    title: String,
    route: String,
    st: St,
    lines: Vec<(Kind, String)>,
    started: Instant,
    elapsed: Option<Duration>,
    handle: JoinHandle<()>,
}

struct App {
    engine: Arc<Engine>,
    cwd: PathBuf,
    profile: Option<String>,
    here: String,
    tasks: Vec<TaskView>,
    sel: usize,
    input: String,
    scroll: usize,
    accts: HashMap<String, AcctState>,
}

type EvTx = mpsc::UnboundedSender<(usize, Event)>;

pub async fn run(engine: Arc<Engine>, profile: Option<String>) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let here = match engine.cfg.resolve_profile(profile.as_deref(), &cwd) {
        Ok(p) => format!("{} ({})", p.name, format!("{:?}", p.mode).to_lowercase()),
        Err(e) => format!("no profile: {e}"),
    };
    let mut app = App { accts: engine.store.snapshot(), engine, cwd, profile, here, tasks: vec![], sel: 0, input: String::new(), scroll: 0 };

    let (key_tx, mut key_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(ev) = event::read() {
            if key_tx.send(ev).is_err() {
                break;
            }
        }
    });
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<(usize, Event)>();

    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableBracketedPaste);
    let mut tick = tokio::time::interval(Duration::from_millis(1000));
    let res: Result<()> = loop {
        if let Err(e) = terminal.draw(|f| app.draw(f)) {
            break Err(e.into());
        }
        tokio::select! {
            Some(input) = key_rx.recv() => if app.on_input(input, &ev_tx) { break Ok(()) },
            Some((id, ev)) = ev_rx.recv() => {
                app.on_event(id, ev);
                while let Ok((id, ev)) = ev_rx.try_recv() {
                    app.on_event(id, ev);
                }
            }
            _ = tick.tick() => app.accts = app.engine.store.snapshot(),
        }
    };
    let _ = execute!(std::io::stdout(), DisableBracketedPaste);
    ratatui::restore();
    for t in &app.tasks {
        t.handle.abort();
    }
    res
}

impl App {
    /// Returns true to quit.
    fn on_input(&mut self, input: Input, ev_tx: &EvTx) -> bool {
        let key = match input {
            Input::Key(k) if k.kind == KeyEventKind::Press => k,
            Input::Paste(s) => {
                self.input.push_str(&s.replace(['\r', '\n'], " "));
                return false;
            }
            _ => return false,
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => return true,
            KeyCode::Char('x') if ctrl => self.cancel(),
            KeyCode::Esc if self.input.is_empty() => return true,
            KeyCode::Esc => self.input.clear(),
            KeyCode::Enter => self.submit(ev_tx),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Up => {
                self.sel = self.sel.saturating_sub(1);
                self.scroll = 0;
            }
            KeyCode::Down => {
                self.sel = (self.sel + 1).min(self.tasks.len().saturating_sub(1));
                self.scroll = 0;
            }
            KeyCode::PageUp => self.scroll += 10,
            KeyCode::PageDown => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Char(c) if !ctrl => self.input.push(c),
            _ => {}
        }
        false
    }

    /// Input syntax: optional leading `@profile`, `m=model`, `a=account`, then the prompt.
    fn submit(&mut self, ev_tx: &EvTx) {
        let raw = std::mem::take(&mut self.input);
        let mut task = Task::new("", self.cwd.clone());
        task.profile = self.profile.clone();
        let mut rest = raw.trim();
        loop {
            let (tok, after) = rest.split_once(' ').unwrap_or((rest, ""));
            if let Some(p) = tok.strip_prefix('@').filter(|p| !p.is_empty()) {
                task.profile = Some(p.into());
            } else if let Some(m) = tok.strip_prefix("m=") {
                task.model = Some(m.into());
            } else if let Some(a) = tok.strip_prefix("a=") {
                task.account = Some(a.into());
            } else {
                break;
            }
            rest = after.trim_start();
        }
        if rest.is_empty() {
            return;
        }
        task.prompt = rest.to_string();

        let id = self.tasks.len();
        let (engine, tx) = (self.engine.clone(), ev_tx.clone());
        let handle = tokio::spawn(async move {
            let (t_tx, mut t_rx) = mpsc::unbounded_channel();
            let fwd = tokio::spawn(async move {
                while let Some(e) = t_rx.recv().await {
                    if tx.send((id, e)).is_err() {
                        break;
                    }
                }
            });
            let _ = engine.run(&task, &t_tx).await;
            drop(t_tx);
            let _ = fwd.await;
        });
        self.tasks.push(TaskView {
            title: rest.to_string(),
            route: "routing…".into(),
            st: St::Running,
            lines: vec![(Kind::Dim, format!("$ {raw}"))],
            started: Instant::now(),
            elapsed: None,
            handle,
        });
        self.sel = id;
        self.scroll = 0;
    }

    fn cancel(&mut self) {
        if let Some(t) = self.tasks.get_mut(self.sel).filter(|t| t.st == St::Running) {
            t.handle.abort(); // drops the child process (kill_on_drop)
            t.st = St::Failed;
            t.elapsed = Some(t.started.elapsed());
            t.lines.push((Kind::Bad, "✗ cancelled".into()));
        }
    }

    fn on_event(&mut self, id: usize, ev: Event) {
        let Some(t) = self.tasks.get_mut(id) else { return };
        if t.st != St::Running {
            return; // cancelled
        }
        match ev {
            Event::Routed { profile, model, account, why } => {
                t.route = format!("{model} @ {account}");
                t.lines.push((Kind::Meta, format!("▸ {model} @ {account}  [{profile}]  {why}")));
            }
            Event::Text { text } => {
                t.lines.extend(text.lines().map(|l| (Kind::Text, l.to_string())));
                t.lines.push((Kind::Text, String::new()));
            }
            Event::Tool { name, detail } => t.lines.push((Kind::Tool, format!("· {name} {detail}"))),
            Event::Limit { account, utilization, blocked, .. } => {
                if blocked {
                    t.lines.push((Kind::Warn, format!("! {account} is rate limited")));
                } else if let Some(u) = utilization.filter(|u| *u >= 0.8) {
                    t.lines.push((Kind::Warn, format!("! {account} at {:.0}% of its window", u * 100.0)));
                }
            }
            Event::Failover { account, reason } => t.lines.push((Kind::Warn, format!("↷ {account}: {reason} — trying next"))),
            Event::Artifact { path } => t.lines.push((Kind::Good, format!("⇒ {path}"))),
            Event::Log { line } => t.lines.push((Kind::Dim, line)),
            Event::Done { ok, summary } => {
                t.st = if ok { St::Done } else { St::Failed };
                t.elapsed = Some(t.started.elapsed());
                t.lines.push((if ok { Kind::Good } else { Kind::Bad }, format!("{} {summary}", if ok { "✓" } else { "✗" })));
            }
        }
    }

    fn draw(&self, f: &mut Frame) {
        let [top, main, input, help] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(5), Constraint::Length(3), Constraint::Length(1)]).areas(f.area());
        let running = self.tasks.iter().filter(|t| t.st == St::Running).count();
        f.render_widget(
            Line::from(vec![
                " orch ".bold().reversed(),
                format!("  profile {}  ·  {}  ·  {running} running", self.here, self.cwd.display()).into(),
            ]),
            top,
        );

        let [side, out] = Layout::horizontal([Constraint::Length(38), Constraint::Min(20)]).areas(main);
        let n_acc = self.engine.cfg.accounts.len() as u16;
        let [acc_area, task_area] = Layout::vertical([Constraint::Length(n_acc + 2), Constraint::Min(3)]).areas(side);

        let t = now();
        let acc_lines: Vec<Line> = self
            .engine
            .cfg
            .accounts
            .iter()
            .map(|a| {
                let s = self.accts.get(&a.id).cloned().unwrap_or_default();
                let busy = self.engine.store.running(&a.id);
                let (dot, color, note) = if s.limited(t) {
                    ("●", Color::Red, format!("limited {}", fmt_dur(s.limited_until.unwrap_or(t) - t)))
                } else if a.credentials_present() == Some(false) {
                    ("○", Color::DarkGray, "no login".into())
                } else {
                    let u = s.utilization.map(|u| format!("{:.0}%", u * 100.0)).unwrap_or_default();
                    ("●", if busy > 0 { Color::Cyan } else { Color::Green }, u)
                };
                Line::from(vec![
                    Span::styled(format!("{dot} "), Style::new().fg(color)),
                    Span::raw(format!("{:<18}", a.id)),
                    Span::styled(format!(" {note}{}", if busy > 0 { format!(" ×{busy}") } else { String::new() }), Style::new().fg(Color::DarkGray)),
                ])
            })
            .collect();
        f.render_widget(Paragraph::new(acc_lines).block(Block::bordered().title(" accounts ")), acc_area);

        let w = task_area.width.saturating_sub(2) as usize;
        let task_lines: Vec<Line> = self
            .tasks
            .iter()
            .enumerate()
            .map(|(i, tk)| {
                let (glyph, color) = match tk.st {
                    St::Running => ("◌", Color::Cyan),
                    St::Done => ("✓", Color::Green),
                    St::Failed => ("✗", Color::Red),
                };
                let secs = tk.elapsed.unwrap_or_else(|| tk.started.elapsed()).as_secs();
                let text: String = format!(" #{} {} · {}", i + 1, fmt_dur(secs), tk.title).chars().take(w.saturating_sub(2)).collect();
                let style = if i == self.sel { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
                Line::from(vec![Span::styled(glyph, Style::new().fg(color)), Span::styled(text, style)])
            })
            .collect();
        f.render_widget(Paragraph::new(task_lines).block(Block::bordered().title(" tasks ")), task_area);

        let (title, body) = match self.tasks.get(self.sel) {
            Some(tk) => (format!(" {} ", tk.route), self.output_lines(tk, out.width.saturating_sub(2) as usize, out.height.saturating_sub(2) as usize)),
            None => (" output ".into(), vec![Line::from(Span::styled(
                "Type a task below. Prefixes: @profile  m=claude:opus  a=account-id",
                Kind::Dim.style(),
            ))]),
        };
        f.render_widget(Paragraph::new(body).block(Block::bordered().title(title)), out);

        let inner_w = input.width.saturating_sub(2) as usize;
        let shown: String = {
            let chars: Vec<char> = self.input.chars().collect();
            chars[chars.len().saturating_sub(inner_w.saturating_sub(1))..].iter().collect()
        };
        f.render_widget(Paragraph::new(shown.as_str()).block(Block::bordered().title(" task ")), input);
        f.set_cursor_position((input.x + 1 + shown.chars().count() as u16, input.y + 1));
        f.render_widget(
            Line::from(" enter run · ↑↓ select · pgup/pgdn scroll · ctrl+x cancel · esc quit").fg(Color::DarkGray),
            help,
        );
    }

    fn output_lines(&self, tk: &TaskView, width: usize, height: usize) -> Vec<Line<'static>> {
        let mut rows: Vec<(Kind, String)> = vec![];
        for (k, l) in &tk.lines {
            for chunk in wrap(l, width) {
                rows.push((*k, chunk));
            }
        }
        let end = rows.len().saturating_sub(self.scroll.min(rows.len().saturating_sub(height)));
        let start = end.saturating_sub(height);
        rows[start..end].iter().map(|(k, s)| Line::from(Span::styled(s.clone(), k.style()))).collect()
    }
}

fn wrap(s: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    if width == 0 || chars.is_empty() {
        return vec![String::new()];
    }
    chars.chunks(width).map(|c| c.iter().collect()).collect()
}
