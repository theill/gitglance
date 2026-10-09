mod ai;
mod git;
mod ignore;
mod ui;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::widgets::TableState;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const HELP: &str = "gitglance - overview of pending git changes in every repo below a folder

USAGE:
    gitglance [DIR] [--depth N] [--summarize] [--interval SECS] [--fetch-every MINS]

    DIR            folder to scan (default: current directory)
    -d, --depth N  how deep to look for repos below DIR (default: 2)
    -s, --summarize
                   ask the AI for a summary of every repo with changes on start
    --no-auto-summary
                   don't summarize a repo's changes on your behalf when you open it
                   (by default, opening a repo with changes for half a second does)
    -i, --interval SECS
                   re-check every repo this often, so changes made in other sessions show
                   up on their own (default: 5, 0 starts paused; w toggles it)
    -F, --fetch-every MINS
                   fetch every repo in the background this often, so `behind` stays
                   current (default: 5, 0 turns it off)

Press x on a repo to ignore it. Ignored repos are listed in DIR/.gitglanceignore, one
per line as their path relative to DIR (`unops/opportunityplus`). A line hides exactly
that repo, never repos nested below it.

Press c to commit everything in a repo (Claude drafts the message) and push,
P to push commits that are already made, or u to pull in commits you are behind on.
Press t for a shell in the selected repo; exit it to come back.

Press A to start a Claude Code session in the selected repo, or R to resume the last
one there (claude --continue). It opens in a new terminal window (xdg-terminal-exec,
$TERMINAL, or Terminal.app on macOS), so gitglance stays live next to it; without a
desktop it runs right here. Set GITGLANCE_AGENT_CMD to start something other than claude.

AI summaries run `claude -p` (model from GITGLANCE_MODEL, default haiku) and are
cached in ~/.cache/gitglance. Set GITGLANCE_AI_CMD to use any other command that
reads a prompt on stdin and prints the answer.";

pub enum SummaryState {
    None,
    Pending,
    Done(String),
    Failed(String),
}

pub struct Repo {
    pub name: String,
    /// Path relative to the scanned folder; empty for the folder itself.
    pub rel: String,
    pub path: PathBuf,
    pub status: Option<Result<git::Status, String>>,
    /// Prompt context for the AI; only present when there is something to summarize.
    pub context: Option<String>,
    pub summary: SummaryState,
    pub busy: bool,
    /// A background re-check is running; unlike `busy`, it shows no spinner.
    checking: bool,
    /// Bumped for every scan, so a slow, older result can't overwrite a newer one.
    gen: u64,
    /// When a re-check last found something different, so the list can mark what moved.
    pub changed_at: Option<Instant>,
    /// Commit activity for the detail page, loaded when the repo is opened.
    pub activity: Option<Result<git::Activity, String>>,
    pub ignored: bool,
}

impl Repo {
    fn new(root: &Path, path: &Path) -> Self {
        let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().into_owned();
        let name = if rel.is_empty() {
            root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| ".".into())
        } else {
            rel.clone()
        };
        Repo {
            name,
            rel,
            path: path.to_path_buf(),
            status: None,
            context: None,
            summary: SummaryState::None,
            busy: false,
            checking: false,
            gen: 0,
            changed_at: None,
            activity: None,
            ignored: false,
        }
    }

    pub fn attention(&self) -> bool {
        if self.ignored {
            return false;
        }
        match &self.status {
            Some(Ok(s)) => s.needs_attention(),
            Some(Err(_)) => true,
            None => false,
        }
    }
}

enum Msg {
    Scanned {
        path: PathBuf,
        gen: u64,
        quiet: bool,
        status: Box<Result<git::Status, String>>,
        context: Option<String>,
        cached: Option<String>,
    },
    Summarized { path: PathBuf, result: Result<String, String> },
    CommitMessage { path: PathBuf, result: Result<String, String> },
    GitDone { path: PathBuf, result: Result<String, String> },
    Activity { path: PathBuf, result: Result<git::Activity, String> },
}

/// What can be selected and opened on the detail page, in display order.
pub enum Item {
    Commit(String),
    File(git::FileChange),
}

pub fn detail_items(s: &git::Status) -> Vec<Item> {
    s.incoming
        .iter()
        .map(|c| Item::Commit(c.split('\t').next().unwrap_or_default().to_string()))
        .chain(s.unpushed.iter().map(|c| Item::Commit(c.split(' ').next().unwrap_or_default().to_string())))
        .chain(s.files.iter().cloned().map(Item::File))
        .collect()
}

