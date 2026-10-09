use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Default, PartialEq)]
pub struct FileChange {
    pub code: String,
    pub path: String,
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

#[derive(Clone, Default, PartialEq)]
pub struct Status {
    pub branch: String,
    pub upstream: Option<String>,
    /// Upstream is configured and still exists, so ahead/behind are meaningful.
    pub tracking: bool,
    pub ahead: u32,
    pub behind: u32,
    pub has_remote: bool,
    pub initial: bool,
    pub files: Vec<FileChange>,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicts: u32,
    pub insertions: u32,
    pub deletions: u32,
    pub stashes: u32,
    pub unpushed_count: u32,
    pub unpushed: Vec<String>,
    /// Upstream commits you are behind on, newest first, as `hash<TAB>age<TAB>subject` (at most `INCOMING_MAX`).
    pub incoming: Vec<String>,
    pub last_commit_ts: Option<i64>,
    pub last_commit_subject: String,
}

impl Status {
    pub fn dirty(&self) -> bool {
        !self.files.is_empty()
    }

    pub fn needs_attention(&self) -> bool {
        self.dirty() || self.unpushed_count > 0 || self.behind > 0
    }

    fn add(&mut self, xy: &str, path: &str) {
        let b = xy.as_bytes();
        if b.first() != Some(&b'.') {
            self.staged += 1;
        }
        if b.get(1) != Some(&b'.') {
            self.unstaged += 1;
        }
        self.files.push(FileChange {
            code: xy.replace('.', " "),
            path: path.to_string(),
            ..Default::default()
        });
    }
}

/// Runs git without taking optional locks, so it never fights with agents committing in the background.
pub fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    run(repo, args, None)
}

fn run(repo: &Path, args: &[&str], input: Option<&str>) -> Result<String, String> {
    // A repo's own config could otherwise make a passive scan run programs (fsmonitor hook, textconv filters).
    let mut child = Command::new("git")
        .args(["-c", "core.fsmonitor=false"])
        .arg("--no-optional-locks")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes -o ConnectTimeout=10")
        .env("LC_ALL", "C")
        .env("GIT_EDITOR", "true")
        .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
        use std::io::Write;
        let _ = stdin.write_all(text.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        // Hooks and "nothing to commit" report on stdout, so fall back to it.
        let err = String::from_utf8_lossy(&out.stderr);
        let err = if err.trim().is_empty() { String::from_utf8_lossy(&out.stdout) } else { err };
        Err(err.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" · "))
    }
}

/// New files that `commit_all` leaves out because they look like secrets (a `.env` or key that is not gitignored).
pub fn secret_new_files(repo: &Path) -> Vec<String> {
    untracked_files(repo).into_iter().filter(|f| looks_secret(f)).collect()
}

/// Stages everything except new secret-looking files (`.gitignore` still applies) and commits.
/// Returns the short hash and the files that were held back.
pub fn commit_all(repo: &Path, message: &str) -> Result<(String, Vec<String>), String> {
    let held_back = secret_new_files(repo);
    let excludes: Vec<String> = held_back.iter().map(|f| format!(":(exclude,literal){f}")).collect();
    let mut args = vec!["add", "-A", "--", "."];
    args.extend(excludes.iter().map(String::as_str));
    git(repo, &args)?;
    run(repo, &["commit", "--quiet", "--file=-"], Some(message))?;
    Ok((git(repo, &["rev-parse", "--short", "HEAD"])?.trim().to_string(), held_back))
}

/// Brings in commits from the upstream. Fast-forward only, or a rebase of local commits when the branch has
/// diverged. A rebase that hits a conflict is aborted, so the repo is never left half-rebased.
pub fn pull(repo: &Path, rebase: bool) -> Result<(), String> {
    let mode = if rebase { "--rebase" } else { "--ff-only" };
    match git(repo, &["pull", "--quiet", mode, "--autostash"]) {
        Ok(_) => Ok(()),
        Err(_) if rebase && rebase_in_progress(repo) => {
            let _ = git(repo, &["rebase", "--abort"]);
            Err(format!(
                "the rebase hit a conflict, so it was undone and nothing changed. Resolve it by hand: cd {} && git pull --rebase",
                repo.display()
            ))
        }
        Err(e) => Err(e),
    }
}

