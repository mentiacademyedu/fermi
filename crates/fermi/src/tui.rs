//! Terminal UI: run several tasks at once, each in its own profile, manage
//! and log in to accounts, and watch everything live. Keyboard and mouse.
//! Thin client over `Engine`, like the future web dashboard.

use crate::setup;
use anyhow::Result;
use fermi_core::auth::{self, AuthInfo};
use fermi_core::state::AcctState;
use fermi_core::util::{fmt_dur, now};
use fermi_core::{Auth, Engine, Event, Task};
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event as Input, KeyCode,
    KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
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

#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Accounts,
    Tasks,
    Input,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Focus::Input => Focus::Accounts,
            Focus::Accounts => Focus::Tasks,
            Focus::Tasks => Focus::Input,
        }
    }
    fn prev(self) -> Self {
        match self {
            Focus::Input => Focus::Tasks,
            Focus::Tasks => Focus::Accounts,
            Focus::Accounts => Focus::Input,
        }
    }
}

/// Things a key, click or button can do.
#[derive(Clone, Copy, PartialEq)]
enum Act {
    None,
    Quit,
    Login,
    /// Top-bar button: asks which account first when there are several.
    LoginButton,
    AddAccount,
    Profile,
    Help,
    Cancel,
    Submit,
    NextFocus,
    TypeTask,
}

enum Modal {
    Help,
    ConfirmQuit,
    Add(Form),
}

/// Add-account form. Fields: 0 id, 1 provider, 2 login type, 3 key env (API only), 4 profile.
struct Form {
    field: usize,
    id: String,
    openai: bool,
    api: bool,
    key_env: String,
    profile: String,
    error: Option<String>,
}

impl Form {
    fn fields(&self) -> &'static [usize] {
        if self.api {
            &[0, 1, 2, 3, 4]
        } else {
            &[0, 1, 2, 4]
        }
    }
    fn step(&mut self, delta: isize) {
        let f = self.fields();
        let i = f.iter().position(|x| *x == self.field).unwrap_or(0) as isize;
        self.field = f[(i + delta).rem_euclid(f.len() as isize) as usize];
    }
    fn is_last(&self) -> bool {
        self.fields().last() == Some(&self.field)
    }
    fn text_mut(&mut self) -> Option<&mut String> {
        match self.field {
            0 => Some(&mut self.id),
            3 => Some(&mut self.key_env),
            4 => Some(&mut self.profile),
            _ => None,
        }
    }
    fn default_key_env(&self) -> &'static str {
        if self.openai {
            "OPENAI_API_KEY"
        } else {
            "ANTHROPIC_API_KEY"
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

type EvTx = mpsc::UnboundedSender<(usize, Event)>;
type ProbeTx = mpsc::UnboundedSender<(String, AuthInfo)>;

struct App {
    engine: Arc<Engine>,
    cwd: PathBuf,
    /// None = pick the profile from the working directory.
    profile: Option<String>,
    tasks: Vec<TaskView>,
    sel_task: usize,
    sel_acct: usize,
    focus: Focus,
    modal: Option<Modal>,
    input: String,
    scroll: usize,
    accts: HashMap<String, AcctState>,
    auth: HashMap<String, AuthInfo>,
    probing: Vec<String>,
    note: Option<(Kind, String)>,
    probe_tx: ProbeTx,
    /// Startup screen, shown while account logins are checked.
    splash: bool,
    opened: Instant,
    // Hit areas from the last draw, for mouse clicks.
    r_acc: Rect,
    r_tasks: Rect,
    r_out: Rect,
    r_input: Rect,
    buttons: Vec<(Rect, Act)>,
}

pub async fn run(engine: Arc<Engine>, profile: Option<String>) -> Result<()> {
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<(usize, Event)>();
    let (probe_tx, mut probe_rx) = mpsc::unbounded_channel();
    let mut app = App::new(engine, profile, std::env::current_dir()?, probe_tx);
    app.probe_all();

    let mut term = enter();
    let mut dirty = true;
    let mut last_tick = Instant::now();
    let mut last_frame = Instant::now();
    // Single-threaded input polling: nothing else reads the console, so the
    // TUI can hand the terminal to an interactive login and take it back.
    let res: Result<()> = loop {
        if dirty {
            if let Err(e) = term.draw(|f| app.draw(f)) {
                break Err(e.into());
            }
            dirty = false;
        }
        match tokio::task::block_in_place(|| event::poll(Duration::from_millis(40))) {
            Err(e) => break Err(e.into()),
            Ok(false) => {}
            Ok(true) => {
                let input = match event::read() {
                    Ok(i) => i,
                    Err(e) => break Err(e.into()),
                };
                dirty = true;
                match app.on_input(input, &ev_tx) {
                    Act::Quit => break Ok(()),
                    Act::Login => {
                        if let Some(id) = app.selected_account() {
                            leave();
                            tokio::task::block_in_place(|| login_flow(&id));
                            term = enter();
                            app.probe(&id);
                            app.note = Some((Kind::Dim, format!("checking login for {id}…")));
                        }
                    }
                    _ => {}
                }
            }
        }
        while let Ok((id, ev)) = ev_rx.try_recv() {
            app.on_event(id, ev);
            dirty = true;
        }
        while let Ok((id, info)) = probe_rx.try_recv() {
            app.on_probe(id, info);
            dirty = true;
        }
        if last_tick.elapsed() >= Duration::from_secs(1) {
            app.accts = app.engine.store.snapshot();
            last_tick = Instant::now();
            dirty = true;
        }
        if app.splash {
            let t = app.opened.elapsed();
            if (app.probing.is_empty() && t >= Duration::from_millis(1200)) || t >= Duration::from_secs(4) {
                app.splash = false;
            }
        }
        // Animate spinners only while something is in flight.
        if (app.splash || app.running() > 0) && last_frame.elapsed() >= Duration::from_millis(150) {
            last_frame = Instant::now();
            dirty = true;
        }
    };
    leave();
    for t in &app.tasks {
        t.handle.abort();
    }
    res
}

fn enter() -> DefaultTerminal {
    let t = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture, EnableBracketedPaste);
    t
}

