use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Default)]
pub struct FileChange {
    pub code: String,
    pub path: String,
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

#[derive(Clone, Default)]
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

/// Stages everything (`.gitignore` still applies) and commits. Returns the short hash.
pub fn commit_all(repo: &Path, message: &str) -> Result<String, String> {
    git(repo, &["add", "-A"])?;
    run(repo, &["commit", "--quiet", "--file=-"], Some(message))?;
    Ok(git(repo, &["rev-parse", "--short", "HEAD"])?.trim().to_string())
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

pub fn fetch(repo: &Path) -> Result<String, String> {
    git(repo, &["fetch", "--all", "--prune", "--quiet"])
}

/// Lockfiles and minified output only add noise to an AI summary.
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

pub fn untracked_files(repo: &Path) -> Vec<String> {
    git(repo, &["ls-files", "--others", "--exclude-standard", "-z"])
        .unwrap_or_default()
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}