fn rebase_in_progress(repo: &Path) -> bool {
    ["rebase-merge", "rebase-apply"].iter().any(|dir| {
        git(repo, &["rev-parse", "--git-path", dir])
            .map(|p| {
                let p = PathBuf::from(p.trim());
                if p.is_relative() { repo.join(p) } else { p }.exists()
            })
            .unwrap_or(false)
    })
}

/// Pushes the current branch, setting an upstream on first push.
pub fn push(repo: &Path, s: &Status) -> Result<String, String> {
    if s.branch == "(detached)" {
        return Err("detached HEAD, nothing to push".into());
    }
    if s.tracking {
        git(repo, &["push", "--quiet"])?;
        return Ok(format!("pushed to {}", s.upstream.as_deref().unwrap_or("upstream")));
    }
    let remotes = git(repo, &["remote"])?;
    let remote = if remotes.lines().any(|r| r == "origin") {
        "origin"
    } else {
        remotes.lines().next().ok_or("no remote to push to")?
    };
    git(repo, &["push", "--quiet", "--set-upstream", remote, "HEAD"])?;
    Ok(format!("pushed {} to {remote}", s.branch))
}

pub fn discover(root: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if is_repo(root) {
        out.push(root.to_path_buf());
    }
    walk(root, depth, &mut out);
    out
}

fn is_repo(p: &Path) -> bool {
    p.join(".git").exists()
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut dirs: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            !n.starts_with('.') && n != "node_modules"
        })
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    for d in dirs {
        if is_repo(&d) {
            out.push(d);
        } else {
            walk(&d, depth - 1, out);
        }
    }
}

pub fn status(repo: &Path) -> Result<Status, String> {
    let raw = git(repo, &["status", "--porcelain=v2", "--branch", "--show-stash", "-z"])?;
    let mut s = Status::default();
    let mut recs = raw.split('\0');
    while let Some(rec) = recs.next() {
        if let Some(h) = rec.strip_prefix("# ") {
            if let Some(v) = h.strip_prefix("branch.head ") {
                s.branch = v.to_string();
            } else if let Some(v) = h.strip_prefix("branch.upstream ") {
                s.upstream = Some(v.to_string());
            } else if let Some(v) = h.strip_prefix("branch.ab ") {
                s.tracking = true;
                for part in v.split(' ') {
                    if let Some(n) = part.strip_prefix('+') {
                        s.ahead = n.parse().unwrap_or(0);
                    } else if let Some(n) = part.strip_prefix('-') {
                        s.behind = n.parse().unwrap_or(0);
                    }
                }
            } else if h == "branch.oid (initial)" {
                s.initial = true;
            } else if let Some(v) = h.strip_prefix("stash ") {
                s.stashes = v.parse().unwrap_or(0);
            }
            continue;
        }
        match rec.chars().next() {
            Some('1') => {
                let f: Vec<&str> = rec.splitn(9, ' ').collect();
                if f.len() == 9 {
                    s.add(f[1], f[8]);
                }
            }
            Some('2') => {
                let f: Vec<&str> = rec.splitn(10, ' ').collect();
                recs.next(); // original path of the rename
                if f.len() == 10 {
                    s.add(f[1], f[9]);
                }
            }
            Some('u') => {
                let f: Vec<&str> = rec.splitn(11, ' ').collect();
                if f.len() == 11 {
                    s.conflicts += 1;
                    s.files.push(FileChange { code: "UU".into(), path: f[10].into(), ..Default::default() });
                }
            }
            Some('?') => {
                s.untracked += 1;
                s.files.push(FileChange { code: "??".into(), path: rec[2..].into(), ..Default::default() });
            }
            _ => {}
        }
    }

    if s.staged + s.unstaged + s.conflicts > 0 {
        let numstat = if s.initial {
            git(repo, &["diff", "--cached", "--numstat"]).unwrap_or_default()
                + &git(repo, &["diff", "--numstat"]).unwrap_or_default()
        } else {
            git(repo, &["diff", "HEAD", "--numstat"]).unwrap_or_default()
        };
        for line in numstat.lines() {
            let mut p = line.splitn(3, '\t');
            let (Some(a), Some(d), Some(path)) = (p.next(), p.next(), p.next()) else { continue };
            let (a, d) = (a.parse::<u32>().ok(), d.parse::<u32>().ok());
            s.insertions += a.unwrap_or(0);
            s.deletions += d.unwrap_or(0);
            if let Some(f) = s.files.iter_mut().find(|f| f.path == path) {
                f.added = Some(f.added.unwrap_or(0) + a.unwrap_or(0));
                f.removed = Some(f.removed.unwrap_or(0) + d.unwrap_or(0));
            }
        }
    }

    if !s.initial {
        if let Ok(out) = git(repo, &["log", "-1", "--format=%ct%x09%s"]) {
            if let Some((ts, subject)) = out.trim_end().split_once('\t') {
                s.last_commit_ts = ts.parse().ok();
                s.last_commit_subject = subject.to_string();
            }
        }
        let range: Option<&[&str]> = if s.tracking {
            s.has_remote = true;
            (s.ahead > 0).then_some(&["@{u}..HEAD"][..])
        } else {
            s.has_remote = git(repo, &["remote"]).map(|r| !r.trim().is_empty()).unwrap_or(false);
            s.has_remote.then_some(&["HEAD", "--not", "--remotes"][..])
        };
        if s.tracking && s.behind > 0 {
            let max = format!("--max-count={INCOMING_MAX}");
            if let Ok(out) = git(repo, &["log", &max, "--format=%h%x09%ct%x09%s", "HEAD..@{u}"]) {
                s.incoming = out.lines().map(String::from).collect();
            }
        }
        if let Some(range) = range {
            let mut args = vec!["log", "--format=%h %s"];
            args.extend_from_slice(range);
            if let Ok(out) = git(repo, &args) {
                let lines: Vec<String> = out.lines().map(String::from).collect();
                s.unpushed_count = lines.len() as u32;
                s.unpushed = lines.into_iter().take(30).collect();
            }
        }
    }
    Ok(s)
}

