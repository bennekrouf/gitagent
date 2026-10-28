//! Reading what changed in the working tree, and fitting it into what a model
//! can read.

use super::*;

/// Splits `git status --porcelain -z` output into changes.
///
/// `-z` rather than the default text form, and not as a style preference.
/// The text form quotes and C-escapes any path holding a space or a
/// non-ASCII byte — `has space.rs` comes back as `"has space.rs"`, quotes
/// included, and `naïve.rs` as `"na\303\257ve.rs"`. That mangled string
/// then fails `git add` outright, and, quietly worse, stops comparing equal
/// to the real path in `needs_staging` and `staged_but_unapproved` — the
/// check that keeps work nobody approved out of a commit. `-z` emits the
/// path raw and the whole class of bug goes away.
///
/// Records are NUL-terminated `XY <path>`. A rename or a copy spends two of
/// them: the new path, then the path it came from. The new path is the one
/// we act on, so the origin record is read and dropped.
///
/// A path that is not valid UTF-8 is still lossily converted upstream in
/// `run`, and will not match anything. That is a narrower problem than this
/// one and is left alone.
pub fn parse_status_z(out: &str) -> Vec<FileChange> {
    let mut changes = vec![];
    let mut records = out.split('\0').filter(|r| !r.is_empty());
    while let Some(record) = records.next() {
        // "XY " and at least one byte of path. The first three bytes are
        // always ASCII, so slicing them cannot split a character.
        if record.len() < 4 || !record.is_char_boundary(3) {
            continue;
        }
        let code = record[..2].to_string();
        let path = record[3..].to_string();
        // Consume the origin path a rename or copy trails behind it.
        if code.contains('R') || code.contains('C') {
            records.next();
        }
        changes.push(FileChange { code, path });
    }
    changes
}

/// A number that changes when what the repository says about itself does: a
/// commit, a checkout, a push or fetch moving a ref, a merge or rebase
/// starting or ending. Read from file timestamps under `.git`, so it costs a
/// handful of `stat` calls — cheap enough to ask every couple of seconds for
/// every repository, which is how a commit made from a terminal, or by
/// GitAgent's own MCP server, reaches the window without a Refresh.
///
/// Not the index: `git status`, which every re-check runs, may rewrite it,
/// and watching it would have each re-check set off the next. Staging changes
/// nothing a re-check reports anyway.
pub fn change_fingerprint(repo: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let root = std::path::Path::new(repo);
    let dot_git = root.join(".git");
    // A worktree or submodule has a `.git` file naming the real directory.
    let git_dir = match std::fs::read_to_string(&dot_git) {
        Ok(text) => text
            .trim()
            .strip_prefix("gitdir:")
            .map(|p| root.join(p.trim()))
            .unwrap_or(dot_git),
        Err(_) => dot_git,
    };
    // Refs are shared between worktrees and live in the common directory.
    let common = std::fs::read_to_string(git_dir.join("commondir"))
        .map(|p| git_dir.join(p.trim()))
        .unwrap_or_else(|_| git_dir.clone());

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut stamp = |path: &std::path::Path| {
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        path.hash(&mut hasher);
        modified.hash(&mut hasher);
    };
    for name in [
        "HEAD",
        "MERGE_HEAD",
        "REBASE_HEAD",
        "CHERRY_PICK_HEAD",
        "FETCH_HEAD",
    ] {
        stamp(&git_dir.join(name));
    }
    stamp(&common.join("packed-refs"));
    fn walk(dir: &std::path::Path, depth: usize, stamp: &mut dyn FnMut(&std::path::Path)) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            if path.is_dir() {
                if depth > 0 {
                    walk(&path, depth - 1, stamp);
                }
            } else {
                stamp(&path);
            }
        }
    }
    walk(&common.join("refs"), 6, &mut stamp);
    hasher.finish()
}

/// How little a file's diff tells a reviewer, lowest first to go: lock and
/// generated files, then documentation, then CI and packaging, then code.
/// Among equals the largest goes first, so the most files survive.
pub(super) fn review_value(path: &str) -> u8 {
    let name = path.rsplit('/').next().unwrap_or(path);
    let generated = name.ends_with(".lock")
        || matches!(
            name,
            "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" | "go.sum"
        )
        || [".min.js", ".map", ".snap", ".svg"]
            .iter()
            .any(|ext| name.ends_with(ext));
    if generated {
        0
    } else if name.ends_with(".md") || path.starts_with("docs/") || name == "LICENSE" {
        1
    } else if [
        ".github/",
        ".gitlab/",
        ".circleci/",
        ".azuredevops/",
        ".pipelines/",
        "homebrew/",
    ]
    .iter()
    .any(|dir| path.starts_with(dir))
    {
        2
    } else {
        3
    }
}

/// Cuts a diff to `budget` bytes a whole file at a time rather than wherever
/// the byte count runs out, which used to leave the model half a file and no
/// idea the rest existed. Lock and generated files go first, then the
/// largest files, and the diff opens with a line naming what went — so the
/// model does not report on code it never saw, and the person knows too.
pub fn fit(diff: &str, budget: usize) -> Fitted {
    if diff.len() <= budget {
        return Fitted {
            text: diff.to_string(),
            left_out: vec![],
        };
    }

    let starts: Vec<usize> = diff
        .match_indices("diff --git ")
        .map(|(i, _)| i)
        .filter(|&i| i == 0 || diff.as_bytes()[i - 1] == b'\n')
        .collect();
    if starts.is_empty() {
        return Fitted {
            text: truncate(diff, budget),
            left_out: vec![],
        };
    }

    let preamble = &diff[..starts[0]];
    let files: Vec<(&str, &str)> = starts
        .iter()
        .enumerate()
        .map(|(n, &start)| {
            let end = starts.get(n + 1).copied().unwrap_or(diff.len());
            let chunk = &diff[start..end];
            let header = chunk.lines().next().unwrap_or_default();
            let path = header.rsplit(" b/").next().unwrap_or(header);
            (path, chunk)
        })
        .collect();

    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by_key(|&i| {
        (
            review_value(files[i].0),
            std::cmp::Reverse(files[i].1.len()),
        )
    });

    // Room for the line naming what was left out, which grows as files go.
    let mut kept = vec![true; files.len()];
    let mut size = diff.len();
    let mut left_out = vec![];
    for i in order {
        let note = 80 + left_out.iter().map(|p: &String| p.len() + 2).sum::<usize>();
        if size + note <= budget {
            break;
        }
        kept[i] = false;
        size -= files[i].1.len();
        left_out.push(files[i].0.to_string());
    }

    // One file too big on its own is cut rather than dropped entirely.
    if left_out.len() == files.len() {
        let (path, chunk) = files
            .iter()
            .max_by_key(|(p, c)| (review_value(p), c.len()))
            .copied()
            .unwrap_or_default();
        left_out.retain(|p| p != path);
        let note = format!("[Left out to fit: {}]\n", left_out.join(", "));
        let room = budget.saturating_sub(preamble.len() + note.len());
        return Fitted {
            text: format!("{note}{preamble}{}", truncate(chunk, room)),
            left_out,
        };
    }

    let mut text = format!("[Left out to fit: {}]\n{preamble}", left_out.join(", "));
    for (i, (_, chunk)) in files.iter().enumerate() {
        if kept[i] {
            text.push_str(chunk);
        }
    }
    Fitted { text, left_out }
}
