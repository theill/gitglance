use crate::git::{self, Status};
use std::collections::hash_map::DefaultHasher;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SUMMARY_INSTRUCTION: &str = "You summarize the pending work in a local git repository (uncommitted changes and commits not yet pushed) for a developer who juggles 10-15 projects at once and needs to recall quickly where each one stands. Reply in plain text, with no markdown headings or bold.
Line 1: a headline of at most 70 characters that captures the gist of the work in progress.
Then a blank line, then 2-5 bullets starting with \"- \", one short line each, describing what changed, grouped by intent rather than by file.
Finally, only if relevant, one line starting with \"Heads-up: \" that flags anything risky: merge conflicts, secrets or credentials, debug leftovers, large generated or binary files, work that looks half-done, or a branch that has never been pushed.
No preamble and no closing remarks.";

const COMMIT_INSTRUCTION: &str = "Write the git commit message for the uncommitted changes below; all of them are committed together (git add -A). Describe only those changes, not the commits listed as not pushed yet. Match the style of the repository's recent commit messages shown under \"Recent commit messages\": the same language, prefixes such as gitmoji or conventional-commit types, casing and length. Keep the subject line to 72 characters at most. Add a blank line and a few short body lines only when the change is too big for the subject alone. Output only the commit message, with no quotes, code fences or commentary.";

const DIFF_BUDGET: usize = 60_000;
/// A commit subject needs far less of the diff, and a shorter prompt answers faster.
const COMMIT_DIFF_BUDGET: usize = 20_000;
const UNTRACKED_BUDGET: usize = 15_000;

fn model() -> String {
    std::env::var("GITGLANCE_MODEL").unwrap_or_else(|_| "haiku".into())
}

fn custom_cmd() -> Option<String> {
    std::env::var("GITGLANCE_AI_CMD").ok().filter(|c| !c.trim().is_empty())
}

fn clip(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Everything the model gets to see. Also the cache key, so an unchanged repo never costs a second call.
pub fn build_context(repo: &Path, s: &Status) -> String {
    context_with_budget(repo, s, DIFF_BUDGET)
}

fn context_with_budget(repo: &Path, s: &Status, diff_budget: usize) -> String {
    let name = repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut c = String::new();
    let _ = writeln!(c, "Repository: {name}");
    let _ = write!(c, "Branch: {}", s.branch);
    match (&s.upstream, s.tracking) {
        (Some(u), true) => {
            let _ = writeln!(c, " (tracking {u}, ahead {}, behind {})", s.ahead, s.behind);
        }
        (Some(u), false) => {
            let _ = writeln!(c, " (upstream {u} no longer exists)");
        }
        (None, _) if s.has_remote => {
            let _ = writeln!(c, " (not pushed to any remote yet)");
        }
        _ => {
            let _ = writeln!(c, " (local repository without remotes)");
        }
    }
    if s.stashes > 0 {
        let _ = writeln!(c, "Stashes: {}", s.stashes);
    }

    if !s.files.is_empty() {
        let _ = writeln!(c, "\nChanged files (git status, XY codes):");
        for f in &s.files {
            let _ = write!(c, "{} {}", f.code, f.path);
            if let (Some(a), Some(d)) = (f.added, f.removed) {
                let _ = write!(c, " (+{a} -{d})");
            }
            c.push('\n');
        }
    }

    if s.unpushed_count > 0 {
        let _ = writeln!(c, "\nCommits not pushed yet ({}):", s.unpushed_count);
        for l in &s.unpushed {
            let _ = writeln!(c, "{l}");
        }
    }

    if s.staged + s.unstaged + s.conflicts > 0 {
        let d = git::diff(repo, s.initial, true);
        if !d.is_empty() {
            let _ = writeln!(c, "\nDiff of tracked files (lockfiles omitted):");
            c.push_str(clip(&d, diff_budget));
            if d.len() > diff_budget {
                c.push_str("\n[diff truncated]\n");
            }
        }
    }

    if s.untracked > 0 {
        let mut budget = UNTRACKED_BUDGET;
        let files = git::untracked_files(repo);
        let _ = writeln!(c, "\nNew untracked files ({}):", files.len());
        for f in files.iter().take(200) {
            let _ = writeln!(c, "{f}");
        }
        for f in files.iter().take(15) {
            if budget == 0 {
                break;
            }
            let Ok(bytes) = std::fs::read(repo.join(f)) else { continue };
            if bytes.len() > 40_000 || bytes.iter().take(8000).any(|&b| b == 0) {
                continue;
            }
            let text = String::from_utf8_lossy(&bytes);
            let head: String = text.lines().take(120).collect::<Vec<_>>().join("\n");
            let head = clip(&head, budget);
            budget -= head.len();
            let _ = writeln!(c, "\n=== new file {f} ===\n{head}");
        }
    }
    c
}

fn cache_dir() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("gitglance")
}

