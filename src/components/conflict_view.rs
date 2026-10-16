//! A stuck merge, settled in the step's panel rather than in an editor: the
//! files git is waiting on, each clash as its two sides with Current,
//! Incoming or Both to choose, and Save to write the file and stage it once
//! nothing in it is left to decide. Renders nothing while nothing is stuck.

use dioxus::prelude::*;

use crate::services::conflicts::{self, Clash, Conflicted, Pick, Piece};
use crate::services::git::{self, InProgress};

/// Unchanged text longer than this between two clashes is folded to its
/// edges: it is context for the clash, not something to read.
const CONTEXT: usize = 3;

#[derive(Props, Clone, PartialEq)]
pub struct ConflictViewProps {
    pub repo: String,
    /// Changes whenever the step does — a fix run, a retry — so the list is
    /// read again after anything that may have settled or abandoned it.
    pub stamp: String,
}

#[component]
pub fn ConflictView(props: ConflictViewProps) -> Element {
    let mut files = use_signal(Vec::<Conflicted>::new);
    // Settled since the stop, and still able to be put back in conflict.
    let mut resolved = use_signal(Vec::<String>::new);
    let mut state = use_signal(|| None::<InProgress>);
    let mut open = use_signal(|| None::<String>);
    let mut pieces = use_signal(|| Ok::<Vec<Piece>, String>(vec![]));
    let mut picks = use_signal(Vec::<Option<Pick>>::new);
    let mut error = use_signal(String::new);
    let mut busy = use_signal(|| false);
    // Set once there was something to resolve, so emptying the list reads as
    // done rather than as the resolver vanishing.
    let mut seen = use_signal(|| false);
    let mut reload = use_signal(|| 0u32);

    let repo = props.repo.clone();
    let stamp = props.stamp.clone();
    use_effect(use_reactive!(|repo, stamp| {
        let _ = (stamp, reload());
        let repo = repo.clone();
        spawn(async move {
            let found = conflicts::list(&repo).await;
            state.set(git::in_progress(&repo).await);
            let keep = open
                .peek()
                .clone()
                .filter(|p| found.iter().any(|f| &f.path == p));
            let next = keep.or_else(|| found.first().map(|f| f.path.clone()));
            let done = conflicts::resolved(&repo).await;
            if !found.is_empty() || !done.is_empty() {
                seen.set(true);
            }
            files.set(found);
            resolved.set(done);
            if let Some(path) = &next {
                let read = conflicts::read(&repo, path).await;
                let n = read
                    .as_ref()
                    .map(|p| conflicts::clash_count(p))
                    .unwrap_or(0);
                picks.set(vec![None; n]);
                pieces.set(read);
            }
            open.set(next);
        });
    }));

    let list = files.read().clone();
    let done = resolved.read().clone();
    if list.is_empty() && done.is_empty() {
        if !seen() {
            return rsx! {};
        }
        // Finished, or abandoned: either way nothing is left to resolve.
        return rsx! {
            div { class: "conflicts conflicts-done", "Nothing is left to resolve." }
        };
    }

    let showing = open().and_then(|path| list.iter().find(|f| f.path == path).cloned());
    let path = showing.as_ref().map(|f| f.path.clone()).unwrap_or_default();
    let read = pieces.read().clone();
    let chosen = picks.read().clone();
    let left = chosen.iter().filter(|p| p.is_none()).count();
    let whole_only = read.is_err()
        || showing
            .as_ref()
            .is_some_and(|f| f.current_deleted() || f.incoming_deleted());
    let finish = state()
        .map(|s| format!(" Finish the {} below.", s.label()))
        .unwrap_or_default();
    // Everything picked so far, saved or staged, back to what git left.
    let every: Vec<String> = done
        .iter()
        .cloned()
        .chain(list.iter().map(|f| f.path.clone()))
        .collect();
    let clashes = read
        .as_ref()
        .map(|p| conflicts::clash_count(p))
        .unwrap_or(0);

    // Every action ends the same way: say what went wrong, if anything, and
    // read the list again.
    let mut act = move |job: Job| {
        busy.set(true);
        error.set(String::new());
        spawn(async move {
            if let Err(e) = job.await {
                error.set(e);
            }
            busy.set(false);
            *reload.write() += 1;
        });
    };

    let act_handle = EventHandler::new(move |job: Job| act(job));

    let rebase = state() == Some(InProgress::Rebase);
    let mut n = 0usize;

    rsx! {
        div { class: "conflicts",
            div { class: "conflicts-head",
                span { "Resolve conflicts" }
                span { class: "conflicts-count",
                    if list.is_empty() { "all resolved" }
                    else if list.len() == 1 { "1 file left" }
                    else { "{list.len()} files left" }
                }
            }
            if rebase {
                div { class: "conflicts-hint",
                    "In a rebase, Current is the branch your commits are being replayed onto, \
                     and Incoming is your own commit."
                }
            }
            div { class: "conflicts-files",
                for f in list.iter().cloned() {
                    button {
                        key: "{f.path}",
                        class: if f.path == path { "conflicts-file conflicts-file-on" } else { "conflicts-file" },
                        onclick: {
                            let repo = props.repo.clone();
                            let to = f.path.clone();
                            move |_| {
                                let repo = repo.clone();
                                let to = to.clone();
                                error.set(String::new());
                                spawn(async move {
                                    let read = conflicts::read(&repo, &to).await;
                                    let n = read.as_ref().map(|p| conflicts::clash_count(p)).unwrap_or(0);
                                    picks.set(vec![None; n]);
                                    pieces.set(read);
                                    open.set(Some(to));
                                });
                            }
                        },
                        "{f.path}"
                    }
                }
            }

            if list.is_empty() {
                div { class: "conflicts-done-line", "Every conflict is resolved.{finish}" }
            }

            if let Some(file) = showing.clone() {
            if let Some(what) = file.what() {
                div { class: "conflicts-hint", "{file.path} was {what}." }
            }

            if whole_only {
                if let Err(why) = &read {
                    div { class: "conflicts-hint", "{why}" }
                }
                div { class: "conflicts-actions",
                    button {
                        class: "btn",
                        disabled: busy(),
                        onclick: {
                            let repo = props.repo.clone();
                            let file = file.clone();
                            move |_| {
                                let (repo, file) = (repo.clone(), file.clone());
                                act(Box::pin(async move { conflicts::take_whole(&repo, &file, Pick::Current).await }))
                            }
                        },
                        if file.current_deleted() { "Delete it (current)" } else { "Keep current" }
                    }
                    button {
                        class: "btn",
                        disabled: busy(),
                        onclick: {
                            let repo = props.repo.clone();
                            let file = file.clone();
                            move |_| {
                                let (repo, file) = (repo.clone(), file.clone());
                                act(Box::pin(async move { conflicts::take_whole(&repo, &file, Pick::Incoming).await }))
                            }
                        },
                        if file.incoming_deleted() { "Delete it (incoming)" } else { "Keep incoming" }
                    }
                }
            } else if clashes == 0 {
                // Fixed already, by hand: git only needs telling.
                div { class: "conflicts-hint",
                    "No conflict markers left in {file.path}. Mark it resolved to stage it as it is."
                }
                div { class: "conflicts-actions",
                    button {
                        class: "btn btn-primary",
                        disabled: busy(),
                        onclick: {
                            let repo = props.repo.clone();
                            let file = file.clone();
                            move |_| {
                                let (repo, file) = (repo.clone(), file.clone());
                                act(Box::pin(async move { conflicts::mark_resolved(&repo, &file).await }))
                            }
                        },
                        "Mark resolved"
                    }
                    StartOver { repo: props.repo.clone(), paths: vec![path.clone()], busy: busy(), open, act: act_handle }
                }
            } else {
                div { class: "conflicts-text",
                    for (i, piece) in read.clone().unwrap_or_default().into_iter().enumerate() {
                        match piece {
                            Piece::Same(text) => rsx! {
                                pre { key: "s{i}", class: "conflict-same", "{fold(&text)}" }
                            },
                            Piece::Clash(clash) => {
                                let at = n;
                                n += 1;
                                rsx! {
                                    ClashBlock {
                                        key: "c{i}",
                                        clash,
                                        pick: chosen.get(at).copied().flatten(),
                                        on_pick: move |p: Option<Pick>| {
                                            if let Some(slot) = picks.write().get_mut(at) {
                                                *slot = p;
                                            }
                                        },
                                    }
                                }
                            }
                        }
                    }
                }
                div { class: "conflicts-actions",
                    button {
                        class: "btn btn-ghost",
                        onclick: move |_| picks.set(vec![Some(Pick::Current); clashes]),
                        "All current"
                    }
                    button {
                        class: "btn btn-ghost",
                        onclick: move |_| picks.set(vec![Some(Pick::Incoming); clashes]),
                        "All incoming"
                    }
                    button {
                        class: "btn btn-primary",
                        disabled: busy() || left == clashes,
                        title: "Writes your choices into the file. Once every conflict in it is \
                                decided, the file is staged as resolved.",
                        onclick: {
                            let repo = props.repo.clone();
                            let path = path.clone();
                            let read = read.clone();
                            let chosen = chosen.clone();
                            move |_| {
                                let Ok(p) = &read else { return };
                                let text = conflicts::resolve(p, &chosen);
                                let (repo, path) = (repo.clone(), path.clone());
                                act(Box::pin(async move {
                                    conflicts::save(&repo, &path, &text).await.map(|_| ())
                                }))
                            }
                        },
                        if left == 0 { "Save and mark resolved" } else { "Save — {left} still open" }
                    }
                    StartOver { repo: props.repo.clone(), paths: vec![path.clone()], busy: busy(), open, act: act_handle }
                }
            }
            }

            if !done.is_empty() {
                div { class: "conflicts-resolved",
                    div { class: "conflicts-sub", "Resolved — start over to pick again" }
                    for p in done.iter().cloned() {
                        div { key: "{p}", class: "conflicts-resolved-row",
                            span { class: "conflicts-resolved-path", "✓ {p}" }
                            StartOver { repo: props.repo.clone(), paths: vec![p.clone()], busy: busy(), open, act: act_handle, small: true }
                        }
                    }
                    div { class: "conflicts-actions",
                        StartOver { repo: props.repo.clone(), paths: every.clone(), busy: busy(), open, act: act_handle }
                    }
                }
            }

            if !error().is_empty() {
                div { class: "conflicts-error", "{error}" }
            }
        }
    }
}

