use crate::git::{self, Activity, Status};
use crate::{ai, App, BoardRow, Group, Modal, Period, Repo, SummaryState, View};
use std::cmp::Reverse;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Cell, Clear, Padding, Paragraph, Row, Table, Wrap};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SPIN: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SELECTED_BG: Color = Color::Indexed(237);

fn spin(tick: usize) -> char {
    SPIN[tick % SPIN.len()]
}

pub fn ago(ts: i64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let d = (now - ts).max(0);
    match d {
        0..=59 => "now".into(),
        60..=3_599 => format!("{}m", d / 60),
        3_600..=86_399 => format!("{}h", d / 3_600),
        86_400..=2_591_999 => format!("{}d", d / 86_400),
        2_592_000..=31_535_999 => format!("{}mo", d / 2_592_000),
        _ => format!("{}y", d / 31_536_000),
    }
}

/// Short age of something that happened in this session: `12s`, `4m`, `2h`.
fn since(t: Instant) -> String {
    let d = t.elapsed().as_secs();
    match d {
        0..=59 => format!("{d}s"),
        60..=3_599 => format!("{}m", d / 60),
        _ => format!("{}h", d / 3_600),
    }
}

/// Marks a repo a background re-check found changed: bright for the first minute, dim for ten.
fn change_mark(r: &Repo) -> Span<'static> {
    fresh_mark(r.changed_at)
}

/// Marks something that changed or arrived at `t`: bright for the first minute, dim for ten.
fn fresh_mark(t: Option<Instant>) -> Span<'static> {
    match t.map(|t| t.elapsed()) {
        Some(d) if d < Duration::from_secs(60) => "●".yellow().bold(),
        Some(d) if d < Duration::from_secs(600) => "•".dark_gray(),
        _ => " ".into(),
    }
}

/// The gitmoji shortcodes (gitmoji.dev), for commit subjects written as `:wrench: Fix…` rather than `🔧 Fix…`.
/// Emoji are written without the variation selector, so the terminal and the layout agree on their width.
const GITMOJI: &[(&str, &str)] = &[
    ("adhesive_bandage", "🩹"), ("airplane", "✈"), ("alembic", "⚗"), ("alien", "👽"), ("ambulance", "🚑"),
    ("arrow_down", "⬇"), ("arrow_up", "⬆"), ("art", "🎨"), ("beers", "🍻"), ("bento", "🍱"), ("bookmark", "🔖"),
    ("boom", "💥"), ("bricks", "🧱"), ("bug", "🐛"), ("building_construction", "🏗"), ("bulb", "💡"),
    ("busts_in_silhouette", "👥"), ("camera_flash", "📸"), ("card_file_box", "🗃"), ("chart_with_upwards_trend", "📈"),
    ("children_crossing", "🚸"), ("closed_lock_with_key", "🔐"), ("clown_face", "🤡"), ("coffin", "⚰"),
    ("construction", "🚧"), ("construction_worker", "👷"), ("dizzy", "💫"), ("egg", "🥚"), ("fire", "🔥"),
    ("globe_with_meridians", "🌐"), ("goal_net", "🥅"), ("green_heart", "💚"), ("hammer", "🔨"),
    ("heavy_minus_sign", "➖"), ("heavy_plus_sign", "➕"), ("iphone", "📱"), ("label", "🏷"), ("lipstick", "💄"),
    ("lock", "🔒"), ("loud_sound", "🔊"), ("mag", "🔍"), ("memo", "📝"), ("money_with_wings", "💸"),
    ("monocle_face", "🧐"), ("mute", "🔇"), ("necktie", "👔"), ("package", "📦"), ("page_facing_up", "📄"),
    ("passport_control", "🛂"), ("pencil2", "✏"), ("poop", "💩"), ("pushpin", "📌"), ("recycle", "♻"),
    ("rewind", "⏪"), ("rocket", "🚀"), ("rotating_light", "🚨"), ("safety_vest", "🦺"), ("see_no_evil", "🙈"),
    ("seedling", "🌱"), ("sparkles", "✨"), ("speech_balloon", "💬"), ("stethoscope", "🩺"), ("tada", "🎉"),
    ("technologist", "🧑‍💻"), ("test_tube", "🧪"), ("thread", "🧵"), ("triangular_flag_on_post", "🚩"),
    ("truck", "🚚"), ("twisted_rightwards_arrows", "🔀"), ("wastebasket", "🗑"), ("wheelchair", "♿"),
    ("white_check_mark", "✅"), ("wrench", "🔧"), ("zap", "⚡"),
];

