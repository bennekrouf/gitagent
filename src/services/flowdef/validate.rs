//! Whether a flow definition can run: its problems, and the order its steps run
//! in.

use super::*;

#[derive(Clone, PartialEq, Debug)]
pub enum Problem {
    Empty,
    NoRoot,
    DuplicateId(String),
    UnknownStep {
        node: String,
        key: String,
    },
    UnknownDep {
        node: String,
        dep: String,
    },
    SelfDep(String),
    Cycle(Vec<String>),
    MissingInput {
        node: String,
        key: String,
    },
    MissingSetting {
        node: String,
        label: String,
    },
    /// A binding names a node that is not upstream of the one reading it, so
    /// there is no promise it has run — or has run *yet* — when the read
    /// happens.
    BindingNotUpstream {
        node: String,
        input: String,
        source: String,
    },
    /// A binding names an upstream node that does not write that output.
    BindingNotProduced {
        node: String,
        input: String,
        source: String,
        key: String,
    },
    /// A binding for an input the step does not have.
    BindingUnknownInput {
        node: String,
        input: String,
    },
}

impl Problem {
    pub fn message(&self) -> String {
        match self {
            Problem::Empty => "This flow has no steps.".into(),
            Problem::NoRoot => "Every step depends on another, so nothing can start. At least one \
                 step needs no dependencies."
                .into(),
            Problem::DuplicateId(id) => format!("Two steps are both called `{id}`."),
            Problem::UnknownStep { node, key } => {
                format!("`{node}` uses the step `{key}`, which does not exist.")
            }
            Problem::UnknownDep { node, dep } => {
                format!("`{node}` depends on `{dep}`, which is not in this flow.")
            }
            Problem::SelfDep(id) => format!("`{id}` depends on itself."),
            Problem::Cycle(ids) => {
                format!(
                    "These steps depend on each other in a loop: {}.",
                    ids.join(" → ")
                )
            }
            Problem::MissingInput { node, key } => {
                format!("`{node}` reads `{key}`, but nothing it depends on produces it.")
            }
            Problem::MissingSetting { node, label } => {
                format!("`{node}` needs its {label} filled in before it can run.")
            }
            Problem::BindingNotUpstream {
                node,
                input,
                source,
            } => format!(
                "`{node}` takes `{input}` from `{source}`, which it does not depend on — \
                 add the dependency, or nothing guarantees `{source}` has run first."
            ),
            Problem::BindingNotProduced {
                node,
                input,
                source,
                key,
            } => format!("`{node}` takes `{input}` from `{source}`, which does not write `{key}`."),
            Problem::BindingUnknownInput { node, input } => {
                format!("`{node}` binds `{input}`, which is not one of its inputs.")
            }
        }
    }
}