pub enum Modal {
    Commit {
        path: PathBuf,
        message: String,
        push: bool,
        generating: bool,
        /// New secret-looking files the commit will leave out.
        held_back: Vec<String>,
        /// Tracked secret-looking files whose changes the commit will include.
        secret_tracked: Vec<String>,
    },
    Confirm { path: PathBuf, title: &'static str, question: String, action: Action },
    /// Every key, opened with `?`.
    Help { scroll: u16 },
}

#[derive(Clone, Copy)]
pub enum Action {
    Push,
    Rebase,
}

pub enum View {
    List,
    /// `sel` indexes `detail_items`; `None` means nothing is highlighted yet.
    Detail { path: PathBuf, scroll: u16, sel: Option<usize> },
    Diff { title: String, lines: Vec<String>, scroll: u16, back: Box<View> },
}

pub struct App {
    pub root: PathBuf,
    pub root_display: String,
    depth: usize,
    auto: bool,
    pub repos: Vec<Repo>,
    ignore: ignore::Ignore,
    pub show_all: bool,
    pub show_ignored: bool,
    pub filter: String,
    pub filtering: bool,
    sel_path: Option<PathBuf>,
    last_pos: usize,
    pub table_state: TableState,
    pub view: View,
    pub modal: Option<Modal>,
    /// Repos with a commit message being drafted right now.
    drafting: HashSet<PathBuf>,
    pub tick: usize,
    pub notice: Option<String>,
    /// Live mode: re-check every repo every `interval`, and fetch every `fetch_every`.
    pub live: bool,
    interval: Duration,
    pub fetch_every: Option<Duration>,
    last_check: Instant,
    pub last_fetch: Option<Instant>,
    /// A command to hand the terminal to, in a repo (an empty command means `$SHELL`). The run loop does it,
    /// since it owns the terminal.
    handoff: Option<(PathBuf, Vec<String>)>,
    /// Summarize a repo's changes once it has been open on the detail page for `LINGER`.
    auto_summary: bool,
    /// The repo whose detail page was just opened, and when; cleared once the linger check ran.
    opened: Option<(PathBuf, Instant)>,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    ai_tx: mpsc::Sender<(PathBuf, String, bool)>,
}

impl App {
    fn new(
        root: PathBuf,
        depth: usize,
        auto: bool,
        auto_summary: bool,
        interval: Duration,
        fetch_every: Option<Duration>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let (ai_tx, ai_rx) = mpsc::channel::<(PathBuf, String, bool)>();
        let ai_rx = Arc::new(Mutex::new(ai_rx));
        for _ in 0..4 {
            let (rx, tx) = (ai_rx.clone(), tx.clone());
            thread::spawn(move || loop {
                let job = rx.lock().unwrap().recv();
                let Ok((path, context, force)) = job else { break };
                let result = ai::summarize(&context, force);
                if tx.send(Msg::Summarized { path, result }).is_err() {
                    break;
                }
            });
        }
        let ignore_file = ignore::Ignore::load(&root);
        let home = std::env::var("HOME").unwrap_or_default();
        let shown = root.to_string_lossy().into_owned();
        let root_display = match shown.strip_prefix(&home) {
            Some(rest) if !home.is_empty() => format!("~{rest}"),
            _ => shown,
        };
        App {
            root,
            root_display,
            depth,
            auto,
            ignore: ignore_file,
            repos: Vec::new(),
            show_all: false,
            show_ignored: false,
            filter: String::new(),
            filtering: false,
            sel_path: None,
            last_pos: 0,
            table_state: TableState::default(),
            view: View::List,
            modal: None,
            drafting: HashSet::new(),
            tick: 0,
            notice: None,
            live: !interval.is_zero(),
            interval: if interval.is_zero() { Duration::from_secs(5) } else { interval },
            fetch_every,
            last_check: Instant::now(),
            last_fetch: None,
            handoff: None,
            auto_summary,
            opened: None,
            tx,
            rx,
            ai_tx,
        }
    }

    pub fn visible(&self) -> Vec<usize> {
        let f = self.filter.to_lowercase();
        let mut v: Vec<usize> = (0..self.repos.len())
            .filter(|&i| {
                let r = &self.repos[i];
                let shown = if r.ignored { self.show_ignored } else { self.show_all || r.attention() };
                shown && (f.is_empty() || r.name.to_lowercase().contains(&f))
            })
            .collect();
        v.sort_by_key(|&i| (self.repos[i].ignored, !self.repos[i].attention()));
        v
    }

    /// The selection follows the repo, not the row, so it stays put while scan results stream in.
    pub fn select_pos(&mut self) -> (Vec<usize>, usize) {
        let vis = self.visible();
        let pos = self
            .sel_path
            .as_ref()
            .and_then(|p| vis.iter().position(|&i| &self.repos[i].path == p))
            .unwrap_or(self.last_pos.min(vis.len().saturating_sub(1)));
        self.last_pos = pos;
        (vis, pos)
    }

    fn move_sel(&mut self, delta: isize) {
        let (vis, pos) = self.select_pos();
        if vis.is_empty() {
            return;
        }
        let np = (pos as isize + delta).clamp(0, vis.len() as isize - 1) as usize;
        self.sel_path = Some(self.repos[vis[np]].path.clone());
        self.last_pos = np;
    }

    fn selected_idx(&mut self) -> Option<usize> {
        let (vis, pos) = self.select_pos();
        vis.get(pos).copied()
    }

    pub fn repo_idx(&self, path: &Path) -> Option<usize> {
        self.repos.iter().position(|r| r.path == path)
    }

    /// A quiet rescan is the live refresh: no spinners, no notice, and it skips repos that are already being scanned.
    fn rescan(&mut self, fetch: bool, quiet: bool) {
        self.last_check = Instant::now();
        if fetch {
            self.last_fetch = Some(Instant::now());
        }
        let paths = git::discover(&self.root, self.depth);
        let mut old: HashMap<PathBuf, Repo> = self.repos.drain(..).map(|r| (r.path.clone(), r)).collect();
        let root = self.root.clone();
        self.repos = paths.iter().map(|p| old.remove(p).unwrap_or_else(|| Repo::new(&root, p))).collect();
        // Re-read so hand edits to the ignore file count on refresh.
        self.ignore = ignore::Ignore::load(&root);
        for r in &mut self.repos {
            r.ignored = self.ignore.is_ignored(&r.rel);
        }
        let paths: Vec<PathBuf> = self
            .repos
            .iter()
            .filter(|r| !r.ignored && !(quiet && (r.busy || r.checking)))
            .map(|r| r.path.clone())
            .collect();
        self.spawn_scan(paths, fetch, quiet);
    }

