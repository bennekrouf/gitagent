//! Resolving a stuck merge, rebase, cherry-pick or revert from the app, the
//! way an editor offers it: each clash in a file shown as its two sides, with
//! Current, Incoming or Both to pick between, and the file staged once
//! nothing is left undecided.
//!
//! Git writes a clash into the file as
//!
//! ```text
//! <<<<<<< HEAD
//! what this side has
//! ||||||| base          (only with merge.conflictStyle = diff3 / zdiff3)
//! what both started from
//! =======
//! what the other side has
//! >>>>>>> origin/main
//! ```
//!
//! and nothing else here depends on more than that.

use std::path::Path;

use super::git;

/// One stretch of a conflicted file: either text both sides agree on, or a
/// clash between them.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    Same(String),
    Clash(Clash),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Clash {
    /// What follows `<<<<<<<` — usually `HEAD`.
    pub current_label: String,
    pub current: String,
    /// The common ancestor's text, when git was asked to write it.
    pub base: Option<String>,
    pub incoming: String,
    /// What follows `>>>>>>>` — the branch or commit coming in.
    pub incoming_label: String,
    /// The clash exactly as it sits in the file, markers and all, so an
    /// undecided one is written back untouched.
    pub raw: String,
}

/// Which side of a clash to keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Current,
    Incoming,
    /// Current, then incoming below it.
    Both,
}

impl Clash {
    pub fn take(&self, pick: Pick) -> String {
        match pick {
            Pick::Current => self.current.clone(),
            Pick::Incoming => self.incoming.clone(),
            Pick::Both => format!("{}{}", self.current, self.incoming),
        }
    }
}

/// The marker a line opens with, and the label after it. Git writes seven
/// characters then a space and a label, or the seven alone.
fn marker(line: &str, sign: char) -> Option<&str> {
    let line = line.trim_end_matches(['\n', '\r']);
    let rest = line.strip_prefix(&*sign.to_string().repeat(7))?;
    if rest.is_empty() {
        Some("")
    } else {
        rest.strip_prefix(' ')
    }
}

fn is_divider(line: &str) -> bool {
    line.trim_end_matches(['\n', '\r']) == "======="
}

/// Splits a file into what both sides agree on and the clashes between.
/// A clash that never closes is left as plain text: there is nothing safe to
/// offer for it.
pub fn parse(text: &str) -> Vec<Piece> {
    enum In {
        Same,
        Current,
        Base,
        Incoming,
    }
    let mut pieces = vec![];
    let mut same = String::new();
    let mut clash = Clash {
        current_label: String::new(),
        current: String::new(),
        base: None,
        incoming: String::new(),
        incoming_label: String::new(),
        raw: String::new(),
    };
    let mut at = In::Same;

    for line in text.split_inclusive('\n') {
        match at {
            In::Same => {
                if let Some(label) = marker(line, '<') {
                    clash = Clash {
                        current_label: label.to_string(),
                        current: String::new(),
                        base: None,
                        incoming: String::new(),
                        incoming_label: String::new(),
                        raw: line.to_string(),
                    };
                    at = In::Current;
                } else {
                    same.push_str(line);
                }
                continue;
            }
            In::Current | In::Base if is_divider(line) => at = In::Incoming,
            In::Current if marker(line, '|').is_some() => {
                clash.base = Some(String::new());
                at = In::Base;
            }
            In::Current => clash.current.push_str(line),
            In::Base => clash.base.get_or_insert_with(String::new).push_str(line),
            In::Incoming => {
                if let Some(label) = marker(line, '>') {
                    clash.raw.push_str(line);
                    clash.incoming_label = label.to_string();
                    if !same.is_empty() {
                        pieces.push(Piece::Same(std::mem::take(&mut same)));
                    }
                    pieces.push(Piece::Clash(clash.clone()));
                    at = In::Same;
                    continue;
                }
                clash.incoming.push_str(line);
            }
        }
        clash.raw.push_str(line);
    }
    if !matches!(at, In::Same) {
        same.push_str(&clash.raw);
    }
    if !same.is_empty() {
        pieces.push(Piece::Same(same));
    }
    pieces
}

/// How many clashes a parsed file has.
pub fn clash_count(pieces: &[Piece]) -> usize {
    pieces
        .iter()
        .filter(|p| matches!(p, Piece::Clash(_)))
        .count()
}

/// The file with each clash replaced by its pick. `picks` runs in clash
/// order; a clash with no pick keeps its markers.
pub fn resolve(pieces: &[Piece], picks: &[Option<Pick>]) -> String {
    let mut out = String::new();
    let mut n = 0;
    for piece in pieces {
        match piece {
            Piece::Same(text) => out.push_str(text),
            Piece::Clash(clash) => {
                match picks.get(n).copied().flatten() {
                    Some(pick) => out.push_str(&clash.take(pick)),
                    None => out.push_str(&clash.raw),
                }
                n += 1;
            }
        }
    }
    out
}

