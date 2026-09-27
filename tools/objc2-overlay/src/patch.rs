//! Applies unified diffs (as `diff -u` or `git diff` write them) strictly:
//! every hunk must match exactly at the line it names. The sources are
//! pinned by checksum, so any mismatch means the patch or the pin is wrong,
//! never that fuzz is wanted.

use std::fs;
use std::path::Path;

#[derive(Debug, PartialEq)]
struct Hunk {
    old_start: usize,
    old_len: usize,
    new_len: usize,
    /// (' ' | '-' | '+', text without the newline)
    lines: Vec<(char, String)>,
}

#[derive(Debug, PartialEq)]
struct FilePatch {
    path: String,
    hunks: Vec<Hunk>,
}

fn strip_prefix(header: &str, marker: &str) -> Result<String, String> {
    let path = header.strip_prefix(marker).ok_or_else(|| format!("expected `{marker}`: {header}"))?;
    // `diff -u` may add a tab and a timestamp.
    let path = path.split('\t').next().unwrap_or(path).trim_end();
    let path = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")).unwrap_or(path);
    Ok(path.to_string())
}

/// Only files the fork changes by hand: relative, inside the crate, and
/// never the generated bindings.
fn check_path(path: &str) -> Result<(), String> {
    let bad = path.is_empty()
        || path.starts_with('/')
        || path.split('/').any(|part| part == ".." || part.is_empty())
        || path.starts_with("src/generated/")
        || path == "src/generated";
    if bad { Err(format!("refusing to patch `{path}`")) } else { Ok(()) }
}

fn parse_range(range: &str) -> Result<(usize, usize), String> {
    let (start, len) = match range.split_once(',') {
        Some((start, len)) => (start, len),
        None => (range, "1"),
    };
    let start = start.parse().map_err(|_| format!("bad hunk range `{range}`"))?;
    let len = len.parse().map_err(|_| format!("bad hunk range `{range}`"))?;
    Ok((start, len))
}

fn parse(text: &str) -> Result<Vec<FilePatch>, String> {
    let mut files: Vec<FilePatch> = Vec::new();
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(rest) = line.strip_prefix("@@ ") {
            let file = files.last_mut().ok_or("hunk before a file header")?;
            let ranges = rest.split(" @@").next().unwrap_or_default();
            let mut parts = ranges.split(' ');
            let old = parts.next().and_then(|p| p.strip_prefix('-')).ok_or(format!("bad hunk `{line}`"))?;
            let new = parts.next().and_then(|p| p.strip_prefix('+')).ok_or(format!("bad hunk `{line}`"))?;
            let (old_start, old_len) = parse_range(old)?;
            let (_, new_len) = parse_range(new)?;
            let mut hunk = Hunk { old_start, old_len, new_len, lines: Vec::new() };
            let (mut old_seen, mut new_seen) = (0, 0);
            while old_seen < old_len || new_seen < new_len {
                let line = lines.next().ok_or_else(|| format!("{}: hunk `{line}` is cut short", file.path))?;
                let (kind, text) = match line.chars().next() {
                    Some(kind @ (' ' | '-' | '+')) => (kind, &line[1..]),
                    // Some tools drop the space of an empty context line.
                    None => (' ', ""),
                    Some(_) => return Err(format!("{}: unexpected line in hunk: {line}", file.path)),
                };
                match kind {
                    ' ' => {
                        old_seen += 1;
                        new_seen += 1;
                    }
                    '-' => old_seen += 1,
                    _ => new_seen += 1,
                }
                hunk.lines.push((kind, text.to_string()));
            }
            if old_seen != old_len || new_seen != new_len {
                return Err(format!("{}: hunk `{line}` has the wrong length", file.path));
            }
            if lines.peek().is_some_and(|next| next.starts_with('\\')) {
                return Err(format!("{}: files without a final newline aren't supported", file.path));
            }
            file.hunks.push(hunk);
        } else if line.starts_with("--- ") {
            let old = strip_prefix(line, "--- ")?;
            let new = strip_prefix(lines.next().ok_or("`---` without `+++`")?, "+++ ")?;
            if old != new {
                return Err(format!("renames and new files aren't supported: {old} -> {new}"));
            }
            check_path(&new)?;
            files.push(FilePatch { path: new, hunks: Vec::new() });
        }
        // Anything else (`diff --git`, `index`, prose) is a header line.
    }
    if files.is_empty() || files.iter().any(|f| f.hunks.is_empty()) {
        return Err("patch has no hunks".into());
    }
    Ok(files)
}