fn leave() {
    let _ = execute!(std::io::stdout(), DisableMouseCapture, DisableBracketedPaste);
    ratatui::restore();
}

/// Runs the provider's own interactive login in the plain terminal.
fn login_flow(id: &str) {
    println!("\n── fermi · log in `{id}` ─────────────────────────────\n");
    println!("Follow the steps below (a browser window may open).\n");
    if let Err(e) = setup::login(id) {
        println!("\nlogin failed: {e:#}");
    }
    println!("\npress Enter to go back to fermi…");
    let _ = std::io::stdin().read_line(&mut String::new());
}

fn inside(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

impl App {
    fn new(engine: Arc<Engine>, profile: Option<String>, cwd: PathBuf, probe_tx: ProbeTx) -> Self {
        App {
            accts: engine.store.snapshot(),
            engine,
            cwd,
            profile,
            tasks: vec![],
            sel_task: 0,
            sel_acct: 0,
            focus: Focus::Input,
            modal: None,
            input: String::new(),
            scroll: 0,
            auth: HashMap::new(),
            probing: vec![],
            note: None,
            probe_tx,
            splash: true,
            opened: Instant::now(),
            r_acc: Rect::default(),
            r_tasks: Rect::default(),
            r_out: Rect::default(),
            r_input: Rect::default(),
            buttons: vec![],
        }
    }

    fn profile_label(&self) -> String {
        match self.engine.cfg.resolve_profile(self.profile.as_deref(), &self.cwd) {
            Ok(p) => {
                let how = if self.profile.is_some() { "chosen" } else { "from folder" };
                format!("{} · {} · {how}", p.name, format!("{:?}", p.mode).to_lowercase())
            }
            Err(_) => "none".into(),
        }
    }

    fn selected_account(&self) -> Option<String> {
        self.engine.cfg.accounts.get(self.sel_acct).map(|a| a.id.clone())
    }

    fn probe(&mut self, id: &str) {
        let Some(a) = self.engine.cfg.account(id).cloned() else { return };
        let (cfg, tx, id) = (self.engine.cfg.clone(), self.probe_tx.clone(), id.to_string());
        self.probing.push(id.clone());
        tokio::spawn(async move {
            let info = auth::probe(&cfg, &a).await;
            let _ = tx.send((id, info));
        });
    }

    fn probe_all(&mut self) {
        let ids: Vec<_> = self.engine.cfg.accounts.iter().map(|a| a.id.clone()).collect();
        for id in ids {
            self.probe(&id);
        }
    }

    fn on_probe(&mut self, id: String, info: AuthInfo) {
        let was_pending = self.note.as_ref().is_some_and(|(_, n)| n.contains(&format!("checking login for {id}")));
        if was_pending {
            self.note = Some(match (info.logged_in, &info.email) {
                (Some(true), Some(e)) => (Kind::Good, format!("✓ {id} is logged in as {e}")),
                (Some(true), None) => (Kind::Good, format!("✓ {id} is logged in")),
                (Some(false), _) => (Kind::Bad, format!("✗ {id} is still not logged in. Select it and press l to retry")),
                (None, _) => (Kind::Dim, format!("{id}: login state unknown")),
            });
        }
        self.probing.retain(|p| p != &id);
        self.auth.insert(id, info);
    }

    fn running(&self) -> usize {
        self.tasks.iter().filter(|t| t.st == St::Running).count()
    }

    // ── input ────────────────────────────────────────────────────────────

    fn on_input(&mut self, input: Input, ev_tx: &EvTx) -> Act {
        let is_press = matches!(&input, Input::Key(k) if k.kind == KeyEventKind::Press)
            || matches!(&input, Input::Mouse(m) if matches!(m.kind, MouseEventKind::Down(_)));
        if self.splash {
            // Any key or click skips the splash.
            self.splash &= !is_press;
            return Act::None;
        }
        if is_press && !self.note.as_ref().is_some_and(|(_, n)| n.starts_with("checking login")) {
            self.note = None;
        }
        match input {
            Input::Key(k) if k.kind == KeyEventKind::Press => {
                let act = self.on_key(k);
                self.apply(act, ev_tx)
            }
            Input::Mouse(m) => {
                let act = self.on_mouse(m);
                self.apply(act, ev_tx)
            }
            Input::Paste(s) => {
                let s = s.replace(['\r', '\n'], " ");
                match &mut self.modal {
                    Some(Modal::Add(f)) => {
                        if let Some(t) = f.text_mut() {
                            t.push_str(s.trim());
                        }
                    }
                    None => {
                        self.focus = Focus::Input;
                        self.input.push_str(&s);
                    }
                    _ => {}
                }
                Act::None
            }
            _ => Act::None,
        }
    }

    /// Acts that change UI state are handled here; Quit and Login go back to
    /// the main loop.
    fn apply(&mut self, act: Act, ev_tx: &EvTx) -> Act {
        match act {
            Act::Quit if self.running() > 0 && !matches!(self.modal, Some(Modal::ConfirmQuit)) => {
                self.modal = Some(Modal::ConfirmQuit);
                Act::None
            }
            Act::Quit => Act::Quit,
            Act::LoginButton if self.focus != Focus::Accounts && self.engine.cfg.accounts.len() > 1 => {
                self.focus = Focus::Accounts;
                self.note = Some((Kind::Meta, "pick the account with ↑↓ or a click, then press l (or click \"log in\" again)".into()));
                Act::None
            }
            Act::Login | Act::LoginButton => {
                if self.engine.cfg.accounts.get(self.sel_acct).is_some_and(|a| a.auth == Auth::Api) {
                    let var = self.engine.cfg.accounts[self.sel_acct].api_key_env.clone().unwrap_or_default();
                    self.note = Some((Kind::Warn, format!("API-key accounts don't log in: set {var} in your environment and restart fermi")));
                    return Act::None;
                }
                Act::Login
            }
            Act::AddAccount => {
                let profile = self.engine.cfg.resolve_profile(self.profile.as_deref(), &self.cwd).map(|p| p.name).unwrap_or_default();
                self.modal = Some(Modal::Add(Form {
                    field: 0,
                    id: String::new(),
                    openai: false,
                    api: false,
                    key_env: String::new(),
                    profile,
                    error: None,
                }));
                Act::None
            }
            Act::Profile => {
                let mut names: Vec<Option<String>> = vec![None];
                names.extend(self.engine.cfg.profiles.iter().map(|p| Some(p.name.clone())));
                let i = names.iter().position(|n| *n == self.profile).unwrap_or(0);
                self.profile = names[(i + 1) % names.len()].clone();
                self.note = Some((Kind::Dim, format!("new tasks use profile: {}", self.profile_label())));
                Act::None
            }
            Act::Help => {
                self.modal = Some(Modal::Help);
                Act::None
            }
            Act::Cancel => {
                self.cancel();
                Act::None
            }
            Act::Submit => {
                self.submit(ev_tx);
                Act::None
            }
            Act::NextFocus => {
                self.focus = self.focus.next();
                Act::None
            }
            Act::TypeTask => {
                self.focus = Focus::Input;
                Act::None
            }
            Act::None => Act::None,
        }
    }

    fn on_key(&mut self, k: KeyEvent) -> Act {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') {
            return Act::Quit;
        }
        if let Some(modal) = self.modal.take() {
            return self.on_modal_key(modal, k);
        }
        match k.code {
            KeyCode::Tab => return Act::NextFocus,
            KeyCode::BackTab => {
                self.focus = self.focus.prev();
                return Act::None;
            }
            KeyCode::F(1) => return Act::Help,
            KeyCode::PageUp => {
                self.scroll += 10;
                return Act::None;
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(10);
                return Act::None;
            }
            _ => {}
        }
        match self.focus {
            Focus::Input => match k.code {
                KeyCode::Enter => return Act::Submit,
                KeyCode::Esc if self.input.is_empty() => self.focus = Focus::Tasks,
                KeyCode::Esc => self.input.clear(),
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Up => self.select_task(-1),
                KeyCode::Down => self.select_task(1),
                KeyCode::Char(c) if !ctrl => self.input.push(c),
                _ => {}
            },
            Focus::Tasks => match k.code {
                KeyCode::Up => self.select_task(-1),
                KeyCode::Down => self.select_task(1),
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char('i') => return Act::TypeTask,
                KeyCode::Char('x') | KeyCode::Delete => return Act::Cancel,
                KeyCode::Char('a') => return Act::AddAccount,
                KeyCode::Char('p') => return Act::Profile,
                KeyCode::Char('?') => return Act::Help,
                KeyCode::Char('q') => return Act::Quit,
                _ => {}
            },
            Focus::Accounts => match k.code {
                KeyCode::Up => self.sel_acct = self.sel_acct.saturating_sub(1),
                KeyCode::Down => self.sel_acct = (self.sel_acct + 1).min(self.engine.cfg.accounts.len().saturating_sub(1)),
                KeyCode::Enter | KeyCode::Char('l') => return Act::Login,
                KeyCode::Esc | KeyCode::Char('i') => return Act::TypeTask,
                KeyCode::Char('a') => return Act::AddAccount,
                KeyCode::Char('p') => return Act::Profile,
                KeyCode::Char('?') => return Act::Help,
                KeyCode::Char('q') => return Act::Quit,
                _ => {}
            },
        }
        Act::None
    }

    fn on_modal_key(&mut self, modal: Modal, k: KeyEvent) -> Act {
        match modal {
            Modal::Help => Act::None, // any key closes
            Modal::ConfirmQuit => match k.code {
                KeyCode::Char('y') | KeyCode::Enter => Act::Quit,
                _ => Act::None,
            },
            Modal::Add(mut f) => {
                match k.code {
                    KeyCode::Esc => return Act::None,
                    KeyCode::Tab | KeyCode::Down => f.step(1),
                    KeyCode::BackTab | KeyCode::Up => f.step(-1),
                    KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if matches!(f.field, 1 | 2) => {
                        if f.field == 1 {
                            f.openai = !f.openai;
                        } else {
                            f.api = !f.api;
                        }
                        f.key_env = if f.api { f.default_key_env().into() } else { String::new() };
                    }
                    KeyCode::Enter if f.is_last() => return self.save_account(f),
                    KeyCode::Enter => f.step(1),
                    KeyCode::Backspace => {
                        if let Some(t) = f.text_mut() {
                            t.pop();
                        }
                    }
                    KeyCode::Char(c) => {
                        if let Some(t) = f.text_mut() {
                            t.push(c);
                        }
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Add(f));
                Act::None
            }
        }
    }

    fn save_account(&mut self, mut f: Form) -> Act {
        let provider = if f.openai { "openai" } else { "claude" };
        let key_env = f.api.then(|| f.key_env.trim().to_string());
        let profile = Some(f.profile.trim()).filter(|p| !p.is_empty());
        let result = setup::add_account(f.id.trim(), provider, key_env.as_deref(), None, profile)
            .and_then(|msg| Ok((msg, Engine::load()?)));
        match result {
            Ok((msg, engine)) => {
                self.engine = engine;
                self.sel_acct = self.engine.cfg.accounts.iter().position(|a| a.id == f.id.trim()).unwrap_or(0);
                self.focus = Focus::Accounts;
                self.note = Some((Kind::Good, msg));
                let id = f.id.trim().to_string();
                self.probe(&id);
                if f.api {
                    Act::None
                } else {
                    Act::Login // log the new account in right away
                }
            }
            Err(e) => {
                f.error = Some(format!("{e:#}"));
                self.modal = Some(Modal::Add(f));
                Act::None
            }
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) -> Act {
        let (x, y) = (m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(Modal::Help) = self.modal {
                    self.modal = None;
                    return Act::None;
                }
                if self.modal.is_some() {
                    return Act::None;
                }
                if let Some((_, act)) = self.buttons.iter().find(|(r, _)| inside(*r, x, y)) {
                    return *act;
                }
                if inside(self.r_acc, x, y) {
                    self.focus = Focus::Accounts;
                    let row = (y.saturating_sub(self.r_acc.y + 1) / 2) as usize;
                    if row < self.engine.cfg.accounts.len() {
                        self.sel_acct = row;
                    }
                } else if inside(self.r_tasks, x, y) {
                    self.focus = Focus::Tasks;
                    let row = y.saturating_sub(self.r_tasks.y + 1) as usize;
                    if row < self.tasks.len() {
                        self.sel_task = row;
                        self.scroll = 0;
                    }
                } else if inside(self.r_input, x, y) {
                    self.focus = Focus::Input;
                } else if inside(self.r_out, x, y) {
                    self.focus = Focus::Tasks;
                }
            }
            MouseEventKind::ScrollUp => self.scroll += 3,
            MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_sub(3),
            _ => {}
        }
        Act::None
    }

    fn select_task(&mut self, delta: isize) {
        if self.tasks.is_empty() {
            return;
        }
        let max = self.tasks.len() as isize - 1;
        self.sel_task = (self.sel_task as isize + delta).clamp(0, max) as usize;
        self.scroll = 0;
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
            self.note = Some((Kind::Warn, "type a task first".into()));
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
        self.sel_task = id;
        self.scroll = 0;
        self.note = None;
    }

    fn cancel(&mut self) {
        if let Some(t) = self.tasks.get_mut(self.sel_task).filter(|t| t.st == St::Running) {
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

    // ── drawing ──────────────────────────────────────────────────────────

    fn pane(&self, title: &str, focus: Focus) -> Block<'static> {
        let focused = self.focus == focus && self.modal.is_none();
        let style = if focused { Style::new().fg(Color::Cyan) } else { Style::new().fg(Color::DarkGray) };
        let title = if focused { format!(" {title} ").bold() } else { format!(" {title} ").into() };
        Block::bordered().border_style(style).title(title)
    }

    fn draw_splash(&self, f: &mut Frame) {
        let art: Vec<&str> = FERMI.lines().skip(1).collect();
        let w = art.iter().map(|l| l.chars().count()).max().unwrap_or(0);
        // Pad to equal width so centering keeps the drawing aligned.
        let mut lines: Vec<Line> = art.iter().map(|l| Line::from(format!("{l:<w$}")).fg(Color::Cyan)).collect();
        lines.push(Line::from(""));
        lines.push(Line::from("F  E  R  M  I".bold()));
        lines.push(Line::from("“Where is everybody?” · Enrico Fermi, 1901–1954".fg(Color::DarkGray).italic()));
        lines.push(Line::from(""));
        for a in &self.engine.cfg.accounts {
            let text = match self.auth.get(&a.id) {
                _ if self.probing.contains(&a.id) => format!("{} checking {}…", spin(self.opened), a.id),
                Some(i) if i.logged_in == Some(false) => format!("○ {} · not logged in", a.id),
                Some(i) => format!("✓ {} {}", a.id, i.email.as_deref().map(|e| format!("· {e}")).unwrap_or_default()),
                None => format!("· {}", a.id),
            };
            lines.push(Line::from(text).fg(Color::DarkGray));
        }
        lines.push(Line::from(""));
        lines.push(Line::from("press any key".fg(Color::DarkGray)));
        let area = f.area();
        let h = (lines.len() as u16).min(area.height);
        let r = Rect::new(area.x, area.y + (area.height - h) / 2, area.width, h);
        f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), r);
    }

    fn draw(&mut self, f: &mut Frame) {
        if self.splash {
            self.draw_splash(f);
            return;
        }
        let [top, main, input, foot] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(6), Constraint::Length(3), Constraint::Length(1)]).areas(f.area());
        self.buttons.clear();
        self.draw_header(f, top);

        let [side, out] = Layout::horizontal([Constraint::Length(44), Constraint::Min(20)]).areas(main);
        let n_acc = self.engine.cfg.accounts.len().max(1) as u16;
        let [acc_area, task_area] = Layout::vertical([Constraint::Length(n_acc * 2 + 2), Constraint::Min(3)]).areas(side);
        (self.r_acc, self.r_tasks, self.r_out, self.r_input) = (acc_area, task_area, out, input);

        self.draw_accounts(f, acc_area);
        self.draw_tasks(f, task_area);
        self.draw_output(f, out);
        self.draw_input(f, input);
        self.draw_footer(f, foot);

        match &self.modal {
            Some(Modal::Help) => draw_help(f),
            Some(Modal::ConfirmQuit) => {
                let n = self.running();
                draw_box(f, 52, 5, " quit? ", vec![
                    Line::from(format!("{n} task(s) still running. Quitting stops them.")),
                    Line::from(""),
                    Line::from(vec!["y".bold().cyan(), " quit   ".into(), "any other key".bold().cyan(), " stay".into()]),
                ]);
            }
            Some(Modal::Add(form)) => draw_form(f, form),
            None => {}
        }
    }

    fn draw_header(&mut self, f: &mut Frame, area: Rect) {
        let left = Line::from(vec![
            " fermi ".bold().reversed(),
            format!("  profile: {}  ·  {} running", self.profile_label(), self.running()).into(),
        ]);
        f.render_widget(left, area);
        let buttons = [("+ account", Act::AddAccount), ("log in", Act::LoginButton), ("profile", Act::Profile), ("help", Act::Help), ("quit", Act::Quit)];
        let mut x = area.x + area.width;
        for (label, act) in buttons.iter().rev() {
            let w = label.chars().count() as u16 + 2;
            if x < area.x + w + 30 {
                break; // too narrow
            }
            x -= w + 1;
            let r = Rect::new(x, area.y, w, 1);
            f.render_widget(Span::styled(format!(" {label} "), Style::new().fg(Color::Black).bg(Color::Cyan)), r);
            self.buttons.push((r, *act));
        }
    }

    fn draw_accounts(&self, f: &mut Frame, area: Rect) {
        let t = now();
        let mut lines: Vec<Line> = vec![];
        if self.engine.cfg.accounts.is_empty() {
            lines.push(Line::from("no accounts yet. Press a to add one".fg(Color::DarkGray)));
        }
        for (i, a) in self.engine.cfg.accounts.iter().enumerate() {
            let s = self.accts.get(&a.id).cloned().unwrap_or_default();
            let info = self.auth.get(&a.id);
            let busy = self.engine.store.running(&a.id);
            let probing = self.probing.contains(&a.id);
            let logged_in = info.and_then(|i| i.logged_in).or(a.credentials_present());
            let (dot, color) = if s.limited(t) {
                ("●", Color::Red)
            } else if logged_in == Some(false) {
                ("○", Color::DarkGray)
            } else if busy > 0 {
                ("●", Color::Cyan)
            } else {
                ("●", Color::Green)
            };
            let selected = i == self.sel_acct && self.focus == Focus::Accounts;
            let id_style = if selected { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new().bold() };
            let mut right = vec![];
            if let Some(u) = s.utilization {
                right.push(format!("{:.0}% used", u * 100.0));
            }
            if busy > 0 {
                right.push(format!("{busy} running"));
            }
            lines.push(Line::from(vec![
                Span::styled(format!("{dot} "), Style::new().fg(color)),
                Span::styled(format!("{} ", a.id), id_style),
                Span::styled(format!("{} {}", a.provider.as_str(), right.join(" · ")), Style::new().fg(Color::DarkGray)),
            ]));
            let detail = if s.limited(t) {
                Span::styled(format!("  limited, resets in {}", fmt_dur(s.limited_until.unwrap_or(t) - t)), Style::new().fg(Color::Red))
            } else if probing && info.is_none() {
                Span::styled("  checking login…", Style::new().fg(Color::DarkGray))
            } else if logged_in == Some(false) {
                let hint = if a.auth == Auth::Api {
                    format!("  {} not set", a.api_key_env.as_deref().unwrap_or("key"))
                } else {
                    "  not logged in · select + l to log in".into()
                };
                Span::styled(hint, Style::new().fg(Color::LightRed))
            } else {
                let who = match info.map(|i| (&i.email, &i.plan)) {
                    Some((Some(e), Some(p))) => format!("{e} · {p}"),
                    Some((Some(e), None)) => e.clone(),
                    Some((None, Some(p))) => p.clone(),
                    _ => "logged in".into(),
                };
                Span::styled(format!("  {who}"), Style::new().fg(Color::DarkGray))
            };
            lines.push(Line::from(detail));
        }
        f.render_widget(Paragraph::new(lines).block(self.pane("accounts", Focus::Accounts)), area);
    }

    fn draw_tasks(&self, f: &mut Frame, area: Rect) {
        let w = area.width.saturating_sub(3) as usize;
        let mut lines: Vec<Line> = self
            .tasks
            .iter()
            .enumerate()
            .map(|(i, tk)| {
                let (glyph, color) = match tk.st {
                    St::Running => (spin(tk.started), Color::Cyan),
                    St::Done => ("✓", Color::Green),
                    St::Failed => ("✗", Color::Red),
                };
                let secs = tk.elapsed.unwrap_or_else(|| tk.started.elapsed()).as_secs();
                let text: String = format!(" #{} {} · {}", i + 1, fmt_dur(secs), tk.title).chars().take(w).collect();
                let style = if i == self.sel_task { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() };
                Line::from(vec![Span::styled(glyph, Style::new().fg(color)), Span::styled(text, style)])
            })
            .collect();
        if lines.is_empty() {
            lines.push(Line::from("no tasks yet".fg(Color::DarkGray)));
        }
        f.render_widget(Paragraph::new(lines).block(self.pane("tasks", Focus::Tasks)), area);
    }

    fn draw_output(&self, f: &mut Frame, area: Rect) {
        let block = Block::bordered().border_style(Style::new().fg(Color::DarkGray));
        match self.tasks.get(self.sel_task) {
            Some(tk) => {
                let mut body = self.output_lines(tk, area.width.saturating_sub(2) as usize, area.height.saturating_sub(2) as usize);
                let waiting = tk.st == St::Running && !tk.lines.iter().any(|(k, _)| matches!(k, Kind::Text | Kind::Tool));
                if waiting {
                    body.push(Line::from(""));
                    for (i, art) in FERMI_MINI.iter().enumerate() {
                        let side = match i {
                            1 => format!("   {} thinking…", spin(tk.started)),
                            2 => format!("   {}", tk.route),
                            _ => String::new(),
                        };
                        body.push(Line::from(vec![
                            Span::styled(format!("{art:<12}"), Style::new().fg(Color::Cyan)),
                            Span::styled(side, Kind::Dim.style()),
                        ]));
                    }
                }
                f.render_widget(Paragraph::new(body).block(block.title(format!(" {} ", tk.route))), area);
            }
            None => {
                let d = Kind::Dim.style();
                let lines = vec![
                    Line::from("Welcome to fermi.".bold()),
                    Line::from(""),
                    Line::from("1. Type a task in the box at the bottom and press Enter."),
                    Line::from("2. Run more tasks while others work. They run in parallel."),
                    Line::from("3. Click a task on the left (or use ↑↓) to see its output."),
                    Line::from(""),
                    Line::from(Span::styled("Accounts: click one, then \"log in\". Add more with \"+ account\".", d)),
                    Line::from(Span::styled("Tab moves between panels. F1 or \"help\" shows every key.", d)),
                    Line::from(""),
                    Line::from(Span::styled("Optional prefixes: @work (profile)  m=claude:opus (model)  a=claude-work (account)", d)),
                ];
                f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(block.title(" output ")), area);
            }
        }
    }

    fn draw_input(&self, f: &mut Frame, area: Rect) {
        let inner_w = area.width.saturating_sub(2) as usize;
        let chars: Vec<char> = self.input.chars().collect();
        let shown: String = chars[chars.len().saturating_sub(inner_w.saturating_sub(1))..].iter().collect();
        let body = if shown.is_empty() && self.focus != Focus::Input {
            Line::from("click here or press Tab to type a task".fg(Color::DarkGray))
        } else {
            Line::from(shown.clone())
        };
        f.render_widget(Paragraph::new(body).block(self.pane("task — Enter to run", Focus::Input)), area);
        if self.focus == Focus::Input && self.modal.is_none() {
            f.set_cursor_position((area.x + 1 + shown.chars().count() as u16, area.y + 1));
        }
    }

    fn draw_footer(&mut self, f: &mut Frame, area: Rect) {
        if let Some((kind, note)) = &self.note {
            f.render_widget(Line::from(Span::styled(format!(" {note}"), kind.style())), area);
            return;
        }
        let hints: &[(&str, &str, Act)] = match self.focus {
            Focus::Input => &[("Enter", "run", Act::Submit), ("Tab", "next panel", Act::NextFocus), ("Esc", "leave box", Act::None), ("F1", "help", Act::Help)],
            Focus::Tasks => &[("↑↓", "select", Act::None), ("x", "cancel task", Act::Cancel), ("i", "type", Act::TypeTask), ("Tab", "next panel", Act::NextFocus), ("q", "quit", Act::Quit)],
            Focus::Accounts => &[("↑↓", "select", Act::None), ("l", "log in", Act::Login), ("a", "add", Act::AddAccount), ("p", "profile", Act::Profile), ("Tab", "next panel", Act::NextFocus), ("q", "quit", Act::Quit)],
        };
        let mut x = area.x + 1;
        let mut spans = vec![Span::raw(" ")];
        for (key, label, act) in hints {
            let text_w = (key.chars().count() + label.chars().count() + 2) as u16;
            spans.push(Span::styled(key.to_string(), Style::new().fg(Color::Cyan).bold()));
            spans.push(Span::styled(format!(" {label}"), Style::new().fg(Color::DarkGray)));
            spans.push(Span::raw("  "));
            if *act != Act::None {
                self.buttons.push((Rect::new(x, area.y, text_w, 1), *act));
            }
            x += text_w + 1;
        }
        f.render_widget(Line::from(spans), area);
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

/// Enrico Fermi, 1901–1954.
const FERMI: &str = r#"
            ,;;;;;;;;;;;;;;,
         ,;;;;;;;;;;;;;;;;;;;;,
        ;;;;;''''      ''''';;;;
       ;;;;'                 ';;;
       ;;;'   ____      ____   ';;
       ;;    '----'    '----'   ;;
      (;;    -(o)-      -(o)-   ;;)
       ;;          /             ;
        ;         /__,          ;
         ;      ._____.        ;
          '.     '---'       .'
            '-.___________.-'
               __|       |__
          _.-''  \  \ /  /  ''-._
         /        \  V  /        \
        /    |     \ | /     |    \
"#;

/// Small Fermi shown while a task has no output yet.
const FERMI_MINI: [&str; 4] = ["  ,;;;;;;,", " ;' o  o ';", " ;   __   ;", "  '-....-'"];

const SPINNER: [&str; 4] = ["◐", "◓", "◑", "◒"];

fn spin(started: Instant) -> &'static str {
    SPINNER[(started.elapsed().as_millis() / 150) as usize % SPINNER.len()]
}