/// A file git is waiting on, and what kind of clash it is.
#[derive(Clone, Debug, PartialEq)]
pub struct Conflicted {
    pub path: String,
    /// The porcelain code: `UU`, `AA`, `DU`, `UD`, …
    pub code: String,
}

impl Conflicted {
    /// This side deleted the file.
    pub fn current_deleted(&self) -> bool {
        self.code.starts_with('D')
    }

    /// The side coming in deleted the file.
    pub fn incoming_deleted(&self) -> bool {
        self.code.ends_with('D')
    }

    /// What happened to the file, in words, when it is not a plain edit on
    /// both sides.
    pub fn what(&self) -> Option<&'static str> {
        match self.code.as_str() {
            "DU" => Some("deleted on the current side, changed on the incoming one"),
            "UD" => Some("changed on the current side, deleted on the incoming one"),
            "DD" => Some("deleted on both sides"),
            "AA" => Some("added on both sides"),
            "AU" | "UA" => Some("added on one side, changed on the other"),
            _ => None,
        }
    }
}

/// Every file git is still waiting on.
pub async fn list(repo: &str) -> Vec<Conflicted> {
    let changes = git::status(repo).await.unwrap_or_default();
    let stuck = git::conflicted(&changes);
    changes
        .into_iter()
        .filter(|c| stuck.contains(&c.path))
        .map(|c| Conflicted {
            path: c.path,
            code: c.code,
        })
        .collect()
}

/// The file as it sits in the working tree, split into pieces. `Err` when it
/// is gone or not text, which leaves only the whole-file choices.
pub async fn read(repo: &str, path: &str) -> Result<Vec<Piece>, String> {
    let bytes = tokio::fs::read(Path::new(repo).join(path))
        .await
        .map_err(|e| format!("could not read {path}: {e}"))?;
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("{path} is not text, so it can only be taken whole"))?;
    Ok(parse(&text))
}

/// Writes the file back, and stages it when nothing in it is left to decide.
/// `Ok(true)` when it was staged.
pub async fn save(repo: &str, path: &str, text: &str) -> Result<bool, String> {
    tokio::fs::write(Path::new(repo).join(path), text)
        .await
        .map_err(|e| format!("could not write {path}: {e}"))?;
    if clash_count(&parse(text)) > 0 {
        return Ok(false);
    }
    git::run(repo, "git", &["add", "--", path]).await?;
    Ok(true)
}

/// Settles a file by taking one side of it whole — the only choice for a
/// file one side deleted, or one that is not text. Taking a side that
/// deleted the file deletes it.
pub async fn take_whole(repo: &str, file: &Conflicted, pick: Pick) -> Result<(), String> {
    let (deleted, flag) = match pick {
        Pick::Current => (file.current_deleted(), "--ours"),
        Pick::Incoming => (file.incoming_deleted(), "--theirs"),
        Pick::Both => return Err("a whole file can only be taken from one side".into()),
    };
    if deleted {
        git::run(repo, "git", &["rm", "--quiet", "--", &file.path]).await?;
    } else {
        git::run(repo, "git", &["checkout", flag, "--", &file.path]).await?;
        git::run(repo, "git", &["add", "--", &file.path]).await?;
    }
    Ok(())
}

/// Stages a file as it is now — for one fixed by hand in an editor. Refused
/// while it still has a clash in it, so a marker is never committed.
pub async fn mark_resolved(repo: &str, file: &Conflicted) -> Result<(), String> {
    let on_disk = Path::new(repo).join(&file.path);
    if !on_disk.exists() {
        git::run(repo, "git", &["rm", "--quiet", "--", &file.path]).await?;
        return Ok(());
    }
    if let Ok(text) = tokio::fs::read_to_string(&on_disk).await {
        let left = clash_count(&parse(&text));
        if left > 0 {
            return Err(format!(
                "{} still has {left} clash{} marked in it.",
                file.path,
                if left == 1 { "" } else { "es" }
            ));
        }
    }
    git::run(repo, "git", &["add", "--", &file.path]).await?;
    Ok(())
}

