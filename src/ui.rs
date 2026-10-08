use crate::git::Status;
use crate::{ai, App, Modal, Repo, SummaryState, View};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Cell, Clear, Padding, Paragraph, Row, Table, Wrap};
use std::time::{SystemTime, UNIX_EPOCH};

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
    }
    modal(f, body, app);
    footer(f, foot, app);
}

fn modal(f: &mut Frame, area: Rect, app: &App) {
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
    if busy > 0 {
        spans.push(format!("   {} scanning {busy}", spin(app.tick)).cyan());
    }
    if thinking > 0 {
        spans.push(format!("   {} summarizing {thinking}", spin(app.tick)).magenta());
    }
    spans.push(if app.show_all { "   [all repos]".dark_gray() } else { "   [with changes]".dark_gray() });
    if app.filtering || !app.filter.is_empty() {
        spans.push(format!("   /{}", app.filter).yellow());
        if app.filtering {
            spans.push("▏".yellow());
        }
    }
    f.render_widget(Line::from(spans), area);
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
        _ if app.modal.is_some() => &[("y/⏎", "yes"), ("n/esc", "cancel")],
        _ if app.filtering => &[("type", "filter"), ("⏎", "done"), ("esc", "clear")],
        View::List => &[
            ("↑↓", "move"),
            ("⏎", "details"),
            ("s", "summarize"),
            ("S", "summarize all"),
            ("c", "commit"),
            ("P", "push"),
            ("u", "pull"),
            ("d", "diff"),
            ("a", "all/changed"),
            ("x", "ignore"),
            ("I", "show ignored"),
            ("/", "filter"),
            ("r", "refresh"),
            ("f", "fetch"),
            ("q", "quit"),
        ],
        View::Detail { .. } => &[
            ("esc", "back"),
            ("↑↓", "select"),
            ("⏎", "open"),
            ("n/p", "next/prev repo"),
            ("s", "re-summarize"),
            ("c", "commit"),
            ("P", "push"),
            ("u", "pull"),
            ("d", "diff"),
            ("o", "open folder"),
            ("r", "refresh"),
            ("q", "quit"),
        ],
        View::Diff { .. } => &[("esc", "back"), ("↑↓", "scroll"), ("space", "page"), ("g/G", "top/bottom"), ("q", "quit")],
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, label) in keys {
        spans.push(k.to_string().cyan().bold());
        spans.push(format!(" {label}   ").dark_gray());
    }
    f.render_widget(Line::from(spans), area);
}

fn state_spans(s: &Status) -> Vec<Span<'static>> {
    let mut v = Vec::new();
    if s.conflicts > 0 {
        v.push(format!("U{} ", s.conflicts).red().bold());
    }
    if s.staged > 0 {
        v.push(format!("S{} ", s.staged).green());
    }
    if s.unstaged > 0 {
        v.push(format!("M{} ", s.unstaged).yellow());
    }
    if s.untracked > 0 {
        v.push(format!("?{} ", s.untracked).blue());
    }
    if s.stashes > 0 {
        v.push(format!("≡{}", s.stashes).dark_gray());
    }
    v
}

fn lines_spans(s: &Status) -> Vec<Span<'static>> {
    let mut v = Vec::new();
    if s.insertions > 0 {
        v.push(format!("+{} ", s.insertions).green());
    }
    if s.deletions > 0 {
        v.push(format!("-{}", s.deletions).red());
    }
    v
}

fn sync_spans(s: &Status) -> Vec<Span<'static>> {
    if s.initial {
        return vec!["no commits".dark_gray()];
    }
    if s.tracking {
        let mut v = Vec::new();
        if s.ahead > 0 {
            v.push(format!("↑{} ", s.ahead).cyan().bold());
        }
        if s.behind > 0 {
            v.push(format!("↓{}", s.behind).magenta().bold());
        }
        if v.is_empty() {
            v.push("✓".green());
        }
        return v;
    }
    if !s.has_remote {
        return vec!["local".dark_gray()];
    }
    if s.unpushed_count > 0 {
        vec![format!("↑{} ", s.unpushed_count).cyan().bold(), "unpushed".cyan()]
    } else if s.upstream.is_some() {
        vec!["upstream gone".dark_gray()]
    } else {
        vec!["no upstream".dark_gray()]
    }
}

fn summary_cell(r: &Repo, tick: usize) -> Line<'static> {
    match &r.summary {
        SummaryState::Done(t) => Line::from(ai::headline(t).to_string()),
        SummaryState::Pending => Line::from(format!("{} summarizing…", spin(tick)).magenta()),
        SummaryState::Failed(e) => Line::from(format!("AI failed: {e}").red()),
        SummaryState::None if r.attention() => Line::from("press s to summarize".dark_gray()),
        SummaryState::None => Line::from("clean".dark_gray()),
    }
}