/// Turns gitmoji shortcodes into their emoji: `:wrench: Release` → `🔧 Release`. Anything else stays as written.
pub fn emojify(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find(':') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let code = after.find(':').map(|end| &after[..end]);
        match code.and_then(|c| GITMOJI.iter().find(|(k, _)| *k == c)) {
            Some((code, emoji)) => {
                out.push_str(emoji);
                rest = &after[code.len() + 1..];
            }
            None => {
                out.push(':');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn trunc(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(w.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let [head, body, foot] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    header(f, head, app);
    match app.view {
        View::List => list(f, body, app),
        View::Detail { .. } => detail(f, body, app),
        View::Diff { ref title, ref lines, scroll, .. } => diff(f, body, title, lines, scroll),
        View::Commits => commits(f, body, app),
    }
    modal(f, body, app);
    footer(f, foot, app);
}

fn modal(f: &mut Frame, area: Rect, app: &mut App) {
    if let Some(Modal::Help { scroll }) = &mut app.modal {
        *scroll = help(f, area, *scroll);
        return;
    }
    let app = &*app;
    let Some(m) = &app.modal else { return };
    let width = area.width.saturating_sub(4).min(96);
    let inner_w = width.saturating_sub(4).max(1) as usize;
    let (title, lines) = match m {
        Modal::Commit { path, message, push, generating, held_back, secret_tracked } => {
            let Some(r) = app.repo_idx(path).map(|i| &app.repos[i]) else { return };
            let mut lines = Vec::new();
            if let Some(Ok(s)) = &r.status {
                lines.push(Line::from(vec![r.name.clone().yellow().bold(), "  on ".dark_gray(), s.branch.clone().bold()]));
                let kept = s.files.iter().filter(|f| !held_back.contains(&f.path)).count();
                let all = if kept == s.files.len() { "all " } else { "" };
                let mut what = vec![format!("Stages and commits {all}{kept} files  ").dark_gray()];
                what.extend(state_spans(s));
                lines.push(Line::from(what));
            }
            if !held_back.is_empty() {
                lines.push(Line::from(
                    format!("Leaving out secret-looking new files (add them to .gitignore): {}", held_back.join(", ")).yellow(),
                ));
            }
            if !secret_tracked.is_empty() {
                lines.push(Line::from(
                    format!("⚠ Includes changes to tracked secret-looking files: {}", secret_tracked.join(", ")).red().bold(),
                ));
            }
            lines.push(Line::raw(""));
            if message.is_empty() && *generating {
                lines.push(Line::from(format!("{} Claude is drafting a message… (or just start typing)", spin(app.tick)).magenta()));
            } else {
                let mut msg: Vec<Line> = message.split('\n').map(|l| Line::from(l.to_string())).collect();
                if let Some(last) = msg.last_mut() {
                    last.push_span("▏".yellow());
                }
                lines.extend(msg);
            }
            lines.push(Line::raw(""));
            let can_push = matches!(&r.status, Some(Ok(s)) if s.has_remote);
            lines.push(match (can_push, *push) {
                (false, _) => Line::from("No remote, so this only commits".dark_gray()),
                (true, true) => Line::from(vec!["[x] ".green().bold(), "push after commit".into()]),
                (true, false) => Line::from(vec!["[ ] ".dark_gray(), "push after commit".dark_gray()]),
            });
            (" Commit ", lines)
        }
        Modal::Help { .. } => return,
        Modal::Confirm { title, question, .. } => {
            let mut lines = vec![Line::raw("")];
            lines.extend(question.split('\n').map(|l| Line::from(l.to_string().bold())));
            lines.push(Line::raw(""));
            (*title, lines)
        }
    };
    let rows: usize = lines.iter().map(|l| l.width().max(1).div_ceil(inner_w)).sum();
    let height = (rows as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 3,
        width,
        height,
    };
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }).block(
            Block::bordered()
                .title(title.bold())
                .border_style(Style::new().cyan())
                .padding(Padding::horizontal(1)),
        ),
        rect,
    );
}

fn header(f: &mut Frame, area: Rect, app: &App) {
    let total = app.repos.len();
    let ignored = app.repos.iter().filter(|r| r.ignored).count();
    let attention = app.repos.iter().filter(|r| r.attention()).count();
    let busy = app.repos.iter().filter(|r| r.busy).count();
    let thinking = app.repos.iter().filter(|r| matches!(r.summary, SummaryState::Pending)).count();
    let mut spans = vec![
        " gitglance ".bold().black().on_cyan(),
        "  ".into(),
        app.root_display.clone().bold(),
        "   ".into(),
        format!("{attention} need attention").yellow(),
        format!(" · {total} repos").dark_gray(),
    ];
    if ignored > 0 {
        spans.push(format!(" · {ignored} ignored").dark_gray());
    }
    if let Some(progress) = &app.syncing {
        spans.push(format!("   {} syncing {}/{}", spin(app.tick), progress.done, progress.total).cyan());
    } else if app.sync_requested {
        spans.push("   sync queued".cyan());
    } else if busy > 0 {
        spans.push(format!("   {} scanning {busy}", spin(app.tick)).cyan());
    }
    if thinking > 0 {
        spans.push(format!("   {} summarizing {thinking}", spin(app.tick)).magenta());
    }
    spans.push(match app.view {
        View::Commits => "   [recent commits]".dark_gray(),
        View::Diff { ref back, .. } if matches!(**back, View::Commits) => "   [recent commits]".dark_gray(),
        _ if app.show_all => "   [all repos]".dark_gray(),
        _ => "   [with changes]".dark_gray(),
    });
    if app.live {
        spans.push("   ● live".green());
        spans.push(match (app.fetch_every, app.last_fetch) {
            (None, _) => " · no auto-fetch".dark_gray(),
            (Some(_), Some(t)) => format!(" · fetched {} ago", since(t)).dark_gray(),
            (Some(_), None) => " · fetching soon".dark_gray(),
        });
    } else {
        spans.push("   ○ paused, w resumes".dark_gray());
    }
    if app.filtering || !app.filter.is_empty() {
        spans.push(format!("   /{}", app.filter).yellow());
        if app.filtering {
            spans.push("▏".yellow());
        }
    }
    f.render_widget(Line::from(spans), area);
}

/// Everything on the help screen, in sections of (keys, what they do). The footer only shows the everyday ones.
const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Repo list",
        &[
            ("↑↓ j k", "move  (g/G top/bottom, PgUp/PgDn)"),
            ("⏎", "open the repo's details"),
            ("s / S", "AI summary for this repo / all listed"),
            ("c", "commit everything and push (AI drafts it)"),
            ("P", "push commits that are already made"),
            ("u", "pull: fast-forward, or rebase if diverged"),
            ("U", "sync every repo: fetch, fast-forward only"),
            ("d", "everything pending as one diff"),
            ("A / R", "new / resume Claude Code session here"),
            ("t", "shell in the repo, exit to come back"),
            ("o", "open the folder in your file manager"),
            ("f / r", "fetch every repo / rescan"),
            ("w", "pause or resume the live refresh"),
            ("a", "repos with changes ↔ all repos"),
            ("tab", "repos with recent commits, and what they are"),
            ("/", "filter by name"),
            ("x / I", "ignore or un-ignore / show ignored"),
            ("q  esc", "quit"),
        ],
    ),
    (
        "Repo details",
        &[
            ("↑↓", "highlight a commit or file"),
            ("⏎", "open it: git show, or the file's diff"),
            ("n / p", "next / previous repo"),
            ("s", "regenerate the AI summary"),
            ("c P u d", "commit, push, pull, diff"),
            ("U", "sync every repo"),
            ("A R t o", "agent, resume, shell, folder"),
            ("esc  ←", "back to the list"),
        ],
    ),
    (
        "Recent commits",
        &[
            ("↑↓ j k", "move  (g/G top/bottom, PgUp/PgDn)"),
            ("⏎", "commit: show it · repo: unfold or fold"),
            ("[ ]  1–4", "period: last hour, today, 24 hours, 7 days"),
            ("n / p", "next / previous commit, on that page"),
            ("/", "filter by repo, author, branch or message"),
            ("t A R o", "shell, agent, resume, folder for its repo"),
            ("tab  esc", "back to the repo list"),
        ],
    ),
    ("Diff", &[("↑↓  space", "scroll, page"), ("g / G", "top / bottom"), ("esc", "back")]),
    (
        "Commit box",
        &[
            ("⏎", "commit (and push)"),
            ("tab", "push on or off"),
            ("ctrl+g", "new AI message"),
            ("ctrl+u", "clear"),
            ("alt+⏎", "new line"),
            ("esc", "cancel"),
        ],
    ),
    (
        "Symbols",
        &[
            ("●  •", "changed or new in the last minute / ten minutes"),
            ("S M ? U", "staged, modified, untracked, conflicts"),
            ("≡", "stashes"),
            ("↑N ↓N", "commits to push / to pull; ↑ marks an unpushed commit"),
            ("✓", "in sync with its upstream"),
            ("!", "sync all couldn't pull it; the reason is in SUMMARY"),
        ],
    ),
];