/// Files resolved since this merge (rebase, …) stopped, that git can still
/// put back in conflict. Git keeps what each side had when a file is staged,
/// and keeps it past the commit, so only what matches `HEAD` now counts:
/// anything else is left over from an earlier stop.
pub async fn resolved(repo: &str) -> Vec<String> {
    if git::in_progress(repo).await.is_none() {
        return vec![];
    }
    let Ok(out) = git::run(repo, "git", &["ls-files", "--resolve-undo", "-z"]).await else {
        return vec![];
    };
    // `mode sha stage\tpath`, one entry per side git kept.
    let mut kept: Vec<(String, Option<String>)> = vec![];
    for entry in out.split('\0').filter(|e| !e.is_empty()) {
        let Some((meta, path)) = entry.split_once('\t') else {
            continue;
        };
        let mut meta = meta.split_whitespace();
        let (sha, stage) = (meta.nth(1), meta.next());
        let at = match kept.iter().position(|(p, _)| p == path) {
            Some(at) => at,
            None => {
                kept.push((path.to_string(), None));
                kept.len() - 1
            }
        };
        if stage == Some("2") {
            kept[at].1 = sha.map(str::to_string);
        }
    }
    let stuck = git::unmerged_paths(repo).await;
    let mut found = vec![];
    for (path, current) in kept {
        if stuck.contains(&path) {
            continue;
        }
        let head = git::run(
            repo,
            "git",
            &["rev-parse", "--verify", "--quiet", &format!("HEAD:{path}")],
        )
        .await
        .ok()
        .map(|s| s.trim().to_string());
        if head == current {
            found.push(path);
        }
    }
    found
}