fn centered(f: &Frame, w: u16, h: u16) -> Rect {
    let a = f.area();
    let (w, h) = (w.min(a.width), h.min(a.height));
    Rect::new(a.x + (a.width - w) / 2, a.y + (a.height - h) / 2, w, h)
}

fn draw_box(f: &mut Frame, w: u16, h: u16, title: &str, lines: Vec<Line>) {
    let r = centered(f, w, h + 2);
    f.render_widget(Clear, r);
    let block = Block::bordered().border_style(Style::new().fg(Color::Cyan)).title(title.to_string().bold());
    f.render_widget(Paragraph::new(lines).block(block), r);
}

fn draw_help(f: &mut Frame) {
    let k = |key: &str, what: &str| Line::from(vec![Span::styled(format!(" {key:<16}"), Style::new().fg(Color::Cyan).bold()), Span::raw(what.to_string())]);
    let lines = vec![
        k("Tab / Shift+Tab", "move between panels: accounts, tasks, task box"),
        k("mouse", "click panels, rows and buttons; wheel scrolls output"),
        k("Enter", "run the task in the box"),
        k("Esc", "leave the task box"),
        k("↑ ↓", "select a task or account"),
        k("PgUp / PgDn", "scroll the output"),
        k("x", "cancel the selected task (tasks panel)"),
        k("l / Enter", "log in the selected account (accounts panel)"),
        k("a", "add an account"),
        k("p", "switch the profile for new tasks"),
        k("Ctrl+C / q", "quit"),
        Line::from(""),
        Line::from(" Task prefixes:  @work  m=claude:opus  a=claude-work".fg(Color::DarkGray)),
        Line::from(" Select text: hold Shift while dragging.".fg(Color::DarkGray)),
        Line::from(""),
        Line::from(" any key or click closes this".fg(Color::DarkGray)),
    ];
    let n = lines.len() as u16;
    draw_box(f, 78, n, " help ", lines);
}