/// How many incoming commits the detail page lists; `d` shows them all.
pub const INCOMING_MAX: usize = 50;

pub fn fetch(repo: &Path) -> Result<String, String> {
    git(repo, &["fetch", "--all", "--prune", "--quiet"])
}

const LOG_PATCH: [&str; 7] =
    ["log", "--no-color", "--no-ext-diff", "--no-textconv", "--stat", "--patch", "--format=fuller"];

/// Commits on this branch that the remote doesn't have yet, with their patches.
pub fn unpushed_log(repo: &Path, s: &Status) -> String {
    if s.unpushed_count == 0 {
        return String::new();
    }
    let mut a = LOG_PATCH.to_vec();
    if s.tracking {
        a.push("@{u}..HEAD");
    } else {
        a.extend_from_slice(&["HEAD", "--not", "--remotes"]);
    }
    git(repo, &a).unwrap_or_default()
}

/// Commits on the upstream that `u` would pull in.
pub fn incoming_log(repo: &Path, s: &Status) -> String {
    if !s.tracking || s.behind == 0 {
        return String::new();
    }
    let mut a = LOG_PATCH.to_vec();
    a.push("HEAD..@{u}");
    git(repo, &a).unwrap_or_default()
}

pub fn show(repo: &Path, hash: &str) -> String {
    git(repo, &["show", "--no-color", "--no-ext-diff", "--no-textconv", "--stat", "--patch", "--format=fuller", hash])
        .unwrap_or_else(|e| format!("git show failed: {e}"))
}

/// The diff of one changed file; for an untracked file, its contents.
pub fn file_diff(repo: &Path, initial: bool, f: &FileChange) -> String {
    if f.code == "??" {
        if f.path.ends_with('/') {
            let files = git(repo, &["ls-files", "--others", "--exclude-standard", "--", &f.path]).unwrap_or_default();
            let list: String = files.lines().map(|l| format!("?? {l}\n")).collect();
            return format!("### New folder {} (untracked)\n\n{list}", f.path);
        }
        return match std::fs::read(repo.join(&f.path)) {
            Ok(b) if b.iter().take(8000).any(|&c| c == 0) => {
                format!("### New file {} (untracked, binary, {} bytes)", f.path, b.len())
            }
            Ok(b) => {
                let body: String = String::from_utf8_lossy(&b).lines().map(|l| format!("+{l}\n")).collect();
                format!("### New file {} (untracked)\n\n{body}", f.path)
            }
            Err(e) => format!("Could not read {}: {e}", f.path),
        };
    }
    let run = |base: &[&str]| {
        let mut a = vec!["diff", "--no-color", "--no-ext-diff", "--no-textconv"];
        a.extend_from_slice(base);
        a.extend_from_slice(&["--", &f.path]);
        git(repo, &a).unwrap_or_default()
    };
    let d = if initial { run(&["--cached"]) + &run(&[]) } else { run(&["HEAD"]) };
    if d.trim().is_empty() {
        "No line changes (a mode change, or a submodule)".into()
    } else {
        d
    }
}