fn help_section(lines: &mut Vec<Line<'static>>, title: &str, keys: &[(&str, &str)]) {
    if !lines.is_empty() {
        lines.push(Line::raw(""));
    }
    lines.push(Line::from(title.to_string().cyan().bold()));
    for (k, what) in keys {
        lines.push(Line::from(vec![format!("  {k:<11}").yellow().bold(), what.to_string().into()]));
    }
}

/// The help screen; two columns when there is room. Returns the scroll, clamped to the content.
fn help(f: &mut Frame, area: Rect, scroll: u16) -> u16 {
    let two = area.width >= 110;
    let (mut left, mut right) = (Vec::new(), Vec::new());
    // Two columns of about equal height: the repo list and recent commits keys left, the rest right.
    for (title, keys) in HELP {
        let side = if two && !["Repo list", "Recent commits"].contains(title) { &mut right } else { &mut left };
        help_section(side, title, keys);
    }
    let width = if two { area.width.saturating_sub(4).min(124) } else { area.width.saturating_sub(2).min(72) };
    let rows = left.len().max(right.len()) as u16;
    let height = (rows + 3).min(area.height);
    let scroll = scroll.min((rows + 3).saturating_sub(height));
    let rect = Rect { x: area.x + (area.width - width) / 2, y: area.y + (area.height - height) / 2, width, height };
    f.render_widget(Clear, rect);
    let block = Block::bordered()
        .title(" Keys ".bold())
        .title_bottom(Line::from(" any key closes ".dark_gray()).right_aligned())
        .border_style(Style::new().cyan())
        .padding(Padding::new(2, 2, 1, 0));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    let cols = if two {
        Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).spacing(4).split(inner).to_vec()
    } else {
        vec![inner]
    };
    f.render_widget(Paragraph::new(left).scroll((scroll, 0)), cols[0]);
    if two {
        f.render_widget(Paragraph::new(right).scroll((scroll, 0)), cols[1]);
    }
    scroll
}

fn footer(f: &mut Frame, area: Rect, app: &App) {
    if let Some(n) = &app.notice {
        f.render_widget(Line::from(format!(" {n}").yellow()), area);
        return;
    }
    let keys: &[(&str, &str)] = match app.view {
        _ if matches!(app.modal, Some(Modal::Commit { push: true, .. })) => &[
            ("⏎", "commit & push"),
            ("tab", "don't push"),
            ("ctrl+g", "new AI message"),
            ("ctrl+u", "clear"),
            ("alt+⏎", "newline"),
            ("esc", "cancel"),
        ],
        _ if matches!(app.modal, Some(Modal::Commit { .. })) => &[
            ("⏎", "commit"),
            ("tab", "also push"),
            ("ctrl+g", "new AI message"),
            ("ctrl+u", "clear"),
            ("alt+⏎", "newline"),
            ("esc", "cancel"),
        ],
        _ if matches!(app.modal, Some(Modal::Help { .. })) => &[("↑↓", "scroll"), ("any other key", "closes")],
        _ if app.modal.is_some() => &[("y/⏎", "yes"), ("n/esc", "cancel")],
        _ if app.filtering => &[("type", "filter"), ("⏎", "done"), ("esc", "clear")],
        // The everyday keys only; ? lists them all.
        View::List => &[
            ("⏎", "details"),
            ("c", "commit"),
            ("P", "push"),
            ("u", "pull"),
            ("U", "sync all"),
            ("d", "diff"),
            ("A", "agent"),
            ("?", "all keys"),
            ("q", "quit"),
        ],
        View::Detail { .. } => &[
            ("esc", "back"),
            ("⏎", "open"),
            ("n/p", "next/prev"),
            ("c", "commit"),
            ("P", "push"),
            ("u", "pull"),
            ("A", "agent"),
            ("?", "all keys"),
        ],
        View::Commits => &[
            ("⏎", "open commit / unfold repo"),
            ("[ ]", "period"),
            ("tab", "repos"),
            ("/", "filter"),
            ("t", "shell"),
            ("A", "agent"),
            ("?", "all keys"),
            ("q", "quit"),
        ],
        View::Diff { ref back, .. } if matches!(**back, View::Commits) => {
            &[("esc", "back"), ("n/p", "next/prev commit"), ("↑↓", "scroll"), ("space", "page"), ("?", "all keys")]
        }
        View::Diff { .. } => &[("esc", "back"), ("↑↓", "scroll"), ("space", "page"), ("?", "all keys")],
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, label) in keys {
        spans.push(k.to_string().cyan().bold());
        spans.push(format!(" {label}   ").dark_gray());
    }
    f.render_widget(Line::from(spans), area);
}

/// Puts a space between spans, so a cell is no wider than its content.
fn spaced(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut out = Vec::with_capacity(spans.len() * 2);
    for (i, sp) in spans.into_iter().enumerate() {
        if i > 0 {
            out.push(" ".into());
        }
        out.push(sp);
    }
    out
}

fn state_spans(s: &Status) -> Vec<Span<'static>> {
    let mut v = Vec::new();
    if s.conflicts > 0 {
        v.push(format!("U{}", s.conflicts).red().bold());
    }
    if s.staged > 0 {
        v.push(format!("S{}", s.staged).green());
    }
    if s.unstaged > 0 {
        v.push(format!("M{}", s.unstaged).yellow());
    }
    if s.untracked > 0 {
        v.push(format!("?{}", s.untracked).blue());
    }
    if s.stashes > 0 {
        v.push(format!("≡{}", s.stashes).dark_gray());
    }
    spaced(v)
}