fn draw_form(f: &mut Frame, form: &Form) {
    let row = |idx: usize, label: &str, value: Span<'static>, hint: &str| {
        let active = form.field == idx;
        let marker = if active { "›" } else { " " };
        Line::from(vec![
            Span::styled(format!(" {marker} {label:<10}"), if active { Style::new().fg(Color::Cyan).bold() } else { Style::new() }),
            value,
            Span::styled(format!("  {hint}"), Style::new().fg(Color::DarkGray)),
        ])
    };
    let text = |s: &str, active: bool, placeholder: &str| {
        if s.is_empty() && !active {
            Span::styled(placeholder.to_string(), Style::new().fg(Color::DarkGray))
        } else {
            Span::styled(format!("{s}{}", if active { "▏" } else { "" }), Style::new().add_modifier(Modifier::UNDERLINED))
        }
    };
    let choice = |s: &str| Span::styled(format!("‹ {s} ›"), Style::new().fg(Color::Yellow));
    let mut lines = vec![
        Line::from(" Add an account. It gets its own separate login.".fg(Color::DarkGray)),
        Line::from(""),
        row(0, "Name", text(&form.id, form.field == 0, "e.g. claude-work"), ""),
        row(1, "Provider", choice(if form.openai { "OpenAI (ChatGPT / Codex)" } else { "Claude" }), "←→ change"),
        row(2, "Login", choice(if form.api { "API key" } else { "subscription login" }), "←→ change"),
    ];
    if form.api {
        lines.push(row(3, "Key env", text(&form.key_env, form.field == 3, form.default_key_env()), "env var holding the key"));
    }
    lines.push(row(4, "Profile", text(&form.profile, form.field == 4, "personal"), "work, personal…"));
    lines.push(Line::from(""));
    match &form.error {
        Some(e) => lines.push(Line::from(Span::styled(format!(" {e}"), Style::new().fg(Color::Red)))),
        None => lines.push(Line::from(
            match (form.is_last(), form.api) {
                (true, false) => " Enter save and log in · Esc cancel",
                (true, true) => " Enter save · Esc cancel",
                _ => " Enter next · ↑↓ move · Esc cancel",
            }
            .fg(Color::DarkGray),
        )),
    }
    let n = lines.len() as u16;
    draw_box(f, 72, n, " + account ", lines);
}