fn cache_file(instruction: &str, context: &str) -> PathBuf {
    let mut h = DefaultHasher::new();
    instruction.hash(&mut h);
    model().hash(&mut h);
    custom_cmd().hash(&mut h);
    context.hash(&mut h);
    cache_dir().join(format!("{:016x}.txt", h.finish()))
}

fn read_cache(instruction: &str, context: &str) -> Option<String> {
    std::fs::read_to_string(cache_file(instruction, context)).ok().filter(|s| !s.trim().is_empty())
}

pub fn cached(context: &str) -> Option<String> {
    read_cache(SUMMARY_INSTRUCTION, context)
}

pub fn summarize(context: &str, force: bool) -> Result<String, String> {
    ask(SUMMARY_INSTRUCTION, context, force)
}

pub fn commit_message(repo: &Path, s: &Status, force: bool) -> Result<String, String> {
    let mut context = context_with_budget(repo, s, COMMIT_DIFF_BUDGET);
    let recent = git::git(repo, &["log", "-12", "--format=%s"]).unwrap_or_default();
    if !recent.trim().is_empty() {
        context.push_str("\nRecent commit messages:\n");
        context.push_str(&recent);
    }
    let msg = ask(COMMIT_INSTRUCTION, &context, force)?;
    let msg: Vec<&str> = msg.lines().filter(|l| !l.trim_start().starts_with("```")).collect();
    Ok(msg.join("\n").trim().trim_matches('"').trim().to_string())
}

fn ask(instruction: &str, context: &str, force: bool) -> Result<String, String> {
    if !force {
        if let Some(s) = read_cache(instruction, context) {
            return Ok(s);
        }
    }
    let custom = custom_cmd();
    let (mut cmd, input) = match &custom {
        Some(c) => {
            let mut k = Command::new("sh");
            k.arg("-c").arg(c);
            (k, format!("{instruction}\n\n{context}"))
        }
        None => {
            let mut k = Command::new("claude");
            k.args([
                "-p",
                instruction,
                "--model",
                &model(),
                "--tools",
                "",
                "--strict-mcp-config",
                "--no-session-persistence",
                // Skipping user settings (plugins, hooks) saves over a second of startup per call.
                "--setting-sources",
                "",
                // Extended thinking is on by default and made a one-line answer take ~12s instead of ~2s.
                "--settings",
                r#"{"alwaysThinkingEnabled":false}"#,
            ]);
            k.env("MAX_THINKING_TOKENS", "0");
            (k, context.to_string())
        }
    };
    // A neutral working dir keeps the CLI from loading the repo's own CLAUDE.md and settings.
    cmd.current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| match &custom {
        Some(_) => format!("could not run GITGLANCE_AI_CMD: {e}"),
        None => format!("could not run `claude` ({e}); install Claude Code or set GITGLANCE_AI_CMD"),
    })?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = writer.join();
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let msg = err.lines().chain(text.lines()).find(|l| !l.trim().is_empty()).unwrap_or("AI command failed");
        return Err(msg.trim().to_string());
    }
    if text.is_empty() {
        return Err("empty response from AI".into());
    }
    let file = cache_file(instruction, context);
    let _ = std::fs::create_dir_all(file.parent().unwrap());
    let _ = std::fs::write(file, &text);
    Ok(text)
}

pub fn headline(summary: &str) -> &str {
    let line = summary.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let line = line.trim_start_matches(|c: char| c == '#' || c == '*' || c == '-' || c.is_whitespace());
    let line = line.strip_prefix("Headline:").unwrap_or(line);
    line.trim().trim_end_matches("**")
}