    fn spawn_scan(&mut self, paths: Vec<PathBuf>, fetch: bool, quiet: bool) {
        let mut jobs = Vec::new();
        for r in &mut self.repos {
            if paths.contains(&r.path) {
                r.gen += 1;
                if quiet {
                    r.checking = true;
                } else {
                    r.busy = true;
                }
                jobs.push((r.path.clone(), r.gen));
            }
        }
        if fetch && !quiet {
            self.notice = Some(format!("Fetching {} repos…", jobs.len()));
        }
        let workers = if fetch { 8 } else { 16 }.min(jobs.len().max(1));
        let jobs = Arc::new(Mutex::new(jobs.into_iter().rev().collect::<Vec<_>>()));
        for _ in 0..workers {
            let (jobs, tx) = (jobs.clone(), self.tx.clone());
            thread::spawn(move || loop {
                let next = jobs.lock().unwrap().pop();
                let Some((path, gen)) = next else { break };
                if fetch {
                    let _ = git::fetch(&path);
                }
                let status = git::status(&path);
                let (context, cached) = match &status {
                    Ok(s) if s.needs_attention() => {
                        let c = ai::build_context(&path, s);
                        let cached = ai::cached(&c);
                        (Some(c), cached)
                    }
                    _ => (None, None),
                };
                if tx.send(Msg::Scanned { path, gen, quiet, status: Box::new(status), context, cached }).is_err() {
                    break;
                }
            });
        }
    }

    fn toggle_ignore(&mut self, i: usize) {
        let rel = self.repos[i].rel.clone();
        if rel.is_empty() {
            self.notice = Some("The folder being scanned can't be ignored".into());
            return;
        }
        let result = if self.repos[i].ignored {
            self.ignore.remove_matching(&rel).map(|removed| {
                format!("Un-ignored {rel} (removed {} from {})", removed.join(", "), ignore::FILE_NAME)
            })
        } else {
            self.ignore.add(&rel).map(|_| format!("Ignored {rel}  ·  I shows ignored repos, x again to undo"))
        };
        self.finish_ignore(result);
    }

    fn finish_ignore(&mut self, result: std::io::Result<String>) {
        self.notice = Some(match result {
            Ok(msg) => msg,
            Err(e) => format!("Could not write {}: {e}", ignore::FILE_NAME),
        });
        let mut back = Vec::new();
        for r in &mut self.repos {
            let ignored = self.ignore.is_ignored(&r.rel);
            if r.ignored && !ignored {
                back.push(r.path.clone());
            }
            r.ignored = ignored;
        }
        if !back.is_empty() {
            self.spawn_scan(back, false, false);
        }
    }

    fn open_detail(&mut self, i: usize) {
        let path = self.repos[i].path.clone();
        self.view = View::Detail { path: path.clone(), scroll: 0, sel: None };
        self.prefetch_message(i);
        self.load_activity(path.clone());
        self.opened = Some((path, Instant::now()));
    }

