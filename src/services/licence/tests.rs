use super::*;

const LICENCE: &str = "ABCDE-FGHJK-LMNPQ-RSTUV";
const DAY: u64 = 24 * 60 * 60;

fn account(active: bool, products: &[&str]) -> Account {
    Account {
        licence: LICENCE.into(),
        email: "dev@example.ch".into(),
        plan: Plan {
            id: "studio".into(),
            name: "Studio".into(),
        },
        products: products.iter().map(|p| p.to_string()).collect(),
        active,
        status: if active { "active" } else { "canceled" }.into(),
        ..Default::default()
    }
}

fn saved(account: Option<Account>, checked: u64) -> Saved {
    Saved {
        licence: LICENCE.into(),
        answer: account,
        checked,
    }
}

#[test]
fn a_mistyped_licence_number_says_what_is_wrong() {
    assert!(check_typed("abcde fghjk-lmnpq rstuv").is_ok());
    assert!(check_typed("").unwrap_err().contains("Paste"));
    assert!(check_typed("IBCDE-FGHJK-LMNPQ-RSTUV")
        .unwrap_err()
        .contains("\u{201c}I\u{201d}"));
    assert!(check_typed("ABCDE-FGHJK")
        .unwrap_err()
        .contains("this has 10"));
}

#[test]
fn pro_needs_an_active_plan_that_lists_gitagent_or_every_app() {
    let at = |a: Account| status_at(Some(&saved(Some(a), DAY)), DAY);
    assert!(matches!(at(account(true, &["gitagent"])), Status::Pro(_)));
    assert!(matches!(
        at(account(true, &["splitter", "*"])),
        Status::Pro(_)
    ));
    assert!(matches!(
        at(account(true, &["splitter"])),
        Status::Credits(_)
    ));
    assert!(matches!(
        at(account(false, &["*"])),
        Status::Lapsed(Lapse::Inactive(_))
    ));
}

#[test]
fn pro_lasts_seven_days_without_a_check_that_gets_through() {
    let s = saved(Some(account(true, &["gitagent"])), 1_000 * DAY);
    let at = |now: u64| status_at(Some(&s), now);
    assert!(at(1_000 * DAY).is_pro());
    assert!(at(1_007 * DAY).is_pro());
    assert!(matches!(
        at(1_007 * DAY + 1),
        Status::Lapsed(Lapse::Offline(_))
    ));
    assert!(
        at(999 * DAY).is_pro(),
        "a clock set back waits for the next check"
    );
}

#[test]
fn no_licence_an_unchecked_one_and_an_unknown_one_are_not_pro() {
    assert_eq!(status_at(None, 0), Status::Free);
    assert_eq!(
        status_at(Some(&saved(None, 0)), DAY),
        Status::Lapsed(Lapse::NotChecked)
    );
    assert_eq!(
        status_at(Some(&saved(None, DAY)), DAY),
        Status::Lapsed(Lapse::Unknown)
    );
}

#[test]
fn a_free_licence_shows_its_credits_and_is_not_pro() {
    let mut free = account(true, &[]);
    free.credits = Credits {
        granted: 50,
        used: 8,
        left: 42,
    };
    let status = status_at(Some(&saved(Some(free), DAY)), DAY);
    let Status::Credits(a) = status else {
        panic!("a licence without GitAgent shows its credits, got {status:?}");
    };
    assert_eq!(
        a.credits_headline(),
        "Free \u{2014} 42 AI credits left this month"
    );
    let mut granted = account(true, &["splitter"]);
    granted.credits.left = 7;
    assert_eq!(
        granted.credits_headline(),
        "AI credits only, not Pro \u{2014} 7 AI credits left this month"
    );
    // A cancelled one is just inactive.
    assert!(matches!(
        status_at(Some(&saved(Some(account(false, &[])), DAY)), DAY),
        Status::Lapsed(Lapse::Inactive(_))
    ));
}

#[test]
fn the_apps_a_plan_includes_are_named() {
    assert_eq!(account(true, &["*"]).apps(), "every mayorana.ch app");
    assert_eq!(account(true, &[]).apps(), "none");
    assert_eq!(
        account(true, &["gitagent", "trackcut"]).apps(),
        "gitagent, trackcut"
    );
}

#[test]
fn what_an_older_gitagent_saved_is_carried_over() {
    let dir = std::env::temp_dir().join(format!("gitagent-licence-legacy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("licence"), "abcde-fghjk-lmnpq-rstuv\n").unwrap();
    std::fs::write(
        dir.join("licence_check.json"),
        r#"{"licence":"ABCDE-FGHJK-LMNPQ-RSTUV","checked":1234,"account":{
            "email":"dev@example.ch","plan":{"id":"studio","name":"Studio"},"products":["*"],
            "active":true,"status":"active","cancel_at_period_end":false,
            "period_end":"2026-11-21T10:00:00Z","credits":{"granted":10,"used":1,"left":9}}}"#,
    )
    .unwrap();
    let s = legacy(&dir).unwrap();
    assert_eq!(s.licence, LICENCE);
    assert_eq!(s.checked, 1234);
    let a = s.answer.unwrap();
    assert!(a.unlocks_for(PRODUCT));
    assert_eq!(a.period_end_day(), "2026-11-21");

    // An answer about another number, or none, leaves it to be checked.
    std::fs::write(
        dir.join("licence_check.json"),
        r#"{"licence":"ZZZZZ-ZZZZZ-ZZZZZ-ZZZZZ","checked":5,"account":null}"#,
    )
    .unwrap();
    assert_eq!(legacy(&dir).unwrap().checked, 0);
    // A signed key from before licence numbers is no licence.
    std::fs::write(dir.join("licence"), "eyJ2IjoxfQ.c2lnbmF0dXJl").unwrap();
    assert!(legacy(&dir).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}