fn lines_spans(s: &Status) -> Vec<Span<'static>> {
    let mut v = Vec::new();
    if s.insertions > 0 {
        v.push(format!("+{}", s.insertions).green());
    }
    if s.deletions > 0 {
        v.push(format!("-{}", s.deletions).red());
    }
    spaced(v)
}

fn sync_spans(s: &Status) -> Vec<Span<'static>> {
    if s.initial {
        return vec!["no commits".dark_gray()];
    }
    if s.tracking {
        let mut v = Vec::new();
        if s.ahead > 0 {
            v.push(format!("↑{}", s.ahead).cyan().bold());
        }
        if s.behind > 0 {
            v.push(format!("↓{}", s.behind).magenta().bold());
        }
        if v.is_empty() {
            v.push("✓".green());
        }
        return spaced(v);
    }
    if !s.has_remote {
        return vec!["local".dark_gray()];
    }
    if s.unpushed_count > 0 {
        spaced(vec![format!("↑{}", s.unpushed_count).cyan().bold(), "unpushed".cyan()])
    } else if s.upstream.is_some() {
        vec!["upstream gone".dark_gray()]
    } else {
        vec!["no upstream".dark_gray()]
    }
}

fn summary_cell(r: &Repo, tick: usize) -> Line<'static> {
    if let Some(issue) = &r.sync_issue {
        return Line::from(format!("sync: {issue}").red());
    }
    match &r.summary {
        SummaryState::Done(t) => Line::from(ai::headline(t, &r.name).to_string()),
        SummaryState::Pending => Line::from(format!("{} summarizing…", spin(tick)).magenta()),
        SummaryState::Failed(e) => Line::from(format!("AI failed: {e}").red()),
        SummaryState::None if r.settling.is_some() => Line::from("summarizing once the changes settle…".dark_gray()),
        SummaryState::None if r.attention() => Line::from("press s to summarize".dark_gray()),
        SummaryState::None => Line::from("clean".dark_gray()),
    }
}

const COLUMNS: [&str; 7] = ["  REPO", "BRANCH", "STATE", "+/-", "SYNC", "AGE", "SUMMARY"];

/// One list row as plain lines (one per column in `COLUMNS`), so the list can size each column to its content.
fn row(r: &Repo, tick: usize, name_w: usize) -> Vec<Line<'static>> {
    let blank = || Line::raw("");
    let name_style = match &r.status {
        Some(Ok(s)) if s.conflicts > 0 => Style::new().red().bold(),
        Some(Ok(s)) if s.dirty() => Style::new().yellow().bold(),
        Some(Ok(s)) if s.needs_attention() => Style::new().cyan().bold(),
        Some(Err(_)) => Style::new().red(),
        _ => Style::new().dark_gray(),
    };
    if r.ignored {
        let name = Line::from(vec!["  ".into(), Span::styled(trunc(&r.name, name_w), Style::new().dark_gray().crossed_out())]);
        let mut cells = vec![name];
        cells.extend((0..5).map(|_| blank()));
        cells.push(Line::from("ignored  (x to un-ignore)".dark_gray()));
        return cells;
    }
    let name = Line::from(vec![change_mark(r), " ".into(), Span::styled(trunc(&r.name, name_w), name_style)]);
    let s = match &r.status {
        None => return vec![name, Line::from(spin(tick).to_string().cyan())],
        Some(Err(e)) => {
            let mut cells = vec![name];
            cells.extend((0..5).map(|_| blank()));
            cells.push(Line::from(format!("git error: {e}").red()));
            return cells;
        }
        Some(Ok(s)) => s,
    };
    let branch_style = match s.branch.as_str() {
        "main" | "master" => Style::new(),
        _ => Style::new().magenta(),
    };
    let age = if r.busy {
        spin(tick).to_string().cyan()
    } else {
        s.last_commit_ts.map(ago).unwrap_or_default().dark_gray()
    };
    let mut sync = sync_spans(s);
    if r.sync_issue.is_some() {
        sync.splice(0..0, ["!".red().bold(), " ".into()]);
    }
    vec![
        name,
        Line::from(Span::styled(trunc(&s.branch, 18), branch_style)),
        Line::from(state_spans(s)),
        Line::from(lines_spans(s)),
        Line::from(sync),
        Line::from(age),
        summary_cell(r, tick),
    ]
}

fn list(f: &mut Frame, area: Rect, app: &mut App) {
    let (vis, pos) = app.select_pos();
    if vis.is_empty() {
        let scanning = app.repos.iter().any(|r| r.busy);
        let msg = if app.repos.is_empty() && !scanning {
            "No git repositories found here.".to_string()
        } else if scanning {
            format!("{} Scanning {} repos…", spin(app.tick), app.repos.len())
        } else if !app.filter.is_empty() {
            format!("Nothing matches /{}", app.filter)
        } else {
            "Everything is committed and pushed ✓   (press a to list all repos)".to_string()
        };
        let [_, mid, _] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1), Constraint::Fill(1)]).areas(area);
        f.render_widget(Line::from(msg).centered(), mid);
        return;
    }
    let name_w = vis.iter().map(|&i| app.repos[i].name.chars().count()).max().unwrap_or(4).clamp(4, 30);
    let cells: Vec<Vec<Line>> = vis.iter().map(|&i| row(&app.repos[i], app.tick, name_w)).collect();
    // Every column but the summary is as wide as its widest cell (or header); the summary gets the rest.
    let mut widths: Vec<Constraint> = (0..COLUMNS.len() - 1)
        .map(|c| {
            let content = cells.iter().filter_map(|r| r.get(c)).map(Line::width).max().unwrap_or(0);
            Constraint::Length(content.max(COLUMNS[c].len()) as u16)
        })
        .collect();
    widths.push(Constraint::Fill(1));
    let header = Row::new(COLUMNS).style(Style::new().dark_gray().bold()).bottom_margin(0);
    let rows: Vec<Row> = cells.into_iter().map(|r| Row::new(r.into_iter().map(Cell::from))).collect();
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(2)
        .row_highlight_style(Style::new().bg(SELECTED_BG))
        .highlight_symbol("▌ ");
    app.table_state.select(Some(pos));
    f.render_stateful_widget(table, area.inner(Margin::new(0, 1)), &mut app.table_state);
}

/// `s` cut to `w` terminal columns, with `…` when it doesn't fit. Counts wide characters like emoji as two.
fn fit(s: &str, w: usize) -> String {
    if Span::raw(s).width() <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = Span::raw(c.to_string()).width();
        if used + cw + 1 > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    out
}