    fn load_activity(&self, path: PathBuf) {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = git::activity(&path);
            let _ = tx.send(Msg::Activity { path, result });
        });
    }

    /// Opening a repo with changes asks for a summary, but only once it has stayed open for `LINGER`, so paging
    /// through repos with n/p or opening one by mistake sends nothing.
    fn summarize_if_lingering(&mut self) {
        const LINGER: Duration = Duration::from_millis(500);
        let Some((path, since)) = &self.opened else { return };
        if since.elapsed() < LINGER {
            return;
        }
        let path = path.clone();
        self.opened = None;
        let still_open = matches!(&self.view, View::Detail { path: p, .. } if *p == path);
        let Some(i) = self.repo_idx(&path) else { return };
        let r = &self.repos[i];
        if self.auto_summary && still_open && r.context.is_some() && matches!(r.summary, SummaryState::None) {
            self.summarize(i, false);
            self.prefetch_message(i);
        }
    }

    fn open_commit(&mut self, i: usize) {
        let r = &self.repos[i];
        let Some(Ok(s)) = &r.status else { return };
        if !s.dirty() {
            self.notice = Some(format!("{}: nothing to commit", r.name));
            return;
        }
        let push = s.has_remote && s.branch != "(detached)";
        let held_back = git::secret_new_files(&r.path);
        let secret_tracked =
            s.files.iter().filter(|f| f.code != "??" && git::looks_secret(&f.path)).map(|f| f.path.clone()).collect();
        self.modal = Some(Modal::Commit {
            path: r.path.clone(),
            message: String::new(),
            push,
            generating: false,
            held_back,
            secret_tracked,
        });
        self.generate_message(false);
    }

    fn generate_message(&mut self, force: bool) {
        let Some(Modal::Commit { path, generating, .. }) = &mut self.modal else { return };
        *generating = true;
        let path = path.clone();
        // A draft started in the background is already on its way and will fill the box.
        if !force && self.drafting.contains(&path) {
            return;
        }
        self.drafting.insert(path.clone());
        self.spawn_draft(path, force);
    }

    /// Drafts the commit message ahead of time, so it is usually ready (and cached) when `c` is pressed.
    /// Only for repos whose changes the user already sent to the AI (s, S or --summarize): just opening a repo
    /// must never send its code anywhere.
    fn prefetch_message(&mut self, i: usize) {
        let r = &self.repos[i];
        let asked = !matches!(r.summary, SummaryState::None);
        if asked && matches!(&r.status, Some(Ok(s)) if s.dirty()) && self.drafting.insert(r.path.clone()) {
            self.spawn_draft(r.path.clone(), false);
        }
    }

    fn spawn_draft(&self, path: PathBuf, force: bool) {
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = git::status(&path).and_then(|s| ai::commit_message(&path, &s, force));
            let _ = tx.send(Msg::CommitMessage { path, result });
        });
    }

    fn run_commit(&mut self) {
        let Some(Modal::Commit { path, message, push, .. }) = &self.modal else { return };
        let message = message.trim().to_string();
        if message.is_empty() {
            self.notice = Some("Write a commit message first (ctrl+g asks Claude)".into());
            return;
        }
        let (path, push) = (path.clone(), *push);
        self.modal = None;
        self.start_op(path, move |p| {
            let s = git::status(p)?;
            let (hash, held_back) = git::commit_all(p, &message)?;
            let note = match held_back.len() {
                0 => String::new(),
                n => format!(" (left out {n} secret-looking new file{}: {})", if n == 1 { "" } else { "s" }, held_back.join(", ")),
            };
            if !push {
                return Ok(format!("committed {hash}{note}"));
            }
            match git::push(p, &s) {
                Ok(pushed) => Ok(format!("committed {hash} and {pushed}{note}")),
                Err(e) => Err(format!("committed {hash}{note}, but the push failed: {e}")),
            }
        });
    }

    fn confirm_push(&mut self, i: usize) {
        let r = &self.repos[i];
        let Some(Ok(s)) = &r.status else { return };
        if s.unpushed_count == 0 {
            self.notice = Some(format!("{}: nothing to push", r.name));
            return;
        }
        let target = match (&s.upstream, s.tracking) {
            (Some(u), true) => u.clone(),
            _ => "a new branch on the remote".into(),
        };
        let n = s.unpushed_count;
        let question = format!("Push {n} commit{} on {} to {target}?", if n == 1 { "" } else { "s" }, s.branch);
        self.modal = Some(Modal::Confirm { path: r.path.clone(), title: " Push ", question, action: Action::Push });
    }

    fn update(&mut self, i: usize) {
        let r = &self.repos[i];
        let Some(Ok(s)) = &r.status else { return };
        if !s.tracking {
            self.notice = Some(format!("{}: {} has no upstream to pull from", r.name, s.branch));
            return;
        }
        if s.behind == 0 {
            self.notice = Some(format!("{}: already up to date with {} (f fetches the latest)", r.name, s.upstream.as_deref().unwrap_or("upstream")));
            return;
        }
        let behind = s.behind;
        let plural = |n: u32| if n == 1 { "" } else { "s" };
        if s.ahead == 0 {
            self.start_op(r.path.clone(), move |p| {
                git::pull(p, false).map(|_| format!("pulled {behind} commit{}", plural(behind)))
            });
            return;
        }
        let question = format!(
            "{} has diverged: {} local commit{} and {behind} new commit{} on {}.\n\nRebase your local commit{} on top (git pull --rebase --autostash)? Push afterwards with P.",
            s.branch,
            s.ahead,
            plural(s.ahead),
            plural(behind),
            s.upstream.as_deref().unwrap_or("upstream"),
            plural(s.ahead),
        );
        self.modal = Some(Modal::Confirm { path: r.path.clone(), title: " Update ", question, action: Action::Rebase });
    }

    /// Runs a git operation off the UI thread, then rescans the repo.
    fn start_op(&mut self, path: PathBuf, op: impl FnOnce(&Path) -> Result<String, String> + Send + 'static) {
        if let Some(i) = self.repo_idx(&path) {
            self.repos[i].busy = true;
        }
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = op(&path);
            let _ = tx.send(Msg::GitDone { path, result });
        });
    }

    fn modal_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match self.modal.as_mut() {
            Some(Modal::Commit { message, push, .. }) => match k.code {
                KeyCode::Esc => self.modal = None,
                KeyCode::Enter if k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) => message.push('\n'),
                KeyCode::Enter => self.run_commit(),
                KeyCode::Tab => *push = !*push,
                KeyCode::Char('g') if ctrl => {
                    message.clear();
                    self.generate_message(true);
                }
                KeyCode::Char('u') if ctrl => message.clear(),
                KeyCode::Backspace => {
                    message.pop();
                }
                KeyCode::Char(c) if !ctrl => message.push(c),
                _ => {}
            },
            Some(Modal::Confirm { path, action, .. }) => match k.code {
                KeyCode::Char('y') | KeyCode::Enter => {
                    let (path, action) = (path.clone(), *action);
                    self.modal = None;
                    match action {
                        Action::Push => self.start_op(path, |p| git::status(p).and_then(|s| git::push(p, &s))),
                        Action::Rebase => self.start_op(path, |p| {
                            git::pull(p, true).map(|_| "rebased onto upstream, press P to push".to_string())
                        }),
                    }
                }
                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('q') => self.modal = None,
                _ => {}
            },
            Some(Modal::Help { scroll }) => {
                // The help screen clamps the scroll when it draws, so it never runs past the end.
                let mut by = |d: i32| *scroll = (*scroll as i32 + d).max(0) as u16;
                match k.code {
                    KeyCode::Down | KeyCode::Char('j') => by(1),
                    KeyCode::Up | KeyCode::Char('k') => by(-1),
                    KeyCode::PageDown | KeyCode::Char(' ') => by(10),
                    KeyCode::PageUp => by(-10),
                    KeyCode::Home | KeyCode::Char('g') => *scroll = 0,
                    KeyCode::End | KeyCode::Char('G') => *scroll = u16::MAX,
                    _ => self.modal = None,
                }
            }
            None => {}
        }
    }

    /// Live mode: quietly re-checks every repo, and fetches now and then, so work done in other sessions shows up
    /// without a key press. The first fetch waits for the startup scan, so the list appears right away.
    fn refresh_if_due(&mut self) {
        if !self.live {
            return;
        }
        let fetch = self.fetch_every.is_some_and(|every| match self.last_fetch {
            Some(t) => t.elapsed() >= every,
            None => !self.repos.iter().any(|r| r.busy),
        });
        if fetch || self.last_check.elapsed() >= self.interval {
            self.rescan(fetch, true);
        }
    }

    /// The terminal window title, so pending work shows in a taskbar or tab bar without looking at the window.
    fn window_title(&self) -> String {
        let pending = self.repos.iter().filter(|r| r.attention()).count();
        let behind = self.repos.iter().filter(|r| !r.ignored && matches!(&r.status, Some(Ok(s)) if s.behind > 0)).count();
        let mut t = match pending {
            0 => "✓".to_string(),
            n => format!("{n} pending"),
        };
        if behind > 0 {
            t.push_str(&format!(" · ↓{behind}"));
        }
        format!("{t} · gitglance {}", self.root_display)
    }

    fn summarize(&mut self, i: usize, force: bool) {
        let r = &mut self.repos[i];
        if matches!(r.summary, SummaryState::Pending) || (!force && matches!(r.summary, SummaryState::Done(_))) {
            return;
        }
        let Some(context) = r.context.clone() else {
            self.notice = Some(format!("{}: nothing to summarize", r.name));
            return;
        };
        r.summary = SummaryState::Pending;
        let _ = self.ai_tx.send((r.path.clone(), context, force));
    }

    fn summarize_all(&mut self) {
        for i in self.visible() {
            if self.repos[i].context.is_some() && !self.repos[i].ignored {
                self.summarize(i, false);
            }
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Scanned { path, gen, quiet, status, context, cached } => {
                let Some(i) = self.repo_idx(&path) else { return };
                let r = &mut self.repos[i];
                if gen != r.gen {
                    return;
                }
                let status = *status;
                r.busy = false;
                r.checking = false;
                let new_commits = matches!((&r.status, &status), (Some(Ok(old)), Ok(new)) if old.last_commit_ts != new.last_commit_ts || old.ahead != new.ahead || old.behind != new.behind);
                if matches!((&r.status, &status), (Some(Ok(old)), Ok(new)) if old != new) {
                    r.changed_at = Some(Instant::now());
                }
                if new_commits && matches!(&self.view, View::Detail { path: p, .. } if *p == path) {
                    self.load_activity(path.clone());
                }
                let r = &mut self.repos[i];
                r.status = Some(status);
                // A re-check that finds the same changes keeps the summary as it is (a failure message, too).
                if !quiet || r.context != context {
                    r.context = context;
                    match cached {
                        Some(c) => r.summary = SummaryState::Done(c),
                        None if !matches!(r.summary, SummaryState::Pending) => r.summary = SummaryState::None,
                        None => {}
                    }
                }
                // Background re-checks never call the AI on their own; only a scan you started does.
                if self.auto && !quiet && self.repos[i].context.is_some() {
                    self.summarize(i, false);
                }
            }
            Msg::CommitMessage { path, result } => {
                self.drafting.remove(&path);
                let Some(Modal::Commit { path: open, message, generating, .. }) = &mut self.modal else { return };
                if *open != path {
                    return;
                }
                *generating = false;
                match result {
                    // Never overwrite what the user has started typing.
                    Ok(m) if message.trim().is_empty() => *message = m,
                    Ok(_) => {}
                    Err(e) => self.notice = Some(format!("Claude could not write a message: {e}")),
                }
            }
            Msg::GitDone { path, result } => {
                let name = self.repo_idx(&path).map(|i| self.repos[i].name.clone()).unwrap_or_default();
                self.notice = Some(match result {
                    Ok(m) => format!("{name}: {m} ✓"),
                    Err(e) => format!("{name}: {e}"),
                });
                self.spawn_scan(vec![path], false, false);
            }
            Msg::Activity { path, result } => {
                if let Some(i) = self.repo_idx(&path) {
                    self.repos[i].activity = Some(result);
                }
            }
            Msg::Summarized { path, result } => {
                if let Some(i) = self.repo_idx(&path) {
                    self.repos[i].summary = match result {
                        Ok(t) => SummaryState::Done(t),
                        Err(e) => SummaryState::Failed(e),
                    };
                }
            }
        }
    }

    /// Everything pending in one scrollable view: uncommitted changes, unpushed commits, incoming commits.
    fn open_diff(&mut self, i: usize) {
        let r = &self.repos[i];
        let Some(Ok(s)) = &r.status else { return };
        let mut uncommitted = git::diff(&r.path, s.initial, false);
        let untracked: Vec<&str> = s.files.iter().filter(|f| f.code == "??").map(|f| f.path.as_str()).collect();
        if !untracked.is_empty() {
            uncommitted.push_str("\n# Untracked\n");
            for u in untracked {
                uncommitted.push_str(&format!("?? {u}\n"));
            }
        }
        let mut text = String::new();
        if !uncommitted.trim().is_empty() {
            text.push_str(&format!("### Uncommitted changes ({} files)\n\n{uncommitted}", s.files.len()));
        }
        let unpushed = git::unpushed_log(&r.path, s);
        if !unpushed.is_empty() {
            text.push_str(&format!("\n### Unpushed commits ({})\n\n{unpushed}", s.unpushed_count));
        }
        let incoming = git::incoming_log(&r.path, s);
        if !incoming.is_empty() {
            text.push_str(&format!("\n### Incoming commits you are behind on ({}), u pulls them\n\n{incoming}", s.behind));
        }
        if text.trim().is_empty() {
            self.notice = Some(format!("{}: nothing pending, no changes and nothing to push or pull (f fetches)", r.name));
            return;
        }
        let title = r.name.clone();
        self.show_text(title, text);
    }

    fn open_item(&mut self, i: usize, item: Item) {
        let r = &self.repos[i];
        let Some(Ok(s)) = &r.status else { return };
        let (title, text) = match &item {
            Item::Commit(hash) => (format!("{} · {hash}", r.name), git::show(&r.path, hash)),
            Item::File(f) => (format!("{} · {}", r.name, f.path), git::file_diff(&r.path, s.initial, f)),
        };
        self.show_text(title, text);
    }

    fn show_text(&mut self, title: String, text: String) {
        let lines = text.lines().take(50_000).map(|l| l.replace('\t', "    ")).collect();
        let back = std::mem::replace(&mut self.view, View::List);
        self.view = View::Diff { title, lines, scroll: 0, back: Box::new(back) };
    }

    /// Starts a Claude Code session in the repo (or resumes the last one there) in a new terminal window, so
    /// gitglance stays live beside it. Without a desktop to open a window on, it runs in this terminal instead.
    fn open_agent(&mut self, i: usize, resume: bool) {
        let r = &self.repos[i];
        let mut cmd: Vec<String> = std::env::var("GITGLANCE_AGENT_CMD")
            .ok()
            .filter(|c| !c.trim().is_empty())
            .unwrap_or_else(|| "claude".into())
            .split_whitespace()
            .map(String::from)
            .collect();
        if !on_path(&cmd[0]) {
            self.notice = Some(format!("{} is not on your PATH (GITGLANCE_AGENT_CMD picks another command)", cmd[0]));
            return;
        }
        if resume {
            cmd.push("--continue".into());
        }
        let what = if resume { "Resumed the last agent session" } else { "Started an agent session" };
        self.notice = Some(match launch_in_window(&r.path, &format!("{} · agent", r.name), &cmd) {
            Ok(true) => format!("{what} for {} in a new window", r.name),
            Ok(false) => {
                self.handoff = Some((r.path.clone(), cmd));
                return;
            }
            Err(e) => format!("Could not open a terminal window: {e}"),
        });
    }

    /// Opens the repo folder in the file manager (Finder on macOS, the default one via xdg-open elsewhere).
    fn open_in_file_manager(&mut self, i: usize) {
        use std::process::{Command, Stdio};
        let cmd = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = Command::new(cmd)
            .arg(&self.repos[i].path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
    }

    /// Returns true when the app should quit.
    fn on_key(&mut self, k: KeyEvent) -> bool {
        self.notice = None;
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return true;
        }
        if self.modal.is_some() {
            self.modal_key(k);
            return false;
        }
        if self.filtering {
            match k.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.filtering = false;
                }
                KeyCode::Enter => self.filtering = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Down => self.move_sel(1),
                KeyCode::Up => self.move_sel(-1),
                KeyCode::Char(c) => self.filter.push(c),
                _ => {}
            }
            return false;
        }
        if k.code == KeyCode::Char('?') {
            self.modal = Some(Modal::Help { scroll: 0 });
            return false;
        }
        match self.view {
            View::List => self.list_key(k),
            View::Detail { .. } => self.detail_key(k),
            View::Diff { .. } => self.diff_key(k),
        }
    }

    fn list_key(&mut self, k: KeyEvent) -> bool {
        match k.code {
            KeyCode::Char('q') => return true,
            KeyCode::Esc if self.filter.is_empty() => return true,
            KeyCode::Esc => self.filter.clear(),
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::PageDown => self.move_sel(10),
            KeyCode::PageUp => self.move_sel(-10),
            KeyCode::Home | KeyCode::Char('g') => self.move_sel(-1_000_000),
            KeyCode::End | KeyCode::Char('G') => self.move_sel(1_000_000),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(i) = self.selected_idx() {
                    self.open_detail(i);
                }
            }
            KeyCode::Char('s') => {
                if let Some(i) = self.selected_idx() {
                    self.summarize(i, true);
                    self.prefetch_message(i);
                }
            }
            KeyCode::Char('S') => self.summarize_all(),
            KeyCode::Char('c') => {
                if let Some(i) = self.selected_idx() {
                    self.open_commit(i);
                }
            }
            KeyCode::Char('P') => {
                if let Some(i) = self.selected_idx() {
                    self.confirm_push(i);
                }
            }
            KeyCode::Char('u') => {
                if let Some(i) = self.selected_idx() {
                    self.update(i);
                }
            }
            KeyCode::Char('d') => {
                if let Some(i) = self.selected_idx() {
                    self.open_diff(i);
                }
            }
            KeyCode::Char('o') => {
                if let Some(i) = self.selected_idx() {
                    self.open_in_file_manager(i);
                }
            }
            KeyCode::Char('t') => {
                if let Some(i) = self.selected_idx() {
                    self.handoff = Some((self.repos[i].path.clone(), Vec::new()));
                }
            }
            KeyCode::Char('A') | KeyCode::Char('R') => {
                if let Some(i) = self.selected_idx() {
                    self.open_agent(i, k.code == KeyCode::Char('R'));
                }
            }
            KeyCode::Char('r') => self.rescan(false, false),
            KeyCode::Char('f') => self.rescan(true, false),
            KeyCode::Char('w') => {
                self.live = !self.live;
                if self.live {
                    self.rescan(false, true);
                }
                self.notice = Some(if self.live {
                    format!("Live: re-checking every {}s, w pauses", self.interval.as_secs())
                } else {
                    "Paused: the list only changes when you press r or f, w resumes".into()
                });
            }
            KeyCode::Char('a') => self.show_all = !self.show_all,
            KeyCode::Char('x') => {
                if let Some(i) = self.selected_idx() {
                    self.toggle_ignore(i);
                }
            }
            KeyCode::Char('I') => {
                self.show_ignored = !self.show_ignored;
                let n = self.repos.iter().filter(|r| r.ignored).count();
                self.notice = Some(match (self.show_ignored, n) {
                    (true, 0) => format!("Nothing ignored yet. Press x on a repo, or list repos in {}", ignore::FILE_NAME),
                    (true, _) => format!("Showing {n} ignored repos at the bottom. x un-ignores"),
                    (false, _) => "Hiding ignored repos".into(),
                });
            }
            KeyCode::Char('/') => self.filtering = true,
            _ => {}
        }
        false
    }

    fn detail_key(&mut self, k: KeyEvent) -> bool {
        let View::Detail { path, .. } = &self.view else { return false };
        let path = path.clone();
        let items = match self.repo_idx(&path).map(|i| &self.repos[i].status) {
            Some(Some(Ok(s))) => detail_items(s),
            _ => Vec::new(),
        };
        let n = items.len();
        let View::Detail { scroll, sel, .. } = &mut self.view else { return false };
        // Moves the highlight through commits and files; scrolls instead when there is nothing to select.
        let mut move_by = |d: isize| {
            if n == 0 {
                *scroll = (*scroll as isize + d).max(0) as u16;
                return;
            }
            *sel = match *sel {
                None if d > 0 => Some((d as usize - 1).min(n - 1)),
                None => None,
                Some(x) if (x as isize) + d < 0 => {
                    *scroll = 0;
                    None
                }
                Some(x) => Some(((x as isize + d) as usize).min(n - 1)),
            };
        };
        match k.code {
            KeyCode::Char('q') => return true,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                self.sel_path = Some(path);
                self.view = View::List;
            }
            KeyCode::Down | KeyCode::Char('j') => move_by(1),
            KeyCode::Up | KeyCode::Char('k') => move_by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => move_by(10),
            KeyCode::PageUp => move_by(-10),
            KeyCode::Home | KeyCode::Char('g') => move_by(-1_000_000),
            KeyCode::End | KeyCode::Char('G') => move_by(1_000_000),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                let picked = sel.and_then(|s| items.into_iter().nth(s));
                if let (Some(item), Some(i)) = (picked, self.repo_idx(&path)) {
                    self.open_item(i, item);
                }
            }
            KeyCode::Char('n') | KeyCode::Char('p') => {
                self.sel_path = Some(path);
                self.move_sel(if k.code == KeyCode::Char('n') { 1 } else { -1 });
                if let Some(i) = self.selected_idx() {
                    self.open_detail(i);
                }
            }
            KeyCode::Char('s') => {
                if let Some(i) = self.repo_idx(&path) {
                    self.summarize(i, true);
                }
            }
            KeyCode::Char('d') => {
                if let Some(i) = self.repo_idx(&path) {
                    self.open_diff(i);
                }
            }
            KeyCode::Char('c') => {
                if let Some(i) = self.repo_idx(&path) {
                    self.open_commit(i);
                }
            }
            KeyCode::Char('P') => {
                if let Some(i) = self.repo_idx(&path) {
                    self.confirm_push(i);
                }
            }
            KeyCode::Char('u') => {
                if let Some(i) = self.repo_idx(&path) {
                    self.update(i);
                }
            }
            KeyCode::Char('o') => {
                if let Some(i) = self.repo_idx(&path) {
                    self.open_in_file_manager(i);
                }
            }
            KeyCode::Char('t') => self.handoff = Some((path, Vec::new())),
            KeyCode::Char('A') | KeyCode::Char('R') => {
                if let Some(i) = self.repo_idx(&path) {
                    self.open_agent(i, k.code == KeyCode::Char('R'));
                }
            }
            KeyCode::Char('r') => self.rescan(false, false),
            _ => {}
        }
        false
    }

    fn diff_key(&mut self, k: KeyEvent) -> bool {
        let View::Diff { scroll, lines, .. } = &mut self.view else { return false };
        let max = lines.len().saturating_sub(5) as i32;
        let mut scroll_by = |d: i32| *scroll = (*scroll as i32 + d).clamp(0, max.max(0)) as u16;
        match k.code {
            KeyCode::Char('q') => return true,
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                if let View::Diff { back, .. } = std::mem::replace(&mut self.view, View::List) {
                    self.view = *back;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => scroll_by(1),
            KeyCode::Up | KeyCode::Char('k') => scroll_by(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => scroll_by(30),
            KeyCode::PageUp => scroll_by(-30),
            KeyCode::Home | KeyCode::Char('g') => scroll_by(-1_000_000),
            KeyCode::End | KeyCode::Char('G') => scroll_by(1_000_000),
            _ => {}
        }
        false
    }
}

// xterm's title stack: save the terminal's own title on start and put it back on the way out.
const PUSH_TITLE: &str = "\x1b[22;0t";
const POP_TITLE: &str = "\x1b[23;0t";

fn write_raw(s: &str) {
    use std::io::Write;
    let mut out = io::stdout();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

/// Is `prog` a path to a file, or the name of one in a `$PATH` folder?
fn on_path(prog: &str) -> bool {
    if prog.contains('/') {
        return Path::new(prog).is_file();
    }
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(prog).is_file()))
}

