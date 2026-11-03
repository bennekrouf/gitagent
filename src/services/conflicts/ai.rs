//! Asking the model to settle a file's conflicts: each clash sent with the
//! lines around it, and a merged text back for each, with a sentence on why.
//!
//! What comes back is a proposal, never a write. It lands in the resolver as
//! a pick like Current or Incoming, shown against nothing else, and the file
//! is only touched when you save it — so a model that misread a clash costs
//! an Undo, not a broken merge.

use serde_json::{json, Value};

use super::Piece;
use crate::services::llm::{complete_json, Asker, LlmConfig};

/// Lines of agreed text sent on either side of a clash, at most: enough for
/// the function or list it sits in, and cut down when the file is too large
/// for the model's window.
const AROUND: [usize; 3] = [40, 10, 0];

/// What the model wrote in place of one clash.
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub text: String,
    /// One sentence on what was kept and why, shown above the text.
    pub why: String,
    /// False when the model said the right answer depends on intent it
    /// cannot see — shown as one to check rather than one to trust.
    pub sure: bool,
}

const SYSTEM: &str = "You resolve git conflicts for a developer, who reviews \
    every answer before it is saved.\n\
    For each conflict, write the exact text that replaces the whole block, \
    markers removed. Rules:\n\
    - Keep what each side meant. When both sides added different things \
      (imports, list entries, changelog bullets, match arms, tests), keep both, \
      in a sensible order, without duplicates.\n\
    - When one side is a newer or fuller version of the same change, keep that \
      one alone rather than both.\n\
    - When the common ancestor is given, use it to tell which side changed what.\n\
    - Match the surrounding indentation, style and line endings. Never add \
      code, comments or text that neither side has, beyond what joining them \
      needs.\n\
    - Never leave <<<<<<<, ======= or >>>>>>> lines in an answer.\n\
    - `why` is one short sentence for the developer: what you kept and why.\n\
    - `sure` is false when the right answer depends on intent you cannot see \
      from the two sides; answer anyway, with your best merge.";

/// The model's proposal for every clash in `pieces`, in clash order. A clash
/// it skipped, or answered with markers still in, is `None`: left for you.
///
/// `kind` is what is in progress — merge, rebase — which decides what Current
/// and Incoming mean.
pub async fn propose(
    cfg: &LlmConfig,
    path: &str,
    kind: &str,
    pieces: &[Piece],
    asker: &mut Asker<'_>,
) -> Result<Vec<Option<Proposal>>, String> {
    if !cfg.uses_model() {
        return Err(
            "No model is set up, so conflicts can only be resolved by hand. Pick a \
             provider in Settings to have them proposed for you."
                .into(),
        );
    }
    let budget = cfg.input_budget();
    let user = AROUND
        .iter()
        .map(|&around| prompt(path, kind, pieces, around))
        .find(|p| p.len() <= budget)
        .ok_or_else(|| {
            format!(
                "The conflicts in {path} are too large for the model to read at once. \
                 Resolve them by hand, or pick a model with a larger window in Settings."
            )
        })?;
    let value = complete_json(cfg, SYSTEM, &user, &schema(), asker).await?;
    Ok(read_reply(&value, pieces))
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "conflicts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "number": { "type": "integer" },
                        "resolution": { "type": "string" },
                        "why": { "type": "string" },
                        "sure": { "type": "boolean" }
                    },
                    "required": ["number", "resolution", "why", "sure"]
                }
            }
        },
        "required": ["conflicts"]
    })
}

/// The question: the file's name, what each side is, and every clash with up
/// to `around` lines of agreed text on either side.
fn prompt(path: &str, kind: &str, pieces: &[Piece], around: usize) -> String {
    let sides = if kind == "rebase" {
        "In a rebase, CURRENT is the branch being rebased onto, and INCOMING is \
         the developer's own commit being replayed."
    } else {
        "CURRENT is the branch checked out, and INCOMING is what is being merged \
         into it."
    };
    let total = super::clash_count(pieces);
    let mut out = format!(
        "File: {path}\nA {kind} stopped on {total} conflict(s) in it. {sides}\n\
         Answer each conflict by its number.\n"
    );
    let mut n = 0;
    for (i, piece) in pieces.iter().enumerate() {
        let Piece::Clash(clash) = piece else {
            continue;
        };
        n += 1;
        out.push_str(&format!("\n--- Conflict {n} of {total} ---\n"));
        if let Some(Piece::Same(before)) = i.checked_sub(1).and_then(|j| pieces.get(j)) {
            let lines: Vec<&str> = before.lines().collect();
            let tail = &lines[lines.len().saturating_sub(around)..];
            if !tail.is_empty() {
                out.push_str(&format!("Just before it:\n{}\n", tail.join("\n")));
            }
        }
        out.push_str(&format!(
            "<<<<<<< CURRENT ({})\n{}",
            label(&clash.current_label),
            clash.current
        ));
        if let Some(base) = &clash.base {
            out.push_str(&format!("||||||| COMMON ANCESTOR\n{base}"));
        }
        out.push_str(&format!(
            "=======\n{}>>>>>>> INCOMING ({})\n",
            clash.incoming,
            label(&clash.incoming_label)
        ));
        if let Some(Piece::Same(after)) = pieces.get(i + 1) {
            let head: Vec<&str> = after.lines().take(around).collect();
            if !head.is_empty() {
                out.push_str(&format!("Just after it:\n{}\n", head.join("\n")));
            }
        }
    }
    out
}