fn width(spans: &[Span]) -> usize {
    spans.iter().map(Span::width).sum()
}

fn first_name(author: &str) -> &str {
    author.split_whitespace().next().unwrap_or(author)
}

/// `5 commits · 10m–8h · Peter, Stefan`: how many, over how long, and who, most commits first.
fn group_meta(app: &App, g: &Group) -> String {
    let log: Vec<&git::LogCommit> = g.commits.iter().map(|&j| app.commit(g.repo, j)).collect();
    let n = log.len();
    let (newest, oldest) = (ago(log[0].ts), ago(log[n - 1].ts));
    let span = if newest == oldest { newest } else { format!("{newest}–{oldest}") };
    let mut who: Vec<(&str, usize)> = Vec::new();
    for c in &log {
        match who.iter_mut().find(|(a, _)| *a == c.author) {
            Some((_, k)) => *k += 1,
            None => who.push((&c.author, 1)),
        }
    }
    who.sort_by_key(|&(_, k)| Reverse(k));
    let mut names: Vec<&str> = who.iter().take(3).map(|&(a, _)| first_name(a)).collect();
    let extra = who.len().saturating_sub(3).to_string();
    if who.len() > 3 {
        names.push(&extra);
    }
    format!("{n} commit{} · {span} · {}", if n == 1 { "" } else { "s" }, names.join(", "))
}

/// Below these widths the board leaves out the files count, then the author, so the message keeps room.
const FILES_MIN_WIDTH: usize = 100;
const AUTHOR_MIN_WIDTH: usize = 80;

/// The recent-commits board: every repo with commits in the period, the one with the newest commit first, each
/// heading its newest commits. Sizes line up on the right; authors show only for commits that aren't yours.
fn commits(f: &mut Frame, area: Rect, app: &mut App) {
    let (groups, rows, pos) = app.board_pos();
    let [bar, _, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(1)]).areas(area.inner(Margin::new(1, 0)));
    let total: usize = groups.iter().map(|g| g.commits.len()).sum();
    let mut top = vec![format!("{}   ", app.period.title()).bold()];
    for (k, p) in Period::ALL.iter().enumerate() {
        if k > 0 {
            top.push(" · ".dark_gray());
        }
        top.push(if *p == app.period { format!("[{}]", p.label()).cyan().bold() } else { p.label().dark_gray() });
    }
    let s = |n: usize| if n == 1 { "" } else { "s" };
    top.push(format!("      {} repo{} · {total} commit{}", groups.len(), s(groups.len()), s(total)).dark_gray());
    let hint = "      [ ] or 1–4 change the period".dark_gray();
    if width(&top) + hint.width() <= bar.width as usize {
        top.push(hint);
    }
    f.render_widget(Line::from(top), bar);

    if rows.is_empty() {
        let msg = if app.repos.iter().any(|r| r.busy) {
            format!("{} Reading commits in {} repos…", spin(app.tick), app.repos.len())
        } else if !app.filter.is_empty() {
            format!("No commit matches /{} in this period", app.filter)
        } else if app.period != Period::Week {
            format!("No commits: {}. ] looks further back.", app.period.title().to_lowercase())
        } else {
            format!("No commits in the last {} days.", git::RECENT_DAYS)
        };
        let [_, mid, _] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1), Constraint::Fill(1)]).areas(body);
        f.render_widget(Line::from(msg).centered(), mid);
        return;
    }

    // Two columns go to the selection bar.
    let w = (body.width as usize).saturating_sub(2);
    let (show_files, show_author) = (w >= FILES_MIN_WIDTH, w >= AUTHOR_MIN_WIDTH);
    let me = app.me.clone();
    let others = |c: &git::LogCommit| show_author && me.as_deref() != Some(c.author.as_str());
    // Column widths for the right-hand side, so sizes and authors line up across every repo.
    let totals: Vec<git::Size> = groups
        .iter()
        .map(|g| {
            g.commits.iter().map(|&j| app.commit(g.repo, j).size).fold(git::Size::default(), |t, s| git::Size {
                files: 0,
                added: t.added + s.added,
                removed: t.removed + s.removed,
            })
        })
        .collect();
    let shown: Vec<&git::LogCommit> =
        rows.iter().filter_map(|r| if let BoardRow::Commit(i, j) = *r { Some(app.commit(i, j)) } else { None }).collect();
    let files = |s: git::Size| format!("{} file{}", s.files, if s.files == 1 { "" } else { "s" });
    let aw = shown.iter().map(|c| c.size.added).chain(totals.iter().map(|t| t.added)).max().unwrap_or(0).to_string().len() + 1;
    let rw = shown.iter().map(|c| c.size.removed).chain(totals.iter().map(|t| t.removed)).max().unwrap_or(0).to_string().len() + 1;
    let fw = if show_files { shown.iter().map(|c| files(c.size).len()).max().unwrap_or(0) } else { 0 };
    // The unpushed marker gets a column only when there is one to show.
    let any_unpushed = shown.iter().any(|c| c.unpushed);
    let who_w = shown.iter().filter(|c| others(c)).map(|c| Span::raw(fit(first_name(&c.author), 12)).width()).max().unwrap_or(0);
    let lines_cell = |s: git::Size, bold: bool| -> Vec<Span<'static>> {
        let (a, r) = (format!("{:>aw$}", format!("+{}", s.added)), format!("{:>rw$}", format!("−{}", s.removed)));
        if bold {
            vec![a.green().bold(), " ".into(), r.red().bold()]
        } else {
            vec![a.green(), " ".into(), r.red()]
        }
    };
    // A row's left side, cut to leave room for its right side, then padded so the right side ends at the edge.
    let join = |mut left: Vec<Span<'static>>, right: Vec<Span<'static>>, cut: usize| -> Line<'static> {
        let room = w.saturating_sub(width(&right) + 2);
        if width(&left) > room {
            // Only the last span (the message or the summary) gets shortened.
            if let Some(last) = left.pop() {
                let keep = room.saturating_sub(width(&left)).max(cut.min(room));
                left.push(Span::styled(fit(&last.content, keep), last.style));
            }
        }
        let gap = w.saturating_sub(width(&left) + width(&right));
        left.push(" ".repeat(gap).into());
        left.extend(right);
        Line::from(left)
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut sel_line = 0;
    for (k, &row) in rows.iter().enumerate() {
        let line = match row {
            BoardRow::Repo(i) => {
                if k > 0 {
                    lines.push(Line::raw(""));
                }
                let g = groups.iter().position(|g| g.repo == i).expect("every repo row has a group");
                let r = &app.repos[i];
                let arrived = groups[g].commits.iter().filter_map(|&j| app.commit(i, j).arrived).max();
                let (dir, leaf) = r.name.rsplit_once('/').map_or(("", r.name.as_str()), |(d, l)| (d, l));
                let mut left = vec![fresh_mark(arrived), " ".into()];
                if !dir.is_empty() {
                    left.push(format!("{dir}/").dark_gray());
                }
                left.push(leaf.to_string().bold());
                let unpushed = groups[g].commits.iter().filter(|&&j| app.commit(i, j).unpushed).count();
                if unpushed > 0 {
                    left.push(format!("  ↑{unpushed} unpushed").cyan().bold());
                }
                left.push("   ".into());
                left.push(group_meta(app, &groups[g]).dark_gray());
                join(left, lines_cell(totals[g], true), 10)
            }
            BoardRow::Commit(i, j) => {
                let c = app.commit(i, j);
                let mut left = vec!["   ".into(), fresh_mark(c.arrived), format!("{:>4}  ", ago(c.ts)).dark_gray()];
                if any_unpushed {
                    left.push(if c.unpushed { "↑ ".cyan().bold() } else { "  ".into() });
                }
                if !matches!(&app.repos[i].status, Some(Ok(s)) if s.branch == c.branch) {
                    left.push(fit(&c.branch, 28).magenta());
                    left.push("  ".into());
                }
                left.push(emojify(&c.subject).into());
                let mut right = Vec::new();
                if who_w > 0 {
                    let who = if others(c) { fit(first_name(&c.author), 12) } else { String::new() };
                    right.push(format!("{who:<who_w$}   "));
                }
                let mut right: Vec<Span<'static>> = right.into_iter().map(Span::raw).collect();
                if show_files {
                    right.push(format!("{:>fw$}  ", files(c.size)).dark_gray());
                }
                right.extend(lines_cell(c.size, false));
                join(left, right, 20)
            }
            BoardRow::More(_, n) => Line::from(format!("          … {n} more  (⏎ shows them)").dark_gray()),
        };
        let selected = k == pos;
        if selected {
            sel_line = lines.len();
        }
        let mut spans = vec![if selected { "▌ ".cyan().bold() } else { "  ".into() }];
        spans.extend(line.spans);
        let line = Line::from(spans);
        lines.push(if selected { line.style(Style::new().bg(SELECTED_BG)) } else { line });
    }

    // Keep the selection on screen, and its repo's header too when the selection is that repo's first commit.
    let height = body.height as usize;
    let mut scroll = app.board_scroll.min(lines.len().saturating_sub(1));
    let want_top = if pos > 0 && matches!(rows[pos - 1], BoardRow::Repo(_)) { sel_line - 1 } else { sel_line };
    if want_top < scroll {
        scroll = want_top.saturating_sub(1);
    } else if sel_line >= scroll + height {
        scroll = sel_line + 1 - height;
    }
    if pos == 0 {
        scroll = 0;
    }
    app.board_scroll = scroll;
    f.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), body);
}