/// One of the git actions above, run by `act`.
type Job = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>>>>;

/// Puts files back in conflict as git left them, throwing away whatever was
/// picked for them here, saved or staged — for a change of mind short of
/// abandoning the whole merge. The first of them is opened.
#[component]
fn StartOver(
    repo: String,
    paths: Vec<String>,
    busy: bool,
    open: Signal<Option<String>>,
    act: EventHandler<Job>,
    #[props(default)] small: bool,
) -> Element {
    let label = match (small, paths.len()) {
        (true, _) => "Start over",
        (false, 1) => "Start over this file",
        _ => "Start over on every file",
    };
    let title = if paths.len() == 1 {
        "Puts the conflicts in this file back as git left them, dropping what you picked."
    } else {
        "Puts every file back in conflict as git left it, dropping what you picked. \
         The merge itself carries on — to undo it entirely, abandon it below."
    };
    rsx! {
        button {
            class: if small { "clash-btn" } else { "btn btn-ghost" },
            disabled: busy,
            title,
            onclick: move |_| {
                let (repo, paths) = (repo.clone(), paths.clone());
                open.set(paths.first().cloned());
                act.call(Box::pin(async move {
                    for path in &paths {
                        conflicts::start_over(&repo, path).await?;
                    }
                    Ok(())
                }))
            },
            "{label}"
        }
    }
}