fn label(label: &str) -> &str {
    if label.is_empty() {
        "unnamed"
    } else {
        label
    }
}

/// Matches the reply to the clashes by number. Anything off — a number out of
/// range, a resolution with markers left in — is dropped rather than shown.
fn read_reply(value: &Value, pieces: &[Piece]) -> Vec<Option<Proposal>> {
    let clashes: Vec<&super::Clash> = pieces
        .iter()
        .filter_map(|p| match p {
            Piece::Clash(c) => Some(c),
            Piece::Same(_) => None,
        })
        .collect();
    let mut out = vec![None; clashes.len()];
    for item in value["conflicts"].as_array().into_iter().flatten() {
        let Some(at) = item["number"]
            .as_u64()
            .and_then(|n| (n as usize).checked_sub(1))
            .filter(|&at| at < clashes.len())
        else {
            continue;
        };
        let Some(text) = item["resolution"].as_str() else {
            continue;
        };
        let marked = |l: &str| {
            l.starts_with("<<<<<<<") || l.starts_with(">>>>>>>") || l.trim_end() == "======="
        };
        if text.lines().any(marked) {
            continue;
        }
        let clash = clashes[at];
        // Models drop the final newline often; the sides it stands in for
        // had one, so the line after the clash would be glued onto it.
        let mut text = text.to_string();
        let sides_end_lines = clash.current.ends_with('\n') || clash.incoming.ends_with('\n');
        if !text.is_empty() && !text.ends_with('\n') && sides_end_lines {
            text.push('\n');
        }
        out[at] = Some(Proposal {
            text,
            why: item["why"].as_str().unwrap_or_default().trim().to_string(),
            sure: item["sure"].as_bool().unwrap_or(false),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::{parse, resolve, Pick};
    use super::*;

    const FILE: &str = "\
fn a() {}
<<<<<<< HEAD
use x;
||||||| base
=======
use y;
>>>>>>> origin/main
fn b() {}
<<<<<<< HEAD
one
=======
two
>>>>>>> origin/main
";

    #[test]
    fn the_prompt_numbers_each_conflict_with_its_context_and_ancestor() {
        let p = prompt("src/lib.rs", "merge", &parse(FILE), 40);
        assert!(p.contains("File: src/lib.rs"));
        assert!(p.contains("2 conflict(s)"));
        assert!(p.contains("--- Conflict 1 of 2 ---\nJust before it:\nfn a() {}\n"));
        assert!(p.contains("<<<<<<< CURRENT (HEAD)\nuse x;\n||||||| COMMON ANCESTOR\n"));
        assert!(p.contains("=======\nuse y;\n>>>>>>> INCOMING (origin/main)\n"));
        assert!(p.contains("--- Conflict 2 of 2 ---"));
        assert!(p.contains("is being merged"));
        // With no room for context, the clashes still go.
        let bare = prompt("src/lib.rs", "rebase", &parse(FILE), 0);
        assert!(!bare.contains("Just before it"));
        assert!(bare.contains("developer's own commit"));
    }

    #[test]
    fn a_reply_becomes_picks_that_resolve_the_file() {
        let pieces = parse(FILE);
        let reply = json!({ "conflicts": [
            { "number": 1, "resolution": "use x;\nuse y;", "why": "Both imports are used.", "sure": true },
            { "number": 2, "resolution": "two\n", "why": "", "sure": false },
        ]});
        let got = read_reply(&reply, &pieces);
        let first = got[0].clone().unwrap();
        // The dropped final newline is put back.
        assert_eq!(first.text, "use x;\nuse y;\n");
        assert!(first.sure);
        assert!(!got[1].as_ref().unwrap().sure);
        let picks: Vec<Option<Pick>> = got.into_iter().map(|p| p.map(Pick::Proposed)).collect();
        assert_eq!(
            resolve(&pieces, &picks),
            "fn a() {}\nuse x;\nuse y;\nfn b() {}\ntwo\n"
        );
    }

    #[test]
    fn answers_with_markers_or_wrong_numbers_are_left_for_you() {
        let pieces = parse(FILE);
        let reply = json!({ "conflicts": [
            { "number": 1, "resolution": "<<<<<<< HEAD\nuse x;\n=======\nuse y;\n>>>>>>> m\n", "why": "", "sure": true },
            { "number": 7, "resolution": "x\n", "why": "", "sure": true },
            { "number": 0, "resolution": "x\n", "why": "", "sure": true },
        ]});
        assert_eq!(read_reply(&reply, &pieces), vec![None, None]);
        assert_eq!(read_reply(&json!({}), &pieces), vec![None, None]);
    }

    #[test]
    fn an_empty_answer_is_a_real_one() {
        let pieces = parse(FILE);
        let reply = json!({ "conflicts": [
            { "number": 2, "resolution": "", "why": "Both removed it.", "sure": true },
        ]});
        let got = read_reply(&reply, &pieces);
        assert_eq!(got[0], None);
        assert_eq!(got[1].as_ref().unwrap().text, "");
    }
}
