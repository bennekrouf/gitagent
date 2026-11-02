//! The step panel over the run map: when it opens on its own, when it closes
//! on its own, and Escape. Hooks, called once from `Workspace` in the same
//! order on every render.

use super::*;

/// The panel's signals and what is on screen, which every rule here reads.
#[derive(Clone, Copy)]
pub(super) struct Panel {
    pub states: Signal<States>,
    pub selected_repo: Signal<Option<String>>,
    pub selected_flow: Signal<String>,
    pub selected_pr: Signal<String>,
    pub selected_node: Signal<String>,
    pub trusted: Signal<BTreeSet<Key>>,
    pub run_view: Signal<bool>,
    /// The step whose panel is open over the run map, if any.
    pub drawer: Signal<Option<String>>,
    /// The step the panel last showed, kept after it closes so the panel can
    /// slide out with its content still in it instead of going blank first.
    pub panel_step: Signal<Option<String>>,
    /// A pull request whose diff is open in the panel instead of a step.
    pub diff_panel: Signal<Option<String>>,
    /// A step approved from its panel, kept open on its log while it runs.
    pub close_when_done: Signal<Option<(Key, String)>>,
}

impl Panel {
    /// The run on screen.
    fn shown(&self) -> Key {
        (
            self.selected_repo.read().clone().unwrap_or_default(),
            self.selected_flow.read().clone(),
            self.selected_pr.read().clone(),
        )
    }
}

/// A step approved from its panel stays open on its log until it settles:
/// closed once it is done, left open on a failure to read.
pub(super) fn use_close_when_done(p: Panel) {
    let Panel {
        states,
        mut drawer,
        mut close_when_done,
        ..
    } = p;
    use_effect(move || {
        let Some((key, step)) = close_when_done.read().clone() else {
            return;
        };
        // Closed by hand, or moved to another step or repository: the panel
        // is yours again.
        if p.shown() != key || drawer.read().as_deref() != Some(step.as_str()) {
            close_when_done.set(None);
            return;
        }
        let status = states
            .read()
            .get(&key)
            .map(|state| state.status(&step))
            .unwrap_or_default();
        match status {
            NodeStatus::Done | NodeStatus::Skipped | NodeStatus::Bypassed => {
                close_when_done.set(None);
                drawer.set(None);
            }
            NodeStatus::Failed | NodeStatus::Rejected | NodeStatus::Blocked => {
                close_when_done.set(None);
            }
            NodeStatus::Pending | NodeStatus::Running | NodeStatus::AwaitingApproval => {}
        }
    });
}

/// The panel remembers the step it shows; a step taking the panel — clicked,
/// or come to need you — takes it from a diff being read.
pub(super) fn use_panel_follows_drawer(p: Panel) {
    let Panel {
        drawer,
        mut panel_step,
        mut diff_panel,
        ..
    } = p;
    use_effect(move || {
        if let Some(step) = drawer.read().clone() {
            panel_step.set(Some(step));
            diff_panel.set(None);
        }
    });
}

/// A step that stops for you — to ask for approval, or because it failed —
/// opens its panel over the run map on its own, once each time: close it
/// while the step still waits and it stays closed. Not for a step a trusted
/// run is about to answer — the panel would only flash open — but yes for one
/// it held back for you, such as a merge the reviews were unhappy about, even
/// with Trust all on, and yes for any failure. Not away from another step that
/// needs you too, which would have the two fight for it.
pub(super) fn use_auto_open(p: Panel) {
    let Panel {
        states,
        trusted,
        run_view,
        mut selected_node,
        mut drawer,
        ..
    } = p;
    let mut auto_opened = use_signal(|| Option::<(u64, String, usize)>::None);
    use_effect(move || {
        if !*run_view.read() || p.selected_repo.read().is_none() {
            return;
        }
        let key = p.shown();
        let states = states.read();
        let Some(state) = states.get(&key) else {
            return;
        };
        let Some((step, at)) = state.newest_for_a_person() else {
            return;
        };
        let this = (state.run, step.clone(), at);
        if auto_opened.peek().as_ref() == Some(&this) {
            return;
        }
        // Read, not peeked: a trusted run that holds the step takes itself out
        // of `trusted`, and that is the moment to open.
        if answers_itself(state, &step, trusted.read().contains(&key)) {
            return;
        }
        auto_opened.set(Some(this));
        let showing_another = drawer
            .peek()
            .as_ref()
            .is_some_and(|open| open != &step && needs_a_person(state.status(open)));
        if !showing_another {
            selected_node.set(step.clone());
            drawer.set(Some(step));
        }
    });
}

/// Escape closes a step's panel over the run map. Listened for on the
/// window, not the panel, because nothing in the panel has focus after a
/// click on the map. The handler is kept on `window` and replaced on each
/// mount, so a workspace opened twice never stacks two of them.
///
/// `dialog_open` says whether a dialog is in front: it owns the key, and
/// closing the panel hidden behind it would be a change nobody could see.
pub(super) fn use_escape_closes(p: Panel, dialog_open: impl Fn() -> bool + Copy + 'static) {
    let Panel {
        mut drawer,
        mut diff_panel,
        ..
    } = p;
    use_future(move || async move {
        let mut keys = document::eval(
            "if (window._gaEscape) window.removeEventListener('keydown', window._gaEscape);\
             window._gaEscape = (e) => { if (e.key === 'Escape') dioxus.send(true); };\
             window.addEventListener('keydown', window._gaEscape);",
        );
        while keys.recv::<bool>().await.is_ok() {
            if !dialog_open() {
                drawer.set(None);
                diff_panel.set(None);
            }
        }
    });
}