/// Lockfiles and minified output only add noise to an AI summary. Secrets are not handled here but by
/// `without_secret_files`, so one rule (`looks_secret`) decides for tracked and untracked files alike.
const NOISE: [&str; 6] = [
    ":(exclude)*package-lock.json",
    ":(exclude)*.lock",
    ":(exclude)*pnpm-lock.yaml",
    ":(exclude)*Package.resolved",
    ":(exclude)*.min.js",
    ":(exclude)*.map",
];

/// Staged and unstaged changes to tracked files.
pub fn diff(repo: &Path, initial: bool, skip_noise: bool) -> String {
    let run = |base: &[&str]| {
        let mut a: Vec<&str> = vec!["diff", "--no-color", "--no-ext-diff", "--no-textconv"];
        a.extend_from_slice(base);
        if skip_noise {
            a.extend_from_slice(&["--", "."]);
            a.extend_from_slice(&NOISE);
        }
        git(repo, &a).unwrap_or_default()
    };
    if initial {
        run(&["--cached"]) + &run(&[])
    } else {
        run(&["HEAD"])
    }
}

/// Drops every file section of a unified diff whose old or new path looks secret, so renames and
/// mixed-case names are caught too. Returns the filtered diff and how many files were dropped.
pub fn without_secret_files(diff: &str) -> (String, usize) {
    let mut out = String::with_capacity(diff.len());
    let (mut skipping, mut dropped) = (false, 0);
    for line in diff.split_inclusive('\n') {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            skipping = diff_paths(rest.trim_end()).iter().any(|p| looks_secret(p));
            dropped += skipping as usize;
        }
        if !skipping {
            out.push_str(line);
        }
    }
    (out, dropped)
}

/// Both paths from the rest of a `diff --git a/<old> b/<new>` header. Ambiguous splits (paths containing
/// " b/") yield every candidate, so a secret can't hide behind an odd name.
fn diff_paths(rest: &str) -> Vec<String> {
    let rest = rest.replace('"', "");
    let mut paths = vec![rest.clone()];
    for (i, _) in rest.match_indices(" b/") {
        paths.push(rest[..i].trim_start_matches("a/").to_string());
        paths.push(rest[i + 3..].to_string());
    }
    paths
}

/// Files whose contents must never go into an AI prompt; only their names are listed.
pub fn looks_secret(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    name == ".env"
        || name.starts_with(".env.")
        || name.contains("id_rsa")
        || name.contains("id_ed25519")
        || name.contains("credentials")
        || name.contains("secret")
        || [".pem", ".key", ".p8", ".p12", ".keystore", ".mobileprovision"].iter().any(|ext| name.ends_with(ext))
}