/// Puts a file back as git first left it: conflict markers in place for
/// both sides to be picked again, and no longer staged. Throws away what
/// was chosen for it, saved or not.
pub async fn start_over(repo: &str, path: &str) -> Result<(), String> {
    // Brings back the sides git kept when the file was staged. A file still
    // in conflict has nothing to bring back, and is left as it is.
    git::run(repo, "git", &["update-index", "--unresolve", "--", path]).await?;
    let stages = git::run(repo, "git", &["ls-files", "--unmerged", "--", path]).await?;
    let has = |n: &str| {
        stages
            .lines()
            .any(|l| l.split('\t').next().is_some_and(|m| m.ends_with(n)))
    };
    // Markers need both sides. With one side deleted, the file comes back as
    // the side that kept it — which is what the merge itself left.
    let args: &[&str] = match (has(" 2"), has(" 3")) {
        (true, true) => &["checkout", "--merge", "--", path],
        (true, false) => &["checkout", "--ours", "--", path],
        (false, true) => &["checkout", "--theirs", "--", path],
        (false, false) => {
            return Err(format!(
                "git kept nothing of {path} to start over from — abandon the whole \
                 operation instead to get every file back as it was."
            ))
        }
    };
    git::run(repo, "git", args).await?;
    // With rerere on, the choice just undone was recorded, and the next time
    // the same conflict came up git would quietly make it again.
    let _ = git::run(repo, "git", &["rerere", "forget", "--", path]).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: &str = "top\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> origin/main\nbottom\n";

    fn clash(pieces: &[Piece], n: usize) -> &Clash {
        pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Clash(c) => Some(c),
                _ => None,
            })
            .nth(n)
            .unwrap()
    }

    #[test]
    fn a_clash_splits_into_its_two_sides_and_labels() {
        let pieces = parse(FILE);
        assert_eq!(pieces.len(), 3);
        assert_eq!(pieces[0], Piece::Same("top\n".into()));
        let c = clash(&pieces, 0);
        assert_eq!(c.current_label, "HEAD");
        assert_eq!(c.current, "mine\n");
        assert_eq!(c.incoming, "theirs\n");
        assert_eq!(c.incoming_label, "origin/main");
        assert_eq!(c.base, None);
        assert_eq!(pieces[2], Piece::Same("bottom\n".into()));
    }

    #[test]
    fn each_pick_writes_the_file_it_names() {
        let pieces = parse(FILE);
        let with = |p| resolve(&pieces, &[Some(p)]);
        assert_eq!(with(Pick::Current), "top\nmine\nbottom\n");
        assert_eq!(with(Pick::Incoming), "top\ntheirs\nbottom\n");
        assert_eq!(with(Pick::Both), "top\nmine\ntheirs\nbottom\n");
    }

    #[test]
    fn an_undecided_clash_is_written_back_exactly() {
        let pieces = parse(FILE);
        assert_eq!(resolve(&pieces, &[None]), FILE);
        assert_eq!(resolve(&pieces, &[]), FILE);
    }

    #[test]
    fn the_base_of_a_diff3_clash_is_kept_apart_from_both_sides() {
        let text = "<<<<<<< HEAD\na\n||||||| base\no\n=======\nb\n>>>>>>> topic\n";
        let pieces = parse(text);
        let c = clash(&pieces, 0);
        assert_eq!(c.current, "a\n");
        assert_eq!(c.base.as_deref(), Some("o\n"));
        assert_eq!(c.incoming, "b\n");
        assert_eq!(resolve(&pieces, &[None]), text);
        assert_eq!(resolve(&pieces, &[Some(Pick::Incoming)]), "b\n");
    }

    #[test]
    fn clashes_are_picked_independently_and_crlf_survives() {
        let text = "<<<<<<< HEAD\r\n1\r\n=======\r\n2\r\n>>>>>>> x\r\nmid\r\n\
                    <<<<<<< HEAD\r\n3\r\n=======\r\n4\r\n>>>>>>> x\r\n";
        let pieces = parse(text);
        assert_eq!(clash_count(&pieces), 2);
        assert_eq!(
            resolve(&pieces, &[Some(Pick::Incoming), Some(Pick::Current)]),
            "2\r\nmid\r\n3\r\n"
        );
        let half = resolve(&pieces, &[Some(Pick::Both), None]);
        assert!(half.starts_with("1\r\n2\r\nmid\r\n<<<<<<< HEAD\r\n"));
    }

    #[test]
    fn an_unclosed_clash_or_a_lookalike_is_left_as_text() {
        let text = "a\n<<<<<<< HEAD\nb\n=======\nc\n";
        assert_eq!(parse(text), vec![Piece::Same(text.into())]);
        // A heading underline in a README is not a divider outside a clash.
        let text = "Title\n=======\n<<<<<<<< eight\n";
        assert_eq!(parse(text), vec![Piece::Same(text.into())]);
    }

    #[test]
    fn an_empty_side_is_a_real_choice() {
        let text = "<<<<<<< HEAD\n=======\nadded\n>>>>>>> x\n";
        let pieces = parse(text);
        assert_eq!(resolve(&pieces, &[Some(Pick::Current)]), "");
        assert_eq!(resolve(&pieces, &[Some(Pick::Both)]), "added\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolving_every_clash_stages_the_file_and_lets_the_merge_finish() {
        let root = std::env::temp_dir().join(format!("gitagent-conflicts-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let sh = |cmd: &str| {
            std::process::Command::new("sh")
                .args(["-c", cmd])
                .current_dir(&root)
                .output()
                .unwrap()
        };
        sh(
            "git init -q -b main . && git config user.name t && git config user.email t@t && \
            printf 'a\\nb\\n' > f && echo keep > gone && git add . && git commit -qm init && \
            git checkout -qb topic && printf 'a\\ntopic\\n' > f && echo edit > gone && \
            git commit -qam topic && git checkout -q main && printf 'a\\nmain\\n' > f && \
            git rm -q gone && git commit -qam main",
        );
        assert!(!sh("git merge topic").status.success(), "the merge clashes");
        let repo = root.to_str().unwrap();

        let stuck = list(repo).await;
        assert_eq!(stuck.len(), 2, "{stuck:?}");
        let f = stuck.iter().find(|c| c.path == "f").unwrap();
        let gone = stuck.iter().find(|c| c.path == "gone").unwrap();
        assert_eq!(gone.code, "DU");

        // Half decided: written, but not staged.
        let pieces = read(repo, "f").await.unwrap();
        assert_eq!(clash_count(&pieces), 1);
        assert!(!save(repo, "f", &resolve(&pieces, &[None])).await.unwrap());
        assert!(mark_resolved(repo, f).await.is_err(), "markers are refused");

        let text = resolve(&pieces, &[Some(Pick::Both)]);
        assert!(save(repo, "f", &text).await.unwrap());
        assert_eq!(
            std::fs::read_to_string(root.join("f")).unwrap(),
            "a\nmain\ntopic\n"
        );

        // Taking the side that deleted it deletes it.
        take_whole(repo, gone, Pick::Current).await.unwrap();
        assert!(!root.join("gone").exists());

        assert!(list(repo).await.is_empty());

        // Changed your mind: both come back exactly as the merge left them.
        let mut done = resolved(repo).await;
        done.sort();
        assert_eq!(done, ["f", "gone"]);
        for path in &done {
            start_over(repo, path).await.unwrap();
        }
        assert_eq!(list(repo).await.len(), 2);
        assert!(resolved(repo).await.is_empty());
        assert_eq!(clash_count(&read(repo, "f").await.unwrap()), 1);
        assert_eq!(
            std::fs::read_to_string(root.join("gone")).unwrap(),
            "edit\n"
        );
        // A half-saved file starts over too.
        let pieces = read(repo, "f").await.unwrap();
        save(repo, "f", &resolve(&pieces, &[Some(Pick::Current)]))
            .await
            .unwrap();
        start_over(repo, "f").await.unwrap();
        assert_eq!(clash_count(&read(repo, "f").await.unwrap()), 1);

        for f in list(repo).await {
            take_whole(repo, &f, Pick::Incoming).await.unwrap();
        }
        assert!(sh("git commit --no-edit -q").status.success());
        // What git still keeps after the commit is not offered again.
        assert!(resolved(repo).await.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }
}