fn section(lines: &mut Vec<Line<'static>>, title: String, hint: &str) {
    lines.push(Line::raw(""));
    let hint = if hint.is_empty() { String::new() } else { format!("  ({hint})") };
    lines.push(Line::from(vec![format!("── {title}").cyan().bold(), hint.dark_gray()]));
}

/// The activity panel sits to the right of the detail page when the terminal is at least this wide.
const SIDE_BY_SIDE: u16 = 120;
const PANEL_W: u16 = 52;

fn detail(f: &mut Frame, area: Rect, app: &mut App) {
    let View::Detail { ref path, scroll, sel } = app.view else { return };
    let Some(i) = app.repo_idx(&path.clone()) else { return };
    let (mut lines, items) = detail_lines(&app.repos[i], app.tick, sel);
    let area = if area.width >= SIDE_BY_SIDE {
        let [main, side] = Layout::horizontal([Constraint::Min(40), Constraint::Length(PANEL_W)]).areas(area);
        let mut panel = vec![Line::from("Activity".cyan().bold())];
        panel.extend(activity_lines(&app.repos[i], app.tick));
        f.render_widget(
            Paragraph::new(panel).block(
                Block::new()
                    .borders(ratatui::widgets::Borders::LEFT)
                    .border_style(Style::new().dark_gray())
                    .padding(Padding::new(2, 1, 1, 0)),
            ),
            side,
        );
        main
    } else {
        section(&mut lines, "Activity".into(), "");
        lines.extend(activity_lines(&app.repos[i], app.tick));
        area
    };

    // Keep the highlighted item on screen, estimating wrapped rows like the Paragraph will.
    let width = area.width.saturating_sub(4).max(1) as usize;
    let height = area.height.saturating_sub(1).max(1) as usize;
    let mut scroll = scroll as usize;
    if let Some(&line) = sel.and_then(|s| items.get(s)) {
        let row: usize = lines[..line].iter().map(|l| l.width().max(1).div_ceil(width)).sum();
        if row < scroll {
            scroll = row.saturating_sub(1);
        } else if row + 2 > scroll + height {
            scroll = (row + 2).saturating_sub(height);
        }
    }
    if let View::Detail { scroll: s, .. } = &mut app.view {
        *s = scroll as u16;
    }
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll as u16, 0))
        .block(Block::new().padding(Padding::new(2, 2, 1, 0)));
    f.render_widget(p, area);
}

