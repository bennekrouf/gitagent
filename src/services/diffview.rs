//! Parsing a unified diff into files, hunks and lines.
//!
//! Purely data: no rendering, no syntax highlighting. `components::diff_view`
//! turns this into HTML — this module only has to agree with `git diff`'s
//! output format, which makes it cheap to test on its own.

#[derive(Clone, PartialEq, Debug)]
pub enum LineKind {
    Add,
    Remove,
    Context,
}

#[derive(Clone, PartialEq, Debug)]
pub struct DiffLine {
    pub kind: LineKind,
    /// The line's text, with the leading `+`/`-`/` ` marker already stripped.
    pub text: String,
}

#[derive(Clone, PartialEq, Debug)]
pub struct Hunk {
    /// The `@@ -a,b +c,d @@` line, kept as-is — it is a useful location
    /// marker on its own and not worth re-deriving.
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct FileDiff {
    pub old_path: String,
    pub new_path: String,
    pub hunks: Vec<Hunk>,
    /// `git diff` says "Binary files a/x and b/x differ" instead of hunks —
    /// nothing to highlight, so callers show a plain notice.
    pub is_binary: bool,
}

impl FileDiff {
    /// The path worth showing and worth guessing a language from: the new
    /// path for anything but a delete, where only the old one still exists.
    pub fn display_path(&self) -> &str {
        if self.new_path != "/dev/null" && !self.new_path.is_empty() {
            &self.new_path
        } else {
            &self.old_path
        }
    }
}

/// Splits a `git diff --unified=N` (or `git show`) style diff into files.
/// A line this does not recognise is dropped rather than mis-filed — losing
/// a stray line is better than corrupting a hunk with it.
pub fn parse(diff: &str) -> Vec<FileDiff> {
    let mut files = Vec::new();
    let mut current: Option<FileDiff> = None;
    let mut hunk: Option<Hunk> = None;

    let flush_hunk = |file: &mut Option<FileDiff>, hunk: &mut Option<Hunk>| {
        if let (Some(f), Some(h)) = (file.as_mut(), hunk.take()) {
            f.hunks.push(h);
        }
    };
    let flush_file =
        |files: &mut Vec<FileDiff>, file: &mut Option<FileDiff>, hunk: &mut Option<Hunk>| {
            if let (Some(f), Some(h)) = (file.as_mut(), hunk.take()) {
                f.hunks.push(h);
            }
            if let Some(f) = file.take() {
                files.push(f);
            }
        };

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush_file(&mut files, &mut current, &mut hunk);
            let (a, b) = split_git_header_paths(rest);
            current = Some(FileDiff {
                old_path: a,
                new_path: b,
                hunks: vec![],
                is_binary: false,
            });
        } else if line.starts_with("Binary files ") && line.ends_with(" differ") {
            if let Some(f) = current.as_mut() {
                f.is_binary = true;
            }
        } else if let Some(path) = line.strip_prefix("--- ") {
            if let Some(f) = current.as_mut() {
                f.old_path = strip_ab_prefix(path);
            }
        } else if let Some(path) = line.strip_prefix("+++ ") {
            if let Some(f) = current.as_mut() {
                f.new_path = strip_ab_prefix(path);
            }
        } else if line.starts_with("@@ ") {
            flush_hunk(&mut current, &mut hunk);
            hunk = Some(Hunk {
                header: line.to_string(),
                lines: vec![],
            });
        } else if let Some(h) = hunk.as_mut() {
            if let Some(text) = line.strip_prefix('+') {
                h.lines.push(DiffLine {
                    kind: LineKind::Add,
                    text: text.to_string(),
                });
            } else if let Some(text) = line.strip_prefix('-') {
                h.lines.push(DiffLine {
                    kind: LineKind::Remove,
                    text: text.to_string(),
                });
            } else if let Some(text) = line.strip_prefix(' ') {
                h.lines.push(DiffLine {
                    kind: LineKind::Context,
                    text: text.to_string(),
                });
            }
            // Any other line inside a hunk (e.g. "\ No newline at end of
            // file") is metadata, not a line of the file — skipped.
        }
        // Lines outside a file/hunk (index, mode changes, ---/+++ we already
        // handled) carry nothing `DiffLine` needs.
    }
    flush_file(&mut files, &mut current, &mut hunk);
    files
}

