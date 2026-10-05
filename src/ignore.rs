//! `.gitglanceignore` in the scanned folder: one repo per line, as its path relative to that folder
//! (`apikiss`, `unops/opportunityplus`). A line hides exactly that repo and nothing else: no globs, and
//! never repos nested below it or with the same name elsewhere. `#` starts a comment.

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

fn matches(p: &str, rel: &str) -> bool {
    !rel.is_empty() && p == rel
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
        assert!(!matches("skan", "old/skan"), "same name elsewhere is not hidden");
        assert!(!matches("skan", "skanner"));
        assert!(!matches("unops", "unops/unops-pdj"), "repos nested below are not hidden");
        assert!(matches("unops/unops-pdj", "unops/unops-pdj"));
        assert!(!matches("unops/unops-pdj", "unops/unops-pdj2"));
        assert!(!matches("unops/*", "unops/unops-pdj"), "no globs");
        assert!(!matches("anything", ""));
        assert_eq!(super::pattern("apikiss   # old experiment"), Some("apikiss"));
        assert_eq!(super::pattern("./unops/ai-bob/"), Some("unops/ai-bob"));
        assert_eq!(super::pattern("# comment"), None);
    }
}
