use std::fmt::Write;

#[derive(Debug, Clone)]
pub struct Issue {
    pub message: String,
}

impl Issue {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

pub trait IssueSink {
    fn push_issue(&mut self, message: impl Into<String>);
}

impl IssueSink for Vec<Issue> {
    fn push_issue(&mut self, message: impl Into<String>) {
        self.push(Issue::new(message));
    }
}

/// Format a list of issues for human display, with a leading count line.
/// Returns an empty string if `issues` is empty.
pub fn render(issues: &[Issue]) -> String {
    if issues.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let n = issues.len();
    let _ = writeln!(
        out,
        "{n} validation {}:",
        if n == 1 { "problem" } else { "problems" }
    );
    for issue in issues {
        let _ = writeln!(out, "  - {}", issue.message);
    }
    // trim trailing newline so callers don't double-space
    while out.ends_with('\n') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_empty_as_empty() {
        assert_eq!(render(&[]), "");
    }

    #[test]
    fn renders_singular_count() {
        let s = render(&[Issue::new("nope")]);
        assert!(s.contains("1 validation problem:"));
        assert!(s.contains("- nope"));
    }

    #[test]
    fn renders_plural_count() {
        let s = render(&[Issue::new("a"), Issue::new("b")]);
        assert!(s.contains("2 validation problems:"));
        assert!(s.contains("- a"));
        assert!(s.contains("- b"));
    }

    #[test]
    fn sink_pushes_messages() {
        let mut v: Vec<Issue> = Vec::new();
        v.push_issue("hello");
        v.push_issue(String::from("world"));
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].message, "hello");
        assert_eq!(v[1].message, "world");
    }
}
