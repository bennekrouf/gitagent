use super::*;

/// The props an event goes to the kit with, as `name=value` pairs.
fn props(event: Event) -> (&'static str, Vec<(&'static str, Value)>) {
    match event.into_kit() {
        kit::Event::Custom { name, props } => (name, props),
        other => panic!("expected a custom event, got {other:?}"),
    }
}

#[test]
fn a_run_is_only_done_if_nothing_in_it_went_wrong() {
    use NodeStatus::*;
    assert_eq!(outcome_of(&[Done, Done, Skipped]), Outcome::Done);
    // Committed, then the push failed: that did not work.
    assert_eq!(outcome_of(&[Done, Failed, Blocked]), Outcome::Failed);
    assert_eq!(outcome_of(&[Done, Rejected, Blocked]), Outcome::Rejected);
    assert_eq!(outcome_of(&[Failed, Rejected]), Outcome::Failed);
    assert_eq!(outcome_of(&[Skipped, Bypassed]), Outcome::Nothing);
    assert_eq!(outcome_of(&[]), Outcome::Nothing);
}

#[test]
fn an_event_cannot_carry_anything_but_its_vocabulary() {
    let (name, p) = props(Event::flow_finished(
        "my-secret-client-flow",
        Outcome::Done,
        &Forge::Unsupported("git.internal.bigcorp.example".into()),
        ProviderKind::Remote,
    ));
    assert_eq!(name, "flow_finished");
    assert_eq!(
        p,
        vec![
            ("flow", Value::Tag("custom")),
            ("outcome", Value::Tag("done")),
            ("forge", Value::Tag("other")),
            ("provider", Value::Tag("remote")),
        ]
    );

    // Shipped flows keep their names.
    assert_eq!(flow_tag("commit_and_pr"), "commit_and_pr");
    assert_eq!(flow_tag("review_and_merge"), "review_and_merge");
}

#[test]
fn an_approval_says_who_decided_and_nothing_else() {
    let (name, p) = props(Event::ApprovalDecided {
        approved: false,
        trusted: true,
    });
    assert_eq!(name, "approval_decided");
    assert_eq!(
        p,
        vec![
            ("approved", Value::Flag(false)),
            ("by", Value::Tag("trusted"))
        ]
    );
}
