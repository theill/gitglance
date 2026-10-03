//! `.gitglanceignore` in the scanned folder: one pattern per line, gitignore style.
//! A pattern without `/` matches a folder name at any depth (`archive`, `*-old`);
//! a pattern with `/` matches a path relative to the folder (`unops/opportunityplus`, `clients/*`).
//! Ignoring a folder also ignores every repo below it. `#` starts a comment.

use std::io;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = ".gitglanceignore";

pub struct Ignore {
    file: PathBuf,
    lines: Vec<String>,
}

fn pattern(line: &str) -> Option<&str> {
    let p = line.split('#').next().unwrap_or("").trim();
    let p = p.strip_prefix("./").unwrap_or(p).trim_end_matches('/');
    (!p.is_empty()).then_some(p)
}

/// `*` matches within one path segment, `?` matches one character.
fn glob(p: &[u8], s: &[u8]) -> bool {
    match (p.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => glob(&p[1..], s) || (!s.is_empty() && s[0] != b'/' && glob(p, &s[1..])),
        (Some(b'?'), Some(&c)) if c != b'/' => glob(&p[1..], &s[1..]),
        (Some(a), Some(b)) if a == b => glob(&p[1..], &s[1..]),
        _ => false,
    }
}

fn matches(p: &str, rel: &str) -> bool {
    if rel.is_empty() {
        return false;
    }
    if p.contains('/') {
        // The repo itself or any folder above it.
        let mut prefix_ends = rel.match_indices('/').map(|(i, _)| i).collect::<Vec<_>>();
        prefix_ends.push(rel.len());
        prefix_ends.iter().any(|&end| glob(p.as_bytes(), rel[..end].as_bytes()))
    } else {
        rel.split('/').any(|seg| glob(p.as_bytes(), seg.as_bytes()))
    }
}

impl Ignore {
    pub fn load(root: &Path) -> Self {
        let file = root.join(FILE_NAME);
        let lines = std::fs::read_to_string(&file).unwrap_or_default().lines().map(String::from).collect();
        Ignore { file, lines }
    }

    pub fn is_ignored(&self, rel: &str) -> bool {
        self.lines.iter().filter_map(|l| pattern(l)).any(|p| matches(p, rel))
    }

    pub fn add(&mut self, rel: &str) -> io::Result<()> {
        self.lines.push(rel.to_string());
        self.save()
    }

    /// Drops every pattern that hides `rel` and returns them, so the caller can say what was removed.
    pub fn remove_matching(&mut self, rel: &str) -> io::Result<Vec<String>> {
        let mut removed = Vec::new();
        self.lines.retain(|l| match pattern(l) {
            Some(p) if matches(p, rel) => {
                removed.push(p.to_string());
                false
            }
            _ => true,
        });
        self.save()?;
        Ok(removed)
    }

    fn save(&self) -> io::Result<()> {
        let mut text = self.lines.join("\n");
        text.push('\n');
        std::fs::write(&self.file, text)
    }
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn patterns() {
        assert!(matches("skan", "skan"));
        assert!(matches("skan", "old/skan"));
        assert!(!matches("skan", "skanner"));
        assert!(matches("unops", "unops/unops-pdj"));
        assert!(matches("unops/*", "unops/unops-pdj"));
        assert!(!matches("unops/*", "unops"));
        assert!(matches("unops/unops-pdj", "unops/unops-pdj"));
        assert!(!matches("unops/unops-pdj", "unops/unops-pdj2"));
        assert!(matches("*-site", "babyart-site"));
        assert!(!matches("*-site", "babyart-site-x"));
        assert!(!matches("anything", ""));
        assert_eq!(super::pattern("apikiss   # old experiment"), Some("apikiss"));
        assert_eq!(super::pattern("# comment"), None);
    }
}