/// A selectable row on the detail page: a bar marks and a background highlights the selection.
fn item_line(spans: Vec<Span<'static>>, selected: bool) -> Line<'static> {
    let mut all = vec![if selected { "▌ ".cyan().bold() } else { "  ".into() }];
    all.extend(spans);
    let line = Line::from(all);
    if selected {
        line.style(Style::new().bg(SELECTED_BG))
    } else {
        line
    }
}

/// The page's lines plus the line index of each selectable item (see `detail_items`).
fn detail_lines(r: &Repo, tick: usize, sel: Option<usize>) -> (Vec<Line<'static>>, Vec<usize>) {
    let mut items = Vec::new();
    let mut title = vec![r.name.clone().yellow().bold(), "   ".into(), r.path.display().to_string().dark_gray()];
    if let Some(t) = r.changed_at {
        title.extend(["   ".into(), change_mark(r), format!(" changed {} ago", since(t)).dark_gray()]);
    }
    let mut lines: Vec<Line<'static>> = vec![Line::from(title)];
    let s = match &r.status {
        None => {
            lines.push(Line::from(format!("{} scanning…", spin(tick)).cyan()));
            return (lines, items);
        }
        Some(Err(e)) => {
            lines.push(Line::from(format!("git error: {e}").red()));
            return (lines, items);
        }
        Some(Ok(s)) => s,
    };

    let mut branch = vec!["branch ".dark_gray(), s.branch.clone().bold()];
    if let Some(u) = &s.upstream {
        branch.push(" → ".dark_gray());
        branch.push(u.clone().into());
    }
    branch.push("   ".into());
    branch.extend(sync_spans(s));
    if s.stashes > 0 {
        branch.push(format!("   {} stashed", s.stashes).dark_gray());
    }
    lines.push(Line::from(branch));
    if let Some(issue) = &r.sync_issue {
        lines.push(Line::from(vec!["! ".red().bold(), format!("sync all couldn't pull: {issue}").red()]));
    }
    if let Some(ts) = s.last_commit_ts {
        lines.push(Line::from(vec![
            "last commit ".dark_gray(),
            match ago(ts).as_str() {
                "now" => "just now  ".to_string(),
                a => format!("{a} ago  "),
            }
            .into(),
            emojify(&s.last_commit_subject).italic(),
        ]));
    }
    let mut totals = vec![format!("{} files changed   ", s.files.len()).into()];
    totals.extend(lines_spans(s));
    totals.push("   ".into());
    totals.extend(state_spans(s));
    lines.push(Line::from(totals));

    section(&mut lines, "AI summary".into(), if r.context.is_some() { "s to regenerate" } else { "" });
    match &r.summary {
        SummaryState::Done(t) => {
            let head = ai::headline(t, &r.name).to_string();
            let mut first = true;
            for l in t.lines() {
                if first && !l.trim().is_empty() {
                    lines.push(Line::from(head.clone().bold()));
                    first = false;
                } else if l.trim_start().starts_with("Heads-up") {
                    lines.push(Line::from(l.to_string().yellow()));
                } else if ai::SECTION_LABELS.contains(&l.trim()) {
                    lines.push(Line::from(l.trim().to_string().dark_gray().bold()));
                } else {
                    lines.push(Line::from(l.to_string()));
                }
            }
        }
        SummaryState::Pending => lines.push(Line::from(format!("{} Asking Claude…", spin(tick)).magenta())),
        SummaryState::Failed(e) => lines.push(Line::from(format!("AI failed: {e}").red())),
        SummaryState::None if r.context.is_some() => {
            lines.push(Line::from("Press s to summarize these changes with AI.".dark_gray()))
        }
        SummaryState::None => lines.push(Line::from("Nothing pending: everything is committed and pushed.".dark_gray())),
    }

    if !s.incoming.is_empty() {
        let upstream = s.upstream.as_deref().unwrap_or("upstream");
        section(&mut lines, format!("Incoming from {upstream} ({})", s.behind), "⏎ shows a commit, u pulls them");
        for c in &s.incoming {
            let mut parts = c.splitn(3, '\t');
            let (hash, ts, msg) = (parts.next().unwrap_or(""), parts.next(), parts.next().unwrap_or(""));
            let age = ts.and_then(|t| t.parse().ok()).map(ago).unwrap_or_default();
            let selected = sel == Some(items.len());
            items.push(lines.len());
            lines.push(item_line(
                vec![hash.to_string().magenta(), " ".into(), format!("{age:>3} ").dark_gray(), emojify(msg).into()],
                selected,
            ));
        }
        if s.behind as usize > s.incoming.len() {
            lines.push(Line::from(format!("  … and {} more (d shows them all)", s.behind as usize - s.incoming.len()).dark_gray()));
        }
    }

    if s.unpushed_count > 0 {
        section(&mut lines, format!("Unpushed commits ({})", s.unpushed_count), "⏎ shows a commit");
        for c in &s.unpushed {
            let (hash, msg) = c.split_once(' ').unwrap_or((c, ""));
            let selected = sel == Some(items.len());
            items.push(lines.len());
            lines.push(item_line(vec![hash.to_string().yellow(), " ".into(), emojify(msg).into()], selected));
        }
        if s.unpushed_count as usize > s.unpushed.len() {
            lines.push(Line::from(format!("… and {} more", s.unpushed_count as usize - s.unpushed.len()).dark_gray()));
        }
    }

    if !s.files.is_empty() {
        section(&mut lines, format!("Changes ({})", s.files.len()), "⏎ shows a file, d everything");
        for fc in &s.files {
            let code_style = match fc.code.as_str() {
                "??" => Style::new().blue(),
                "UU" => Style::new().red().bold(),
                c if c.starts_with(' ') => Style::new().yellow(),
                _ => Style::new().green(),
            };
            let mut l = vec![Span::styled(format!("{:<3}", fc.code), code_style), fc.path.clone().into()];
            if let (Some(a), Some(d)) = (fc.added, fc.removed) {
                l.push("  ".into());
                if a > 0 {
                    l.push(format!("+{a} ").green());
                }
                if d > 0 {
                    l.push(format!("-{d}").red());
                }
            }
            let selected = sel == Some(items.len());
            items.push(lines.len());
            lines.push(item_line(l, selected));
        }
    }
    (lines, items)
}

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn day_label(day: i64) -> String {
    let (_, m, d) = git::civil_from_days(day);
    format!("{} {d}", MONTHS[m as usize - 1])
}

/// `1234` → `1,234`.
fn thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `1234` → `1.2k`, for line counts.
fn short(n: u32) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1_000.0),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// A bar graph `height` rows tall, one column per value, in eighth-block steps.
fn bars(values: &[u32], height: usize) -> Vec<String> {
    const EIGHTHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = values.iter().copied().max().unwrap_or(0).max(1) as usize;
    let levels: Vec<usize> = values
        .iter()
        .map(|&v| if v == 0 { 0 } else { (v as usize * height * 8).div_ceil(max).max(1) })
        .collect();
    (0..height)
        .map(|row| {
            let floor = (height - 1 - row) * 8;
            levels.iter().map(|&l| EIGHTHS[l.saturating_sub(floor).min(8)]).collect()
        })
        .collect()
}

