//! The MCP tools themselves, and the ones that read and change flows.

use super::*;

pub(super) fn tools() -> Value {
    let run_id = json!({ "run_id": { "type": "integer", "description": "From run_flow." } });
    json!([
        {
            "name": "list_repos",
            "description": "Every git repository directly inside a folder, with its branch, \
                            uncommitted changes, open pull requests, what it needs next, and \
                            the flow GitAgent would run for that.",
            "inputSchema": {
                "type": "object",
                "properties": { "folder": { "type": "string", "description": "Absolute path." } },
                "required": ["folder"],
            },
        },
        {
            "name": "list_flows",
            "description": "The flows that can run in a repository, and their steps. Steps \
                            marked gated stop for approval.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo": { "type": "string", "description": "Absolute path." } },
                "required": ["repo"],
            },
        },
        {
            "name": "run_flow",
            "description": "Start a flow and run it until it finishes or reaches a step that \
                            needs approval. That step's proposal comes back under `awaiting`: \
                            show it to the user, then call approve or reject.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "repo": { "type": "string", "description": "Absolute path." },
                    "flow": { "type": "string", "description": "Flow id from list_flows." },
                    "pr": { "type": "string", "description": "Pull request number, for a \
                                                              review flow." },
                    "trusted": { "type": "boolean", "description": "Only when the user asked \
                        for a trusted run: GitAgent approves what its Trusted run would, holds \
                        at the rest, and continues into the next flow the repository needs. \
                        Already on if the user turned on Trust all in the GitAgent window." },
                },
                "required": ["repo", "flow"],
            },
        },
        {
            "name": "approve",
            "description": "Approve the step a run is waiting on, as the user decided, and \
                            run on to the next approval or the end.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "run_id": run_id["run_id"],
                    "exclude": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Files the user unchecked, from `awaiting.files`: \
                                        left out of the commit. Only a step that lists \
                                        files (the commit) takes this.",
                    },
                },
                "required": ["run_id"],
            },
        },
        {
            "name": "reject",
            "description": "Decline the step a run is waiting on. Nothing is run, and the \
                            steps that depend on it are blocked.",
            "inputSchema": {
                "type": "object",
                "properties": run_id,
                "required": ["run_id"],
            },
        },
        {
            "name": "run_status",
            "description": "Where a run stands, without moving it.",
            "inputSchema": {
                "type": "object",
                "properties": run_id,
                "required": ["run_id"],
            },
        },
        {
            "name": "list_steps",
            "description": "Every kind of step a flow can be built from: what it does, what it \
                            reads and writes, whether it stops for approval by default, and the \
                            settings it takes.",
            "inputSchema": { "type": "object", "properties": {} },
        },
        {
            "name": "get_flow",
            "description": "One flow's full definition — its steps, how they depend on each \
                            other, their settings — in the shape save_flow takes, plus any \
                            problems that keep it from running and which repositories show it.",
            "inputSchema": {
                "type": "object",
                "properties": { "flow": { "type": "string", "description": "Flow id." } },
                "required": ["flow"],
            },
        },
        {
            "name": "save_flow",
            "description": "Create a flow, or replace one with the same id, as Setup's Save \
                            does. Take the shape from get_flow: id (names the flow to replace; \
                            a flow you made always ends up with the id its label reads as, \
                            which `saved` in the result gives, with a `note` when it \
                            is not the id you gave), label, handles, and nodes, each with id, \
                            step (a key from list_steps), deps, and optional title, config and \
                            gated. Refused, with the reasons, if the flow would not run. A \
                            script or server step always stops for approval: `gated: false` \
                            on one is refused unless it was already so. flows.toml is backed \
                            up first, and the open window picks the change up by itself.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "flow": { "type": "object", "description": "The flow definition." },
                    "repo": { "type": "string", "description": "For a new flow: show it on \
                        this repository only, as a flow made in Setup is. Leave out to show \
                        it everywhere." },
                },
                "required": ["flow"],
            },
        },
        {
            "name": "delete_flow",
            "description": "Remove a flow. flows.toml is backed up first.",
            "inputSchema": {
                "type": "object",
                "properties": { "flow": { "type": "string", "description": "Flow id." } },
                "required": ["flow"],
            },
        },
        {
            "name": "show_flow",
            "description": "Which repositories show a flow: on one repository (shown true or \
                            false), or on every repository (everywhere true).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "flow": { "type": "string", "description": "Flow id." },
                    "repo": { "type": "string", "description": "Absolute path." },
                    "shown": { "type": "boolean" },
                    "everywhere": { "type": "boolean" },
                },
                "required": ["flow"],
            },
        },
        {
            "name": "repo_insight",
            "description": "Everything GitAgent knows about one repository: branch, uncommitted \
                            files, commits ahead or behind, what it needs next and which flow \
                            answers that, every open pull request with its checks and size, \
                            the release state, each shown flow with what it would act on, and \
                            any run Claude has going there.",
            "inputSchema": {
                "type": "object",
                "properties": { "repo": { "type": "string", "description": "Absolute path." } },
                "required": ["repo"],
            },
        },
    ])
}

