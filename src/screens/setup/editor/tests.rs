use super::*;
use crate::services::catalogue::CATALOGUE;
use crate::services::probe::Need;

/// What one render of the editor is given.
#[derive(Clone, PartialEq, Props)]
struct Case {
    flow: Option<FlowDef>,
    selected: String,
    adding: bool,
    identities: Vec<String>,
    /// Set Setup up as opened from a repository, with the flow not saved yet.
    new_from_repo: bool,
}

#[allow(non_snake_case)]
fn Harness(case: Case) -> Element {
    let book = use_signal(|| FlowBook {
        flows: case.flow.clone().into_iter().collect(),
        ..FlowBook::default()
    });
    let scopes = use_signal(RepoFlows::default);
    let everywhere = use_signal(BTreeSet::new);
    let flow_id = use_signal(|| case.flow.as_ref().map(|f| f.id.clone()).unwrap_or_default());
    let node_id = use_signal(|| case.selected.clone());
    let adding = use_signal(|| case.adding);
    let testing = use_signal(|| false);
    let settings_open = use_signal(|| false);
    let test_result = use_signal(|| None);
    let identities = use_signal(|| case.identities.clone());
    let saved = use_signal(Layout::default);
    let editor_w = use_signal(|| 360.0);
    let dragging = use_signal(|| 0u8);
    let drag_from = use_signal(|| (0.0, 0.0));
    let llm_config = use_signal(LlmConfig::default);
    let props = SetupProps {
        repo: None,
        folder: None,
        llm_config,
        on_close: EventHandler::new(|_| {}),
    };
    let (origin, origin_label) = if case.new_from_repo {
        (Some("/code/site".to_string()), "site".to_string())
    } else {
        (None, String::new())
    };
    let new = case.new_from_repo;
    let edit_flow = move |_: &dyn Fn(&mut FlowDef)| {};
    flow_editor(
        book,
        scopes,
        everywhere,
        flow_id,
        node_id,
        adding,
        testing,
        settings_open,
        test_result,
        identities,
        saved,
        editor_w,
        dragging,
        drag_from,
        FlowBook::defaults(),
        move |_| new,
        origin,
        origin_label,
        case.flow.clone(),
        case.selected.clone(),
        edit_flow,
        props,
    )
}

fn render(case: Case) -> String {
    let mut dom = VirtualDom::new_with_props(Harness, case);
    dom.rebuild_in_place();
    dioxus_ssr::render(&dom)
}

/// A shipped flow, with a remote step added after its last step: the one
/// step whose settings use every kind of field.
fn flow() -> FlowDef {
    let mut flow = FlowBook::defaults().flows[0].clone();
    let last = flow.nodes.last().map(|n| n.id.clone()).unwrap_or_default();
    let mut remote = NodeDef::from_catalogue("deploy", "run_remote");
    remote.deps = vec![last];
    flow.nodes.push(remote);
    flow
}

fn case(selected: &str) -> Case {
    Case {
        flow: Some(flow()),
        selected: selected.into(),
        adding: false,
        identities: vec!["/Users/me/.ssh/id_ed25519".into()],
        new_from_repo: false,
    }
}

/// How many times `needle` appears in `html`.
fn count(html: &str, needle: &str) -> usize {
    html.matches(needle).count()
}

#[test]
fn with_no_flow_there_is_nothing_to_edit() {
    let html = render(Case {
        flow: None,
        ..case("")
    });
    assert!(html.contains("No flow selected"));
    assert!(!html.contains("Delete flow"));
}

#[test]
fn a_flow_shows_its_name_scope_purpose_and_buttons() {
    let flow = flow();
    let html = render(case(""));
    assert!(html.contains(&format!("value=\"{}\"", flow.label)));
    assert!(html.contains("Duplicate") && html.contains("Delete flow"));
    assert!(html.contains("Shown on every repository."));
    assert!(html.contains("Open this flow when a repository has"));
    assert_eq!(count(&html, "type=\"checkbox\""), Need::ALL.len());
    // Nothing selected: the step column says what to do.
    assert!(html.contains("Pick a step in the diagram to edit it."));
    assert!(html.contains("Add a step"));
}

#[test]
fn a_new_flow_from_a_repository_says_where_it_will_be_shown() {
    let html = render(Case {
        new_from_repo: true,
        ..case("")
    });
    assert!(html.contains("New: will be shown only on site once saved."));
    assert!(html.contains("Show on every repository"));
}

#[test]
fn adding_a_step_lists_every_step_this_build_knows() {
    let html = render(Case {
        adding: true,
        ..case("deploy")
    });
    assert_eq!(count(&html, "class=\"cat-item\""), CATALOGUE.len());
    // The catalogue replaces the step's settings rather than adding to them.
    assert!(!html.contains("Runs after"));
}

#[test]
fn a_remote_step_shows_every_setting_with_its_key_picker_and_test() {
    let html = render(case("deploy"));
    for label in [
        "Title",
        "Subtitle",
        "Host",
        "Command",
        "Identity file",
        "Port",
    ] {
        assert!(html.contains(label), "{label} is missing");
    }
    // Required settings are starred; the identity is optional.
    assert!(html.contains("Host *") && !html.contains("Identity file *"));
    assert!(html.contains("Browse…"));
    assert!(html.contains("Use /Users/me/.ssh/id_ed25519"));
    assert!(html.contains("Test connection"));
    assert!(html.contains("Stop for approval before running"));
    assert!(html.contains("Remove"));
}

#[test]
fn without_keys_in_ssh_there_is_nothing_to_pick() {
    let html = render(Case {
        identities: vec![],
        ..case("deploy")
    });
    assert!(html.contains("Browse…"));
    assert!(!html.contains("identity-pick"));
}

#[test]
fn a_step_runs_after_any_other_except_one_that_would_make_a_loop() {
    let flow = flow();
    let first = flow.nodes[0].id.clone();
    // The first step: every later one waits on it, directly or not, so making
    // it wait on any of them would be a loop.
    let html = render(case(&first));
    assert!(html.contains("Runs after"));
    assert_eq!(count(&html, "Would create a loop"), flow.nodes.len() - 1);
    // The remote step waits on the last shipped one and could wait on any.
    let html = render(case("deploy"));
    assert_eq!(count(&html, "Would create a loop"), 0);
}
