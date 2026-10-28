use super::*;
use crate::services::llm::ProviderKind;

fn cfg(main: ProviderKind, second: ProviderKind, lens: bool) -> LlmConfig {
    let mut c = LlmConfig {
        kind: main,
        ..Default::default()
    };
    c.second.kind = second;
    c.lenses.security = lens;
    c
}

#[test]
fn a_second_opinion_with_no_second_reviewer_says_so() {
    let c = cfg(ProviderKind::Ollama, ProviderKind::Off, true);
    let (_, action) = unset_in_settings(Step::SecondOpinion, &c).unwrap();
    assert_eq!(action, "Choose a second reviewer");
    assert!(unset_in_settings(Step::Lenses, &c).is_none());
}

#[test]
fn focused_reviews_with_no_lens_on_say_so() {
    let c = cfg(ProviderKind::Ollama, ProviderKind::Remote, false);
    let (_, action) = unset_in_settings(Step::Lenses, &c).unwrap();
    assert_eq!(action, "Turn on a focused review");
    assert!(unset_in_settings(Step::SecondOpinion, &c).is_none());
}

#[test]
fn with_no_model_at_all_the_model_comes_first() {
    // Picking a second reviewer or a lens would not help: both are
    // skipped while the install runs without a model.
    let c = cfg(ProviderKind::Off, ProviderKind::Remote, true);
    for step in [Step::SecondOpinion, Step::Lenses] {
        assert_eq!(unset_in_settings(step, &c).unwrap().1, "Choose a model");
    }
}

#[test]
fn other_steps_need_nothing_from_here() {
    let c = cfg(ProviderKind::Off, ProviderKind::Off, false);
    assert!(unset_in_settings(Step::Commit, &c).is_none());
}