/// `diff --git a/src/lib.rs b/src/lib.rs` → `("a/src/lib.rs", "b/src/lib.rs")`
/// with the `a/`/`b/` prefixes stripped. A path containing a space defeats
/// this — git itself falls back to quoting in that case, which callers of
/// this parser have not needed to handle yet.
fn split_git_header_paths(rest: &str) -> (String, String) {
    match rest.split_once(" b/") {
        Some((a, b)) => (a.strip_prefix("a/").unwrap_or(a).to_string(), b.to_string()),
        None => (rest.to_string(), rest.to_string()),
    }
}

fn strip_ab_prefix(path: &str) -> String {
    // `--- a/x`, `+++ b/x`, or `--- /dev/null` for an added/removed file.
    let path = path.split('\t').next().unwrap_or(path);
    path.strip_prefix("a/")
        .or_else(|| path.strip_prefix("b/"))
        .unwrap_or(path)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> &'static str {
        "diff --git a/src/lib.rs b/src/lib.rs\n\
         index 1234567..89abcde 100644\n\
         --- a/src/lib.rs\n\
         +++ b/src/lib.rs\n\
         @@ -1,3 +1,4 @@\n\
         \u{20}fn main() {\n\
         -    old();\n\
         +    new();\n\
         +    another();\n\
         \u{20}}\n"
    }

    #[test]
    fn a_single_file_diff_parses_to_one_file_with_one_hunk() {
        let files = parse(sample());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].old_path, "src/lib.rs");
        assert_eq!(files[0].new_path, "src/lib.rs");
        assert_eq!(files[0].hunks.len(), 1);
    }

    #[test]
    fn lines_are_classified_by_their_leading_marker() {
        let files = parse(sample());
        let lines = &files[0].hunks[0].lines;
        assert_eq!(lines[0].kind, LineKind::Context);
        assert_eq!(lines[1].kind, LineKind::Remove);
        assert_eq!(lines[1].text, "    old();");
        assert_eq!(lines[2].kind, LineKind::Add);
        assert_eq!(lines[2].text, "    new();");
    }

    #[test]
    fn multiple_files_are_kept_separate() {
        let two = format!("{}{}", sample(), sample().replace("lib.rs", "main.rs"));
        let files = parse(&two);
        assert_eq!(files.len(), 2);
        assert_eq!(files[1].new_path, "src/main.rs");
    }

    #[test]
    fn multiple_hunks_in_one_file_are_kept_separate() {
        let diff = "diff --git a/x b/x\n\
                    --- a/x\n\
                    +++ b/x\n\
                    @@ -1,2 +1,2 @@\n\
                    -one\n\
                    +ONE\n\
                    @@ -10,2 +10,2 @@\n\
                    -ten\n\
                    +TEN\n";
        let files = parse(diff);
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[0].hunks[1].header, "@@ -10,2 +10,2 @@");
    }

    #[test]
    fn a_binary_file_carries_no_hunks() {
        let diff = "diff --git a/logo.png b/logo.png\n\
                    index 111..222 100644\n\
                    Binary files a/logo.png and b/logo.png differ\n";
        let files = parse(diff);
        assert_eq!(files.len(), 1);
        assert!(files[0].is_binary);
        assert!(files[0].hunks.is_empty());
    }

    #[test]
    fn a_new_file_has_dev_null_as_its_old_path() {
        let diff = "diff --git a/new.rs b/new.rs\n\
                    new file mode 100644\n\
                    --- /dev/null\n\
                    +++ b/new.rs\n\
                    @@ -0,0 +1,1 @@\n\
                    +hello\n";
        let files = parse(diff);
        assert_eq!(files[0].old_path, "/dev/null");
        assert_eq!(files[0].display_path(), "new.rs");
    }

    #[test]
    fn an_empty_diff_yields_no_files() {
        assert_eq!(parse(""), vec![]);
        assert_eq!(parse("   \n  \n"), vec![]);
    }
}