/// Everything wrong with a flow, in the order worth fixing it.
pub fn validate(flow: &FlowDef) -> Vec<Problem> {
    let mut problems = vec![];

    if flow.nodes.is_empty() {
        return vec![Problem::Empty];
    }

    let mut seen = HashSet::new();
    for node in &flow.nodes {
        if !seen.insert(node.id.clone()) {
            problems.push(Problem::DuplicateId(node.id.clone()));
        }
        if catalogue::by_key(&node.step).is_none() {
            problems.push(Problem::UnknownStep {
                node: node.id.clone(),
                key: node.step.clone(),
            });
        }
    }

    let ids: HashSet<&str> = flow.nodes.iter().map(|n| n.id.as_str()).collect();
    for node in &flow.nodes {
        for dep in &node.deps {
            if dep == &node.id {
                problems.push(Problem::SelfDep(node.id.clone()));
            } else if !ids.contains(dep.as_str()) {
                problems.push(Problem::UnknownDep {
                    node: node.id.clone(),
                    dep: dep.clone(),
                });
            }
        }
    }

    if !flow.nodes.iter().any(|n| n.deps.is_empty()) {
        problems.push(Problem::NoRoot);
    }

    // Kahn's algorithm: whatever cannot be ordered is in, or behind, a cycle.
    let order = topological_order(flow);
    if order.len() != flow.nodes.len() {
        let stuck: Vec<String> = flow
            .nodes
            .iter()
            .map(|n| n.id.clone())
            .filter(|id| !order.contains(id))
            .collect();
        if !stuck.is_empty() {
            problems.push(Problem::Cycle(stuck));
        }
    }

    // An input is satisfied only by a *transitive* dependency. A node that
    // happens to run earlier is not enough — the graph gives no such promise.
    let by_id: HashMap<&str, &NodeDef> = flow.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    for node in &flow.nodes {
        let Some(info) = catalogue::by_key(&node.step) else {
            continue;
        };
        for field in info.config {
            if field.required && node.setting(field.key).trim().is_empty() {
                problems.push(Problem::MissingSetting {
                    node: node.id.clone(),
                    label: field.label.to_lowercase(),
                });
            }
        }

        let upstream = ancestors(flow, &node.id);
        let writes_of = |id: &str| -> Vec<String> {
            by_id
                .get(id)
                .and_then(|d| catalogue::by_key(&d.step))
                .map(|i| catalogue::expand(i.writes, id))
                .unwrap_or_default()
        };

        let inputs = catalogue::expand(info.reads, &node.id);
        // A binding for something the step never reads is dead configuration
        // that will look like it is doing something. Say so.
        for input in node.bind.keys() {
            if !inputs.contains(input) {
                problems.push(Problem::BindingUnknownInput {
                    node: node.id.clone(),
                    input: input.clone(),
                });
            }
        }

        for input in &inputs {
            // A bound input answers to its named producer and to nothing else:
            // it does not matter whether some other ancestor also writes the
            // key, which is the entire point of having said which one.
            if let Some((source, key)) = node.binding(input) {
                if !upstream.contains(source) {
                    problems.push(Problem::BindingNotUpstream {
                        node: node.id.clone(),
                        input: input.clone(),
                        source: source.to_string(),
                    });
                } else if !writes_of(source).iter().any(|w| w == key) {
                    problems.push(Problem::BindingNotProduced {
                        node: node.id.clone(),
                        input: input.clone(),
                        source: source.to_string(),
                        key: key.to_string(),
                    });
                }
                continue;
            }

            if !upstream.iter().any(|id| writes_of(id).contains(input)) {
                problems.push(Problem::MissingInput {
                    node: node.id.clone(),
                    key: input.clone(),
                });
            }
        }
    }

    problems
}

/// Every node reachable by following dependencies upward. Cycle-safe.
pub fn ancestors(flow: &FlowDef, id: &str) -> BTreeSet<String> {
    let by_id: HashMap<&str, &NodeDef> = flow.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let mut found = BTreeSet::new();
    let mut queue: Vec<String> = by_id.get(id).map(|n| n.deps.clone()).unwrap_or_default();

    while let Some(next) = queue.pop() {
        if !found.insert(next.clone()) {
            continue;
        }
        if let Some(node) = by_id.get(next.as_str()) {
            queue.extend(node.deps.clone());
        }
    }
    found
}

pub fn topological_order(flow: &FlowDef) -> Vec<String> {
    let mut remaining: Vec<&NodeDef> = flow.nodes.iter().collect();
    let mut done: Vec<String> = vec![];

    loop {
        let ready: Vec<String> = remaining
            .iter()
            .filter(|n| n.deps.iter().all(|d| done.contains(d)))
            .map(|n| n.id.clone())
            .collect();
        if ready.is_empty() {
            break;
        }
        remaining.retain(|n| !ready.contains(&n.id));
        done.extend(ready);
    }
    done
}

/// Where a newly added node should attach. With a node selected, it branches
/// off that one; with nothing selected, it attaches after every step nothing
/// else depends on, so the flow keeps growing at its end.
pub fn default_deps(flow: &FlowDef, selected: &str) -> Vec<String> {
    if !selected.is_empty() && flow.nodes.iter().any(|n| n.id == selected) {
        return vec![selected.to_string()];
    }
    let depended_on: HashSet<&str> = flow
        .nodes
        .iter()
        .flat_map(|n| n.deps.iter().map(|d| d.as_str()))
        .collect();
    flow.nodes
        .iter()
        .map(|n| n.id.as_str())
        .filter(|id| !depended_on.contains(id))
        .map(|s| s.to_string())
        .collect()
}