fn wrap(s: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    if width == 0 || chars.is_empty() {
        return vec![String::new()];
    }
    chars.chunks(width).map(|c| c.iter().collect()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fermi_core::config::Config;
    use fermi_core::state::Store;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyEventState, MouseEvent};
    use ratatui::Terminal;

    const CFG: &str = r#"
        default_profile = "personal"
        [[accounts]]
        id = "claude-personal"
        provider = "claude"
        home = "default"
        [[accounts]]
        id = "claude-work"
        provider = "claude"
        [[profiles]]
        name = "personal"
        accounts = ["claude-personal"]
        [[profiles]]
        name = "work"
        accounts = ["claude-work"]
    "#;

    fn app() -> (App, EvTx) {
        let cfg = Config::parse(CFG).unwrap();
        let home = std::env::temp_dir().join(format!("fermi-tui-test-{}", std::process::id()));
        let engine = Arc::new(Engine { catalog: cfg.catalog(), store: Store::open(&home), cfg, home, depth: 0 });
        let (probe_tx, _) = mpsc::unbounded_channel();
        let (ev_tx, _) = mpsc::unbounded_channel();
        let mut a = App::new(engine, None, PathBuf::from("/"), probe_tx);
        a.auth.insert("claude-personal".into(), AuthInfo { logged_in: Some(true), email: Some("me@example.com".into()), plan: Some("pro".into()) });
        a.auth.insert("claude-work".into(), AuthInfo { logged_in: Some(false), ..Default::default() });
        (a, ev_tx)
    }

    fn key(a: &mut App, tx: &EvTx, code: KeyCode) -> Act {
        let k = KeyEvent { code, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE };
        a.on_input(Input::Key(k), tx)
    }

    fn render(a: &mut App) -> String {
        let mut t = Terminal::new(TestBackend::new(120, 26)).unwrap();
        t.draw(|f| a.draw(f)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn screens() {
        let (mut a, tx) = app();
        let splash = render(&mut a);
        println!("{splash}\n");
        assert!(splash.contains("F  E  R  M  I") && splash.contains("me@example.com"));
        assert!(key(&mut a, &tx, KeyCode::Char('x')) == Act::None && !a.splash);

        let main = render(&mut a);
        println!("{main}\n");
        assert!(main.contains("me@example.com · pro"));
        assert!(main.contains("not logged in"));
        assert!(main.contains("Welcome to fermi"));

        // Tab into accounts, move to the second, log in.
        key(&mut a, &tx, KeyCode::Tab);
        assert!(a.focus == Focus::Accounts);
        key(&mut a, &tx, KeyCode::Down);
        assert!(key(&mut a, &tx, KeyCode::Char('l')) == Act::Login);
        assert_eq!(a.selected_account().as_deref(), Some("claude-work"));

        // Add-account form: type a name, flip provider, move through fields.
        key(&mut a, &tx, KeyCode::Char('a'));
        for c in "openai-me".chars() {
            key(&mut a, &tx, KeyCode::Char(c));
        }
        key(&mut a, &tx, KeyCode::Enter);
        key(&mut a, &tx, KeyCode::Right);
        let form = render(&mut a);
        println!("{form}\n");
        assert!(form.contains("openai-me") && form.contains("OpenAI"));
        key(&mut a, &tx, KeyCode::Esc);
        assert!(a.modal.is_none());

        // Help opens and any key closes it.
        key(&mut a, &tx, KeyCode::F(1));
        println!("{}\n", render(&mut a));
        key(&mut a, &tx, KeyCode::Char('z'));
        assert!(a.modal.is_none());

        // Esc in the task box never quits; q in a panel does (no tasks running).
        key(&mut a, &tx, KeyCode::Char('i'));
        assert!(a.focus == Focus::Input);
        assert!(key(&mut a, &tx, KeyCode::Esc) == Act::None);
        assert!(key(&mut a, &tx, KeyCode::Char('q')) == Act::Quit);

        // Profile cycling: folder → personal → work → folder.
        key(&mut a, &tx, KeyCode::Char('p'));
        assert_eq!(a.profile.as_deref(), Some("personal"));
        key(&mut a, &tx, KeyCode::Char('p'));
        assert_eq!(a.profile.as_deref(), Some("work"));
        key(&mut a, &tx, KeyCode::Char('p'));
        assert_eq!(a.profile, None);

        // Clicking the header "help" button opens help.
        let (r, _) = *a.buttons.iter().find(|(_, act)| *act == Act::Help).unwrap();
        let click = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: r.x + 1, row: r.y, modifiers: KeyModifiers::NONE };
        a.on_input(Input::Mouse(click), &tx);
        assert!(matches!(a.modal, Some(Modal::Help)));
    }
}