/// One clash: both sides, labelled as git labelled them, and the choice.
#[component]
fn ClashBlock(clash: Clash, pick: Option<Pick>, on_pick: EventHandler<Option<Pick>>) -> Element {
    let current = name("Current", &clash.current_label);
    let incoming = name("Incoming", &clash.incoming_label);
    rsx! {
        div { class: if pick.is_some() { "clash clash-picked" } else { "clash" },
            div { class: "clash-bar",
                if let Some(p) = pick {
                    span { class: "clash-kept",
                        match p {
                            Pick::Current => "Kept current",
                            Pick::Incoming => "Kept incoming",
                            Pick::Both => "Kept both",
                        }
                    }
                    button { class: "clash-btn", onclick: move |_| on_pick.call(None), "Undo" }
                } else {
                    button { class: "clash-btn", onclick: move |_| on_pick.call(Some(Pick::Current)), "Accept current" }
                    button { class: "clash-btn", onclick: move |_| on_pick.call(Some(Pick::Incoming)), "Accept incoming" }
                    button { class: "clash-btn", onclick: move |_| on_pick.call(Some(Pick::Both)), "Accept both" }
                }
            }
            if let Some(p) = pick {
                pre { class: "clash-side clash-result", "{shown(&clash.take(p))}" }
            } else {
                div { class: "clash-label clash-label-current", "{current}" }
                pre { class: "clash-side clash-current", "{shown(&clash.current)}" }
                if let Some(base) = &clash.base {
                    div { class: "clash-label", "Common ancestor" }
                    pre { class: "clash-side clash-base", "{shown(base)}" }
                }
                div { class: "clash-label clash-label-incoming", "{incoming}" }
                pre { class: "clash-side clash-incoming", "{shown(&clash.incoming)}" }
            }
        }
    }
}

fn name(side: &str, label: &str) -> String {
    // A conflict put back by Start over is labelled only `ours` / `theirs`,
    // which says nothing the side's own name does not.
    if label.is_empty() || label == "ours" || label == "theirs" {
        side.to_string()
    } else {
        format!("{side} — {label}")
    }
}

/// A side with nothing in it is a real choice, so it says so.
fn shown(text: &str) -> String {
    if text.is_empty() {
        "(nothing)".into()
    } else {
        text.trim_end_matches(['\n', '\r']).to_string()
    }
}

/// Unchanged text, cut to its first and last few lines when it is long.
fn fold(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= CONTEXT * 2 + 1 {
        return lines.join("\n");
    }
    format!(
        "{}\n  ⋯ {} unchanged lines ⋯\n{}",
        lines[..CONTEXT].join("\n"),
        lines.len() - CONTEXT * 2,
        lines[lines.len() - CONTEXT..].join("\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_unchanged_text_keeps_only_its_edges() {
        let text: String = (1..=10).map(|i| format!("{i}\n")).collect();
        assert_eq!(fold(&text), "1\n2\n3\n  ⋯ 4 unchanged lines ⋯\n8\n9\n10");
        assert_eq!(fold("a\nb\n"), "a\nb");
    }
}