/// Runs `cmd` in a new terminal window in `dir`, detached so it outlives gitglance. `Ok(false)` means there is no
/// desktop or terminal launcher to open a window with.
fn launch_in_window(dir: &Path, title: &str, cmd: &[String]) -> Result<bool, String> {
    let mut c = if cfg!(target_os = "macos") {
        // Terminal.app runs a shell line: single-quote every word for the shell, then escape that for AppleScript.
        let quote = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
        let args: Vec<String> = cmd.iter().map(|a| quote(a)).collect();
        let line = format!("cd {} && {}", quote(&dir.to_string_lossy()), args.join(" "));
        let line = line.replace('\\', "\\\\").replace('"', "\\\"");
        let mut c = Command::new("osascript");
        c.args(["-e", &format!("tell application \"Terminal\" to do script \"{line}\"")]);
        c.args(["-e", "tell application \"Terminal\" to activate"]);
        c
    } else {
        let desktop = ["WAYLAND_DISPLAY", "DISPLAY"].iter().any(|v| std::env::var_os(v).is_some_and(|x| !x.is_empty()));
        if !desktop {
            return Ok(false);
        }
        if on_path("xdg-terminal-exec") {
            let mut c = Command::new("xdg-terminal-exec");
            c.arg(format!("--dir={}", dir.display())).arg(format!("--title={title}")).arg("--").args(cmd);
            c
        } else if let Some(t) = std::env::var("TERMINAL").ok().filter(|t| on_path(t)) {
            let mut c = Command::new(t);
            c.arg("-e").args(cmd);
            c
        } else {
            return Ok(false);
        }
    };
    // Terminals that ignore --dir start where they were launched from.
    c.current_dir(dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    {
        // Its own process group, so closing gitglance's terminal doesn't take the agent's window with it.
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    let mut child = c.spawn().map_err(|e| e.to_string())?;
    thread::spawn(move || child.wait());
    Ok(true)
}

/// Hands the terminal to `cmd` (`$SHELL` when empty) in `dir` until it exits, then takes it back.
fn hand_off(terminal: &mut ratatui::DefaultTerminal, dir: &Path, cmd: &[String]) -> io::Result<()> {
    use ratatui::crossterm::{execute, terminal as term};
    ratatui::restore();
    write_raw(POP_TITLE);
    let shell = [std::env::var("SHELL").unwrap_or_else(|_| "sh".into())];
    let cmd = if cmd.is_empty() { &shell[..] } else { cmd };
    let what = if cmd == shell { "shell" } else { cmd[0].as_str() };
    println!("\n  gitglance: {what} in {}, exit to go back\n", dir.display());
    if let Err(e) = Command::new(&cmd[0]).args(&cmd[1..]).current_dir(dir).status() {
        eprintln!("could not start {}: {e}", cmd[0]);
        thread::sleep(Duration::from_secs(2));
    }
    write_raw(PUSH_TITLE);
    term::enable_raw_mode()?;
    execute!(io::stdout(), term::EnterAlternateScreen)?;
    terminal.clear()
}

fn run(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> io::Result<()> {
    use ratatui::crossterm::{execute, terminal::SetTitle};
    let mut title = String::new();
    loop {
        while let Ok(msg) = app.rx.try_recv() {
            app.handle(msg);
        }
        if let Some((dir, cmd)) = app.handoff.take() {
            hand_off(terminal, &dir, &cmd)?;
            title.clear();
            app.spawn_scan(vec![dir], false, false);
        }
        app.refresh_if_due();
        app.summarize_if_lingering();
        let t = app.window_title();
        if t != title {
            let _ = execute!(io::stdout(), SetTitle(&t));
            title = t;
        }
        terminal.draw(|f| ui::draw(f, app))?;
        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press && app.on_key(k) {
                    return Ok(());
                }
            }
        }
        app.tick = app.tick.wrapping_add(1);
    }
}

fn main() -> io::Result<()> {
    let mut root = None;
    let mut depth = 2;
    let mut auto = false;
    let mut auto_summary = true;
    let mut interval = Duration::from_secs(5);
    let mut fetch_every = Some(Duration::from_secs(5 * 60));
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{HELP}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("gitglance {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "-d" | "--depth" => depth = args.next().and_then(|v| v.parse().ok()).unwrap_or(depth),
            "-s" | "--summarize" => auto = true,
            "--no-auto-summary" => auto_summary = false,
            "-i" | "--interval" => {
                if let Some(secs) = args.next().and_then(|v| v.parse().ok()) {
                    interval = Duration::from_secs(secs);
                }
            }
            "-F" | "--fetch-every" => {
                if let Some(mins) = args.next().and_then(|v| v.parse::<u64>().ok()) {
                    fetch_every = (mins > 0).then(|| Duration::from_secs(mins * 60));
                }
            }
            other => root = Some(PathBuf::from(other)),
        }
    }
    let root = root.map_or_else(std::env::current_dir, Ok)?.canonicalize()?;
    let mut app = App::new(root, depth, auto, auto_summary, interval, fetch_every);
    app.rescan(false, false);

    let mut terminal = ratatui::init();
    write_raw(PUSH_TITLE);
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    write_raw(POP_TITLE);
    result
}