pub fn untracked_files(repo: &Path) -> Vec<String> {
    git(repo, &["ls-files", "--others", "--exclude-standard", "-z"])
        .unwrap_or_default()
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// How many days and weeks the activity graphs cover.
pub const DAYS: usize = 42;
pub const WEEKS: usize = 24;

#[derive(Clone, Default)]
pub struct Author {
    pub name: String,
    pub commits: u32,
    pub added: u32,
    pub removed: u32,
}

/// Commit activity across all branches (merges left out), for the detail page.
#[derive(Clone, Default)]
pub struct Activity {
    pub total_commits: u32,
    pub first_commit_ts: Option<i64>,
    /// Today as days since 1970-01-01, in local time.
    pub today: i64,
    /// Commits per day, oldest first; the last one is today.
    pub days: Vec<u32>,
    /// Commits per week (weeks start on Monday), oldest first; the last one is this week.
    pub weeks: Vec<u32>,
    /// Authors over the `WEEKS` window, most commits first.
    pub authors: Vec<Author>,
}

pub fn activity(repo: &Path) -> Result<Activity, String> {
    let today = local_today();
    let this_monday = today - weekday(today);
    let first_day = this_monday - 7 * (WEEKS as i64 - 1);
    let since = format!("--since={}.days.ago", today - first_day + 1);
    let log = git(
        repo,
        &[
            "log",
            "--exclude=refs/stash",
            "--all",
            "--no-merges",
            &since,
            "--date=format-local:%Y-%m-%d",
            "--format=%x01%ad%x09%aN",
            "--shortstat",
        ],
    )?;
    let mut a = Activity { today, days: vec![0; DAYS], weeks: vec![0; WEEKS], ..Default::default() };
    let mut authors: Vec<Author> = Vec::new();
    let mut current: Option<usize> = None;
    for line in log.lines() {
        if let Some(rest) = line.strip_prefix('\u{1}') {
            current = None;
            let Some((date, name)) = rest.split_once('\t') else { continue };
            let Some(day) = parse_day(date) else { continue };
            if day < first_day || day > today {
                continue;
            }
            let back = (today - day) as usize;
            if back < DAYS {
                a.days[DAYS - 1 - back] += 1;
            }
            let weeks_back = ((this_monday - (day - weekday(day))) / 7) as usize;
            if weeks_back < WEEKS {
                a.weeks[WEEKS - 1 - weeks_back] += 1;
            }
            let i = match authors.iter().position(|x| x.name == name) {
                Some(i) => i,
                None => {
                    authors.push(Author { name: name.to_string(), ..Default::default() });
                    authors.len() - 1
                }
            };
            authors[i].commits += 1;
            current = Some(i);
        } else if let (Some(i), true) = (current, line.contains("changed")) {
            // " 3 files changed, 10 insertions(+), 2 deletions(-)"
            for part in line.split(',') {
                let n: u32 = part.split_whitespace().next().and_then(|n| n.parse().ok()).unwrap_or(0);
                if part.contains("insertion") {
                    authors[i].added += n;
                } else if part.contains("deletion") {
                    authors[i].removed += n;
                }
            }
        }
    }
    authors.sort_by(|x, y| y.commits.cmp(&x.commits).then_with(|| x.name.cmp(&y.name)));
    a.authors = authors;
    a.total_commits = git(repo, &["rev-list", "--count", "--no-merges", "--exclude=refs/stash", "--all"])
        .ok()
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0);
    a.first_commit_ts = git(repo, &["log", "--exclude=refs/stash", "--all", "--max-parents=0", "--format=%ct"])
        .ok()
        .and_then(|out| out.lines().filter_map(|l| l.trim().parse().ok()).min());
    Ok(a)
}

/// Today's local date as days since 1970-01-01, from `date`; UTC if that isn't available.
fn local_today() -> i64 {
    Command::new("date")
        .arg("+%Y-%m-%d")
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| parse_day(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or_else(|| {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            secs.div_euclid(86_400)
        })
}

fn parse_day(s: &str) -> Option<i64> {
    let mut p = s.splitn(3, '-').map(|n| n.parse::<i64>().ok());
    Some(days_from_civil(p.next()??, p.next()??, p.next()??))
}

/// Monday is 0. 1970-01-01 was a Thursday.
pub fn weekday(day: i64) -> i64 {
    (day + 3).rem_euclid(7)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The (year, month, day) for days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { yoe + era * 400 + 1 } else { yoe + era * 400 }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        for day in [-1, 0, 59, 365, 11_016, 20_734, 20_735] {
            let (y, m, d) = civil_from_days(day);
            assert_eq!(days_from_civil(y, m as i64, d as i64), day);
        }
        assert_eq!(parse_day("1970-01-01"), Some(0));
        assert_eq!(civil_from_days(parse_day("2026-10-08").unwrap()), (2026, 10, 8));
        assert_eq!(weekday(parse_day("2026-10-05").unwrap()), 0); // a Monday
        assert_eq!(weekday(parse_day("2026-10-11").unwrap()), 6);
    }
}