/// A graph with a baseline: `·` marks empty columns, so a quiet period still shows. Each value is `col` wide:
/// a bar of `col - 1` columns and a gap, or a single column when `col` is 1.
fn graph(lines: &mut Vec<Line<'static>>, values: &[u32], height: usize, col: usize, color: Color) {
    let widen = |c: char| match col {
        1 => c.to_string(),
        _ => format!("{}{}", c.to_string().repeat(col - 1), ' '),
    };
    for row in bars(values, height) {
        lines.push(Line::from(row.chars().map(widen).collect::<String>().fg(color)));
    }
    let base: String = values.iter().map(|&v| widen(if v == 0 { '·' } else { '▔' })).collect();
    lines.push(Line::from(base.dark_gray()));
}

/// A label at each end of a `width`-wide axis.
fn axis(left: String, right: &str, width: usize) -> Line<'static> {
    let gap = width.saturating_sub(left.chars().count() + right.chars().count());
    Line::from(format!("{left}{}{right}", " ".repeat(gap)).dark_gray())
}

/// The activity panel: totals, commits per day and per week, and who made them.
fn activity_lines(r: &Repo, tick: usize) -> Vec<Line<'static>> {
    let a: &Activity = match &r.activity {
        None => return vec![Line::from(format!("{} counting commits…", spin(tick)).cyan())],
        Some(Err(e)) => return vec![Line::from(format!("No history: {e}").dark_gray())],
        Some(Ok(a)) => a,
    };
    if a.total_commits == 0 {
        return vec![Line::from("No commits yet.".dark_gray())];
    }
    let mut lines = Vec::new();
    let mut total = vec![thousands(a.total_commits).bold(), " commits".into()];
    if let Some(ts) = a.first_commit_ts {
        total.push(match ago(ts).as_str() {
            "now" => " · started just now".dark_gray(),
            a => format!(" · started {a} ago").dark_gray(),
        });
    }
    lines.push(Line::from(total));
    let (today, week, last_week) =
        (a.days[git::DAYS - 1], a.weeks[git::WEEKS - 1], a.weeks.get(git::WEEKS - 2).copied().unwrap_or(0));
    lines.push(Line::from(vec![
        "today ".dark_gray(),
        today.to_string().bold(),
        "   this week ".dark_gray(),
        week.to_string().bold(),
        "   last week ".dark_gray(),
        last_week.to_string().bold(),
    ]));
    lines.push(Line::from("all branches, merges left out".dark_gray().italic()));

    let max_day = a.days.iter().max().copied().unwrap_or(0);
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        "Per day".bold(),
        format!(", last {} days  ", git::DAYS).dark_gray(),
        format!("max {max_day}").dark_gray(),
    ]));
    graph(&mut lines, &a.days, 3, 1, Color::Cyan);
    lines.push(axis(day_label(a.today - git::DAYS as i64 + 1), "today", git::DAYS));

    let max_week = a.weeks.iter().max().copied().unwrap_or(0);
    let in_window: u32 = a.weeks.iter().sum();
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        "Per week".bold(),
        format!(", {in_window} in {} weeks  ", git::WEEKS).dark_gray(),
        format!("max {max_week}").dark_gray(),
    ]));
    graph(&mut lines, &a.weeks, 3, 2, Color::Magenta);
    let first_monday = a.today - git::weekday(a.today) - 7 * (git::WEEKS as i64 - 1);
    lines.push(axis(day_label(first_monday), "this week", git::WEEKS * 2 - 1));

    lines.push(Line::raw(""));
    lines.push(Line::from(vec!["Who".bold(), format!(", last {} weeks", git::WEEKS).dark_gray()]));
    if a.authors.is_empty() {
        lines.push(Line::from("nobody committed in this period".dark_gray()));
    }
    let top = a.authors.first().map_or(1, |x| x.commits.max(1));
    const BAR: usize = 10;
    for au in a.authors.iter().take(6) {
        let eighths = (au.commits as usize * BAR * 8).div_ceil(top as usize);
        let mut bar = "█".repeat(eighths / 8);
        if !eighths.is_multiple_of(8) {
            bar.push(['▏', '▎', '▍', '▌', '▋', '▊', '▉'][eighths % 8 - 1]);
        }
        lines.push(Line::from(vec![
            format!("{bar:<BAR$} ").cyan(),
            format!("{:>4} ", au.commits).bold(),
            format!("{:<16} ", trunc(&au.name, 16)).into(),
            format!("+{}", short(au.added)).green(),
            " ".into(),
            format!("-{}", short(au.removed)).red(),
        ]));
    }
    if a.authors.len() > 6 {
        let rest: u32 = a.authors[6..].iter().map(|x| x.commits).sum();
        lines.push(Line::from(format!("… {} more, {rest} commits", a.authors.len() - 6).dark_gray()));
    }
    lines
}

fn diff(f: &mut Frame, area: Rect, title: &str, lines: &[String], scroll: u16) {
    let height = area.height.saturating_sub(2) as usize;
    let start = (scroll as usize).min(lines.len());
    let shown: Vec<Line> = lines[start..(start + height).min(lines.len())]
        .iter()
        .map(|l| {
            let style = if l.starts_with("### ") {
                Style::new().magenta().bold()
            } else if l.starts_with("commit ") {
                Style::new().yellow().bold()
            } else if l.starts_with("+++") || l.starts_with("---") || l.starts_with("diff ") {
                Style::new().bold()
            } else if l.starts_with('+') {
                Style::new().green()
            } else if l.starts_with('-') {
                Style::new().red()
            } else if l.starts_with("@@") {
                Style::new().cyan()
            } else if l.starts_with("??") || l.starts_with("# ") {
                Style::new().blue()
            } else {
                Style::new()
            };
            Line::styled(l.as_str(), style)
        })
        .collect();
    let [top, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    f.render_widget(
        Line::from(vec![
            format!(" {title} ").yellow().bold(),
            format!(" line {}/{}", start + 1, lines.len()).dark_gray(),
        ]),
        top,
    );
    f.render_widget(Paragraph::new(shown).block(Block::new().padding(Padding::horizontal(1))), rest);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitmoji_shortcodes_become_emoji() {
        assert_eq!(emojify(":wrench: New versions release automatically"), "🔧 New versions release automatically");
        assert_eq!(emojify(":sparkles::bug: Both"), "✨🐛 Both");
        // Times, unknown codes and stray colons stay as written.
        assert_eq!(emojify("Meet at 10:30: :nope: and a:b"), "Meet at 10:30: :nope: and a:b");
        assert_eq!(emojify("ends with :"), "ends with :");
    }
}