fn apply_hunks(path: &str, content: &str, hunks: &[Hunk]) -> Result<String, String> {
    if !content.is_empty() && !content.ends_with('\n') {
        return Err(format!("{path}: files without a final newline aren't supported"));
    }
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    // How far earlier hunks moved the lines after them.
    let mut delta: isize = 0;
    for hunk in hunks {
        let old_start = if hunk.old_len == 0 { hunk.old_start } else { hunk.old_start - 1 };
        let at = old_start.checked_add_signed(delta).ok_or(format!("{path}: hunk before the file"))?;
        let old: Vec<&str> = hunk.lines.iter().filter(|(k, _)| *k != '+').map(|(_, t)| t.as_str()).collect();
        let new: Vec<String> = hunk.lines.iter().filter(|(k, _)| *k != '-').map(|(_, t)| t.clone()).collect();
        let found = lines.get(at..at + old.len());
        if found.is_none_or(|found| found.iter().zip(&old).any(|(a, b)| a != b)) {
            return Err(format!("{path}: hunk at line {} doesn't match", hunk.old_start));
        }
        lines.splice(at..at + old.len(), new);
        delta += hunk.new_len as isize - hunk.old_len as isize;
    }
    let mut out = lines.join("\n");
    out.push('\n');
    Ok(out)
}

/// Applies `patch` to the crate in `root`, and returns the files it changed.
pub fn apply(root: &Path, patch: &str) -> Result<Vec<String>, String> {
    let files = parse(patch)?;
    let mut changed = Vec::new();
    for file in &files {
        let full = root.join(&file.path);
        let content = fs::read_to_string(&full).map_err(|e| format!("{}: {e}", full.display()))?;
        let patched = apply_hunks(&file.path, &content, &file.hunks)?;
        fs::write(&full, patched).map_err(|e| format!("{}: {e}", full.display()))?;
        changed.push(file.path.clone());
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATCH: &str = "\
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,4 @@
 one
+one and a half
 two
 three
@@ -5,2 +6,2 @@
 five
-six
+SIX
";

    const BEFORE: &str = "one\ntwo\nthree\nfour\nfive\nsix\n";

    #[test]
    fn applies_in_place() {
        let files = parse(PATCH).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/lib.rs");
        let after = apply_hunks("src/lib.rs", BEFORE, &files[0].hunks).unwrap();
        assert_eq!(after, "one\none and a half\ntwo\nthree\nfour\nfive\nSIX\n");
    }

    #[test]
    fn no_fuzz() {
        let files = parse(PATCH).unwrap();
        // The same lines one further down no longer match.
        let shifted = format!("zero\n{BEFORE}");
        let err = apply_hunks("src/lib.rs", &shifted, &files[0].hunks).unwrap_err();
        assert!(err.contains("doesn't match"), "{err}");
    }

    #[test]
    fn pure_insertion() {
        let patch = "--- a/x.rs\n+++ b/x.rs\n@@ -2,0 +3,1 @@\n+inserted\n";
        let files = parse(patch).unwrap();
        let after = apply_hunks("x.rs", "a\nb\nc\n", &files[0].hunks).unwrap();
        assert_eq!(after, "a\nb\ninserted\nc\n");
    }

    #[test]
    fn empty_context_lines_without_their_space() {
        // As the patches here are kept: no trailing whitespace.
        let patch = "--- a/x.rs\n+++ b/x.rs\n@@ -1,3 +1,3 @@\n a\n\n-c\n+C\n";
        let files = parse(patch).unwrap();
        assert_eq!(apply_hunks("x.rs", "a\n\nc\n", &files[0].hunks).unwrap(), "a\n\nC\n");
    }

    #[test]
    fn refuses_generated_and_outside_paths() {
        for path in ["src/generated/mod.rs", "../x.rs", "/etc/passwd", "src//x.rs"] {
            let patch = format!("--- a/{path}\n+++ b/{path}\n@@ -1 +1 @@\n-a\n+b\n");
            assert!(parse(&patch).is_err(), "{path}");
        }
    }

    #[test]
    fn rejects_short_hunks() {
        let patch = "--- a/x.rs\n+++ b/x.rs\n@@ -1,3 +1,3 @@\n a\n";
        assert!(parse(patch).is_err());
    }
}