fn row(r: &Repo, tick: usize, name_w: usize) -> Row<'static> {
    let name_style = match &r.status {
        Some(Ok(s)) if s.conflicts > 0 => Style::new().red().bold(),
        Some(Ok(s)) if s.dirty() => Style::new().yellow().bold(),
        Some(Ok(s)) if s.needs_attention() => Style::new().cyan().bold(),
        Some(Err(_)) => Style::new().red(),
        _ => Style::new().dark_gray(),
    };
    if r.ignored {
        let mut cells = vec![Cell::from(Span::styled(trunc(&r.name, name_w), Style::new().dark_gray().crossed_out()))];
        cells.extend((0..6).map(|_| Cell::from("")));
        cells.push(Cell::from("ignored  (x to un-ignore)".dark_gray()));
        return Row::new(cells);
    }
    let name = Cell::from(Span::styled(trunc(&r.name, name_w), name_style));
    let s = match &r.status {
        None => return Row::new(vec![name, Cell::from(spin(tick).to_string().cyan())]),
        Some(Err(e)) => {
            let blank = || Cell::from("");
            return Row::new(vec![
                name,
                blank(),
                blank(),
                blank(),
                blank(),
                blank(),
                blank(),
                Cell::from(format!("git error: {e}").red()),
            ]);
        }
        Some(Ok(s)) => s,
    };
    let branch_style = match s.branch.as_str() {
        "main" | "master" => Style::new(),
        _ => Style::new().magenta(),
    };
    let files = if s.dirty() { s.files.len().to_string().bold() } else { "·".dark_gray() };
    let mut cells = vec![
        name,
        Cell::from(Span::styled(trunc(&s.branch, 18), branch_style)),
        Cell::from(Line::from(files).right_aligned()),
        Cell::from(Line::from(state_spans(s))),
        Cell::from(Line::from(lines_spans(s))),
        Cell::from(Line::from(sync_spans(s))),
        Cell::from(s.last_commit_ts.map(ago).unwrap_or_default().dark_gray()),
        Cell::from(summary_cell(r, tick)),
    ];
    if r.busy {
        cells[6] = Cell::from(spin(tick).to_string().cyan());
    }
    Row::new(cells)
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
    let header = Row::new(["REPO", "BRANCH", "FILES", "STATE", "+/-", "SYNC", "AGE", "SUMMARY"])
        .style(Style::new().dark_gray().bold())
        .bottom_margin(0);
    let rows: Vec<Row> = vis.iter().map(|&i| row(&app.repos[i], app.tick, name_w)).collect();
    let widths = [
        Constraint::Length(name_w as u16),
        Constraint::Length(18),
        Constraint::Length(5),
        Constraint::Length(13),
        Constraint::Length(13),
        Constraint::Length(12),
        Constraint::Length(4),
        Constraint::Fill(1),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .column_spacing(2)
        .row_highlight_style(Style::new().bg(SELECTED_BG))
        .highlight_symbol("▌ ");
    app.table_state.select(Some(pos));
    f.render_stateful_widget(table, area.inner(Margin::new(0, 1)), &mut app.table_state);
}

fn section(lines: &mut Vec<Line<'static>>, title: String, hint: &str) {
    lines.push(Line::raw(""));
    let hint = if hint.is_empty() { String::new() } else { format!("  ({hint})") };
    lines.push(Line::from(vec![format!("── {title}").cyan().bold(), hint.dark_gray()]));
}

fn detail(f: &mut Frame, area: Rect, app: &mut App) {
    let View::Detail { ref path, scroll, sel } = app.view else { return };
    let Some(i) = app.repo_idx(&path.clone()) else { return };
    let (lines, items) = detail_lines(&app.repos[i], app.tick, sel);

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
    let mut lines: Vec<Line<'static>> = vec![Line::from(vec![
        r.name.clone().yellow().bold(),
        "   ".into(),
        r.path.display().to_string().dark_gray(),
    ])];
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
    if let Some(ts) = s.last_commit_ts {
        lines.push(Line::from(vec![
            "last commit ".dark_gray(),
            match ago(ts).as_str() {
                "now" => "just now  ".to_string(),
                a => format!("{a} ago  "),
            }
            .into(),
            s.last_commit_subject.clone().italic(),
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
            let head = ai::headline(t).to_string();
            let mut first = true;
            for l in t.lines() {
                if first && !l.trim().is_empty() {
                    lines.push(Line::from(head.clone().bold()));
                    first = false;
                } else if l.trim_start().starts_with("Heads-up") {
                    lines.push(Line::from(l.to_string().yellow()));
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

    if s.unpushed_count > 0 {
        section(&mut lines, format!("Unpushed commits ({})", s.unpushed_count), "⏎ shows a commit");
        for c in &s.unpushed {
            let (hash, msg) = c.split_once(' ').unwrap_or((c, ""));
            let selected = sel == Some(items.len());
            items.push(lines.len());
            lines.push(item_line(vec![hash.to_string().yellow(), " ".into(), msg.to_string().into()], selected));
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