pub(super) fn list_steps() -> Value {
    json!(catalogue::CATALOGUE
        .iter()
        .map(|s| json!({
            "step": s.key,
            "title": s.title,
            "about": s.about,
            "uses_model": s.kind == NodeKind::Model,
            "reads": s.reads,
            "writes": s.writes,
            "stops_for_approval": s.gate_by_default,
            "settings": s.config.iter().map(|c| json!({
                "key": c.key,
                "label": c.label,
                "help": c.help,
                "required": c.required,
                "example": c.placeholder,
            })).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>())
}

/// A flow as save_flow takes it, with what keeps it from running and where
/// it is shown alongside — those two are read-only and ignored on save.
pub(super) fn flow_json(def: &FlowDef, shown: &store::RepoFlows) -> Value {
    let mut out = serde_json::to_value(def).unwrap_or_default();
    if let Some(nodes) = out.as_object_mut().and_then(|o| o.remove("node")) {
        out["nodes"] = nodes;
    }
    out["problems"] = json!(flowdef::validate(def)
        .iter()
        .map(|p| p.message())
        .collect::<Vec<_>>());
    out["shown_only_on"] = json!(shown.shown_only_on(&def.id));
    out["hidden_on"] = json!(shown
        .hidden
        .iter()
        .filter(|(_, ids)| ids.iter().any(|id| id == &def.id))
        .map(|(repo, _)| repo)
        .collect::<Vec<_>>());
    out
}

pub(super) fn save_flow(raw: &Value, repo: &str) -> Result<Value, String> {
    let mut raw = raw.clone();
    let obj = raw
        .as_object_mut()
        .ok_or("`flow` must be an object, in the shape get_flow returns")?;
    for read_only in ["problems", "shown_only_on", "hidden_on"] {
        obj.remove(read_only);
    }
    if !obj.contains_key("node") {
        if let Some(nodes) = obj.remove("nodes") {
            obj.insert("node".into(), nodes);
        }
    }
    let label = obj
        .get("label")
        .and_then(|l| l.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if label.is_empty() {
        return Err("a flow needs a `label` — the name shown on its tab".into());
    }
    let given = obj
        .get("id")
        .and_then(|i| i.as_str())
        .map(str::trim)
        .filter(|i| !i.is_empty())
        .map(String::from);
    if given.is_none() {
        obj.insert("id".into(), json!(flowdef::id_from_label(&label)));
    }
    let flow: FlowDef = serde_json::from_value(raw)
        .map_err(|e| format!("not a flow definition ({e}); see get_flow for the shape"))?;

    let mut book = FlowBook::load();
    let before = book.get(&flow.id).cloned();
    // A command the app cannot vet only runs unattended on the person's own
    // say-so, given in Setup — never on a client's.
    for node in &flow.nodes {
        let command = matches!(node.step.as_str(), "run_script" | "run_remote");
        if command && node.gated == Some(false) {
            let already = before.as_ref().is_some_and(|f| {
                f.nodes
                    .iter()
                    .any(|n| n.id == node.id && n.step == node.step && n.gated == Some(false))
            });
            if !already {
                return Err(format!(
                    "step `{}` runs a command, and GitAgent's MCP tools will not make it run \
                     without approval. Leave `gated` out, or have the user untick Needs \
                     approval on it in Setup.",
                    node.id
                ));
            }
        }
    }
    let problems = flowdef::validate(&flow);
    if !problems.is_empty() {
        return Err(format!(
            "not saved — this flow would not run:\n- {}",
            problems
                .iter()
                .map(|p| p.message())
                .collect::<Vec<_>>()
                .join("\n- ")
        ));
    }

    back_up_flows();
    let created = before.is_none();
    match book.flows.iter_mut().find(|f| f.id == flow.id) {
        Some(existing) => *existing = flow.clone(),
        None => book.flows.push(flow.clone()),
    }
    let mut shown = store::load_repo_flows();
    let mut id = flow.id.clone();
    for (old, new) in book.ids_from_labels() {
        shown.rename_flow(&old, &new);
        if old == id {
            id = new;
        }
    }
    if created && !repo.trim().is_empty() {
        shown.only_on(&id, &repo_path(repo)?);
    }
    book.save();
    store::save_repo_flows(&shown);
    let saved = book.get(&id).ok_or("saved, but could not read it back")?;
    let mut out = json!({
        "saved": id,
        "created": created,
        "flow": flow_json(saved, &shown),
    });
    if let Some(note) = id_note(given.as_deref(), &id) {
        out["note"] = json!(note);
    }
    Ok(out)
}

pub(super) fn delete_flow(id: &str) -> Result<Value, String> {
    let mut book = FlowBook::load();
    if book.get(id).is_none() {
        return Err(format!("no flow {id}; see list_flows"));
    }
    if book.flows.len() == 1 {
        return Err("this is the only flow left; GitAgent needs at least one".into());
    }
    back_up_flows();
    book.flows.retain(|f| f.id != id);
    book.save();
    let mut shown = store::load_repo_flows();
    shown.forget_flow(id);
    store::save_repo_flows(&shown);
    Ok(json!({ "deleted": id, "backup": "flows.toml.bak, in GitAgent's data folder" }))
}

pub(super) fn show_flow(
    id: &str,
    repo: &str,
    shown: Option<bool>,
    everywhere: bool,
) -> Result<Value, String> {
    if FlowBook::load().get(id).is_none() {
        return Err(format!("no flow {id}; see list_flows"));
    }
    let mut lists = store::load_repo_flows();
    if everywhere {
        lists.everywhere(id);
        for ids in lists.hidden.values_mut() {
            ids.retain(|f| f != id);
        }
        lists.hidden.retain(|_, ids| !ids.is_empty());
    } else {
        let repo = repo_path(repo)?;
        match shown {
            Some(true) => lists.show(&repo, id),
            Some(false) => lists.hide(&repo, id),
            None => {
                return Err("say `shown` true or false for the repo, or `everywhere` true".into())
            }
        }
    }
    store::save_repo_flows(&lists);
    let def = FlowBook::load();
    let saved = def
        .get(id)
        .ok_or_else(|| format!("{id} could not be read back"))?;
    Ok(flow_json(saved, &lists))
}

pub(super) fn list_flows(repo: &str) -> Result<Value, String> {
    let book = FlowBook::load();
    let hidden = store::load_repo_flows().hidden_for(repo);
    Ok(json!(book
        .runnable_for(&hidden)
        .into_iter()
        .map(|def| json!({
            "flow": def.id,
            "label": def.label,
            "steps": def.to_graph().nodes.iter().map(|n| json!({
                "id": n.id,
                "title": n.title,
                "gated": n.requires_approval,
            })).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>()))
}
