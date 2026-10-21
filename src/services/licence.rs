//! GitAgent Pro: the licence on this computer.
//!
//! Pro comes with a monthly plan from mayorana.ch. Subscribing emails a
//! licence number, `ABCDE-FGHJK-LMNPQ-RSTUV`, which the buyer pastes here. The
//! app asks api0's gateway what the licence unlocks — at start and then daily
//! — and keeps the last answer on disk, so Pro carries on for [`GRACE_DAYS`]
//! after the last check that got through. A licence is Pro while its plan is
//! active and lists GitAgent (or every app, `"*"`).
//!
//! The same licence is the API key of api0's model provider, whose use comes
//! out of the plan's monthly AI credits. A licence whose plan doesn't list
//! GitAgent still gives those credits, but not Pro: the free licence anyone
//! can ask for by email from the licence window (a few credits a month, no
//! apps), or one granted by hand. If the free licence's email later buys a
//! plan, the same number moves to it.
//!
//! Without a licence GitAgent works in every repository, offline included: a
//! licence never decides which repositories flows can run in.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use super::store;

/// The product name in a plan's list of apps, and what GitAgent calls itself
/// to api0 (`X-Client-App`).
pub const PRODUCT: &str = "gitagent";

/// Where "Buy GitAgent Pro" leads.
pub const BUY_URL: &str = "https://mayorana.ch/en/apps/gitagent";

/// The plans and what each gives, for someone with no licence yet.
pub const PRICING_URL: &str = "https://mayorana.ch/pricing";

/// How long Pro lasts without reaching the gateway, after the last check that
/// did.
pub const GRACE_DAYS: i64 = 7;

/// How often a running app checks the licence again.
pub const CHECK_EVERY: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// After a check that couldn't reach the gateway, how soon to try again.
pub const RETRY_EVERY: std::time::Duration = std::time::Duration::from_secs(60 * 60);

const GATEWAY: &str = "https://gateway.api0.ai/v1";
/// Where the free licence is asked for, passed through by the gateway.
const FREE_CREDITS: &str = "https://gateway.api0.ai/api/plans/free";

/// The letters a licence number is made of: no 0, 1, I or O, which are easily
/// misread.
const ALPHABET: &str = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const LICENCE_LEN: usize = 20;
const GROUP: usize = 5;

const LICENCE_FILE: &str = "licence";
const CHECK_FILE: &str = "licence_check.json";

/// The licence number in `input`, as `ABCDE-FGHJK-LMNPQ-RSTUV`. Case, spaces
/// and dashes don't matter: numbers copied from an email come in all shapes.
pub fn normalise(input: &str) -> Result<String, String> {
    let chars: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if chars.is_empty() {
        return Err("Paste the licence number from your subscription email.".into());
    }
    if let Some(bad) = chars.chars().find(|c| !ALPHABET.contains(*c)) {
        return Err(format!(
            "A licence number has no \u{201c}{bad}\u{201d}. Copy it from your subscription email."
        ));
    }
    if chars.len() != LICENCE_LEN {
        return Err(format!(
            "A licence number has {LICENCE_LEN} letters and digits; this has {}. \
             Copy the whole number from your subscription email.",
            chars.len()
        ));
    }
    let groups: Vec<&str> = (0..LICENCE_LEN)
        .step_by(GROUP)
        .map(|i| &chars[i..i + GROUP])
        .collect();
    Ok(groups.join("-"))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credits {
    #[serde(default)]
    pub granted: i64,
    #[serde(default)]
    pub used: i64,
    #[serde(default)]
    pub left: i64,
}

/// What the gateway says a licence unlocks (`GET /v1/licence`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub plan: Plan,
    /// The apps the plan makes Pro; `"*"` is every app.
    #[serde(default)]
    pub products: Vec<String>,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub cancel_at_period_end: bool,
    #[serde(default)]
    pub period_end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub credits: Credits,
}

impl Account {
    pub fn includes_gitagent(&self) -> bool {
        self.products.iter().any(|p| p == PRODUCT || p == "*")
    }

    /// Whether this plan makes GitAgent Pro, as of the answer.
    pub fn unlocks(&self) -> bool {
        self.active && self.includes_gitagent()
    }

    /// The apps the plan includes, for reading.
    pub fn apps(&self) -> String {
        if self.products.iter().any(|p| p == "*") {
            "every mayorana.ch app".into()
        } else if self.products.is_empty() {
            "none".into()
        } else {
            self.products.join(", ")
        }
    }

    /// The day the plan renews, or ends when it is cancelled.
    pub fn period_end_day(&self) -> Option<String> {
        self.period_end.map(|d| d.format("%Y-%m-%d").to_string())
    }

    /// The free allowance everyone can have: AI credits, no apps.
    pub fn is_free(&self) -> bool {
        self.products.is_empty()
    }

    /// How a licence that isn't Pro is headed in the licence window.
    pub fn credits_headline(&self) -> String {
        let kind = if self.is_free() {
            "Free"
        } else {
            "AI credits only, not Pro"
        };
        format!(
            "{kind} \u{2014} {} AI credits left this month",
            self.credits.left
        )
    }
}

/// The last answer the gateway gave, kept for when it can't be reached.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// The licence asked about, so an answer never outlives a change of
    /// licence.
    pub licence: String,
    /// When the answer came, in seconds since 1970, by this computer's clock.
    pub checked: i64,
    /// `None` when the gateway didn't know the licence.
    pub account: Option<Account>,
}

/// Why a saved licence doesn't make GitAgent Pro.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lapse {
    /// The gateway hasn't been reached since the licence was saved.
    NotChecked,
    /// The gateway doesn't know this licence.
    Unknown,
    /// The subscription is cancelled, unpaid or over.
    Inactive(Account),
    /// The last successful check is older than the grace period.
    Offline(Account),
}

impl Lapse {
    pub fn account(&self) -> Option<&Account> {
        match self {
            Lapse::Inactive(a) | Lapse::Offline(a) => Some(a),
            Lapse::NotChecked | Lapse::Unknown => None,
        }
    }

    /// What is wrong and what to do about it, in a sentence or two.
    pub fn explain(&self) -> String {
        match self {
            Lapse::NotChecked => "Your licence hasn't been checked yet: GitAgent needs to reach \
                                  api0 once. Check your connection."
                .into(),
            Lapse::Unknown => "This licence number isn't known. Check it against your \
                               subscription email."
                .into(),
            Lapse::Inactive(a) => format!(
                "Your {} subscription isn't active ({}). Renew it to keep its AI credits.",
                a.plan.name,
                if a.status.is_empty() {
                    "ended"
                } else {
                    a.status.as_str()
                }
            ),
            Lapse::Offline(_) => format!(
                "GitAgent couldn't check your licence for more than {GRACE_DAYS} days. \
                 Connect to the internet to keep Pro."
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// No licence on this computer.
    Free,
    /// A licence whose plan makes GitAgent Pro.
    Pro(Account),
    /// An active plan that doesn't list GitAgent: AI credits for api0's
    /// model, but not Pro.
    Credits(Account),
    /// A licence that doesn't make GitAgent Pro now.
    Lapsed(Lapse),
}

impl Status {
    /// Whether this computer has GitAgent Pro.
    pub fn is_pro(&self) -> bool {
        matches!(self, Status::Pro(_))
    }
}

/// What `licence` (as saved) and the last `check` of it amount to at `now`
/// (seconds since 1970).
pub fn status_at(licence: Option<&str>, check: Option<&Check>, now: i64) -> Status {
    let Some(licence) = licence else {
        return Status::Free;
    };
    let Some(check) = check.filter(|c| c.licence == licence) else {
        return Status::Lapsed(Lapse::NotChecked);
    };
    let Some(account) = check.account.clone() else {
        return Status::Lapsed(Lapse::Unknown);
    };
    if !account.active {
        return Status::Lapsed(Lapse::Inactive(account));
    }
    if !account.unlocks() {
        return Status::Credits(account);
    }
    // A clock set back counts as within the grace: the next check that gets
    // through settles it either way.
    if now - check.checked > GRACE_DAYS * 24 * 60 * 60 {
        return Status::Lapsed(Lapse::Offline(account));
    }
    Status::Pro(account)
}

fn licence_path() -> PathBuf {
    store::data_dir().join(LICENCE_FILE)
}

fn check_path() -> PathBuf {
    store::data_dir().join(CHECK_FILE)
}

fn now() -> i64 {
    Utc::now().timestamp()
}

fn load_check() -> Option<Check> {
    let text = std::fs::read_to_string(check_path()).ok()?;
    serde_json::from_str(&text).ok()
}

fn save_check(check: &Check) {
    let _ = std::fs::create_dir_all(store::data_dir());
    if let Ok(json) = serde_json::to_string_pretty(check) {
        let _ = store::write_atomic(&check_path(), json.as_bytes());
    }
}

/// The licence number saved on this computer. A key from before licence
/// numbers (a signed key) counts as none.
pub fn saved_key() -> Option<String> {
    let text = std::fs::read_to_string(licence_path()).ok()?;
    normalise(&text).ok()
}

/// The licence on this computer as of its last check — no network. Read
/// afresh every time, so every window agrees.
pub fn current() -> Status {
    status_at(saved_key().as_deref(), load_check().as_ref(), now())
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("http client: {e}"))
}

/// The gateway's answer about `licence`: `Ok(None)` when it doesn't know it,
/// `Err` when it couldn't be asked.
async fn fetch(licence: &str) -> Result<Option<Account>, String> {
    let resp = client()?
        .get(format!("{GATEWAY}/licence"))
        .bearer_auth(licence)
        .header("X-Client-App", PRODUCT)
        .send()
        .await
        .map_err(|e| format!("api0 couldn't be reached: {e}"))?;
    let status = resp.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(None);
    }
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("api0 returned {status}"));
    }
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("api0's answer couldn't be read: {e}"))
}

/// Asks the gateway about the saved licence and keeps its answer. `Err` when
/// it couldn't be reached; [`current`] then still holds the last answer.
pub async fn check() -> Result<Status, String> {
    let Some(licence) = saved_key() else {
        return Ok(Status::Free);
    };
    let account = fetch(&licence).await?;
    save_check(&Check {
        licence,
        checked: now(),
        account,
    });
    Ok(current())
}

/// Checks the licence number in `input` and saves it with the answer. A
/// licence that is known but doesn't make GitAgent Pro is saved too, so its
/// plan can be shown and fixed from here.
pub async fn activate(input: &str) -> Result<Status, String> {
    let licence = normalise(input)?;
    let Some(account) = fetch(&licence).await? else {
        return Err(Lapse::Unknown.explain());
    };
    let _ = std::fs::create_dir_all(store::data_dir());
    store::write_atomic(&licence_path(), licence.as_bytes())
        .map_err(|e| format!("The licence couldn't be saved: {e}"))?;
    let check = Check {
        licence: licence.clone(),
        checked: now(),
        account: Some(account),
    };
    save_check(&check);
    Ok(status_at(Some(&licence), Some(&check), check.checked))
}

/// Removes the licence from this computer (to move it to another one).
pub fn deactivate() {
    let _ = std::fs::remove_file(licence_path());
    let _ = std::fs::remove_file(check_path());
}

/// The address of Stripe's page for changing or cancelling the subscription.
pub async fn portal_url() -> Result<String, String> {
    let licence = saved_key().ok_or("There is no licence on this computer.")?;
    let resp = client()?
        .post(format!("{GATEWAY}/licence/portal"))
        .bearer_auth(&licence)
        .header("X-Client-App", PRODUCT)
        .send()
        .await
        .map_err(|e| format!("api0 couldn't be reached: {e}"))?;
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("api0 returned {status}"));
    }
    serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v["url"].as_str().map(str::to_string))
        .filter(|u| u.starts_with("https://"))
        .ok_or_else(|| "api0 didn't give a page to open".into())
}

/// What asking for the free licence came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FreeReply {
    /// The licence number was emailed (or emailed again).
    Sent(String),
    /// Not sent, and why.
    Refused(String),
    /// There are no free credits right now.
    Unavailable,
}

fn free_reply(status: u16, body: &serde_json::Value, email: &str) -> FreeReply {
    match status {
        200..=299 => FreeReply::Sent(format!(
            "Check your email at {email}: paste the licence number it contains here."
        )),
        404 => FreeReply::Unavailable,
        409 => FreeReply::Refused(format!(
            "{email} already has a paid plan. Paste the licence number from its \
             subscription email."
        )),
        _ => FreeReply::Refused(
            body["message"]
                .as_str()
                .or(body["error"].as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("api0 returned {status}")),
        ),
    }
}

/// Asks for the free licence for `email`. Its number is only ever emailed,
/// which proves the address is real; asking again re-sends the same one, at
/// most once an hour.
pub async fn request_free_credits(email: &str) -> FreeReply {
    let email = email.trim();
    let well_formed = email
        .split_once('@')
        .is_some_and(|(name, host)| !name.is_empty() && host.contains('.'));
    if !well_formed {
        return FreeReply::Refused(
            "Enter the email address the licence number should go to.".into(),
        );
    }
    let client = match client() {
        Ok(c) => c,
        Err(e) => return FreeReply::Refused(e),
    };
    let resp = match client
        .post(FREE_CREDITS)
        .header("X-Client-App", PRODUCT)
        .json(&serde_json::json!({ "email": email }))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => return FreeReply::Refused(format!("api0 couldn't be reached: {e}")),
    };
    let status = resp.status().as_u16();
    let body: serde_json::Value = resp.json().await.unwrap_or_default();
    free_reply(status, &body, email)
}

/// Opens `url` in the default browser.
pub fn open_in_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");
    cmd.arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("The browser couldn't be opened: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LICENCE: &str = "ABCDE-FGHJK-LMNPQ-RSTUV";
    const DAY: i64 = 24 * 60 * 60;

    fn account(active: bool, products: &[&str]) -> Account {
        Account {
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

    fn checked(account: Option<Account>, at: i64) -> Check {
        Check {
            licence: LICENCE.into(),
            checked: at,
            account,
        }
    }

    #[test]
    fn a_licence_number_is_read_in_any_case_with_spaces_or_dashes() {
        for input in [
            "ABCDE-FGHJK-LMNPQ-RSTUV",
            "abcdefghjklmnpqrstuv",
            " abcde fghjk\nLMNPQ-rstuv ",
            "AB-CDE-FGHJKLMNPQRS-TUV",
        ] {
            assert_eq!(normalise(input).as_deref(), Ok(LICENCE), "{input:?}");
        }
    }

    #[test]
    fn a_short_long_or_misspelt_licence_number_is_refused() {
        assert!(normalise("").is_err());
        assert!(normalise("ABCDE-FGHJK-LMNPQ")
            .unwrap_err()
            .contains("this has 15"));
        assert!(normalise("ABCDE-FGHJK-LMNPQ-RSTUV-W").is_err());
        // 0, 1, I and O are not in the alphabet.
        assert!(normalise("ABCDE-FGHJK-LMNPQ-RSTU0")
            .unwrap_err()
            .contains('0'));
        assert!(normalise("IBCDE-FGHJK-LMNPQ-RSTUV").is_err());
        // A signed key from before licence numbers.
        assert!(normalise("eyJ2IjoxfQ.c2lnbmF0dXJl").is_err());
    }

    #[test]
    fn pro_needs_an_active_plan_that_lists_gitagent_or_every_app() {
        assert!(account(true, &["gitagent"]).unlocks());
        assert!(account(true, &["splitter", "*"]).unlocks());
        assert!(!account(true, &["splitter"]).unlocks());
        assert!(!account(false, &["gitagent"]).unlocks());

        let at = |a: Account| status_at(Some(LICENCE), Some(&checked(Some(a), 0)), 0);
        assert!(matches!(at(account(true, &["gitagent"])), Status::Pro(_)));
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
        let check = checked(Some(account(true, &["gitagent"])), 1_000 * DAY);
        let at = |now: i64| status_at(Some(LICENCE), Some(&check), now);
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
        assert_eq!(status_at(None, None, 0), Status::Free);
        assert_eq!(
            status_at(Some(LICENCE), None, 0),
            Status::Lapsed(Lapse::NotChecked)
        );
        let other = Check {
            licence: "ZZZZZ-ZZZZZ-ZZZZZ-ZZZZZ".into(),
            ..checked(Some(account(true, &["*"])), 0)
        };
        assert_eq!(
            status_at(Some(LICENCE), Some(&other), 0),
            Status::Lapsed(Lapse::NotChecked),
            "an answer about another licence doesn't count"
        );
        assert_eq!(
            status_at(Some(LICENCE), Some(&checked(None, 0)), 0),
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
        let status = status_at(Some(LICENCE), Some(&checked(Some(free), 0)), 0);
        assert!(!status.is_pro());
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
            status_at(
                Some(LICENCE),
                Some(&checked(Some(account(false, &[])), 0)),
                0
            ),
            Status::Lapsed(Lapse::Inactive(_))
        ));
    }

    #[test]
    fn the_free_credits_reply_is_read() {
        let none = serde_json::Value::Null;
        assert!(matches!(
            free_reply(200, &none, "a@b.ch"),
            FreeReply::Sent(m) if m.contains("a@b.ch")
        ));
        assert_eq!(free_reply(404, &none, "a@b.ch"), FreeReply::Unavailable);
        assert!(matches!(
            free_reply(409, &none, "a@b.ch"),
            FreeReply::Refused(m) if m.contains("paid plan")
        ));
        let slow = serde_json::json!({ "message": "Asked too often; try in an hour." });
        assert_eq!(
            free_reply(429, &slow, "a@b.ch"),
            FreeReply::Refused("Asked too often; try in an hour.".into())
        );
    }

    #[test]
    fn the_gateways_answer_is_read() {
        let body = r#"{"object":"licence","licence":"ABCDE-FGHJK-LMNPQ-RSTUV","email":"dev@example.ch",
            "plan":{"id":"studio","name":"Studio"},"products":["*"],"active":true,"status":"active",
            "cancel_at_period_end":false,"period_end":"2026-11-21T10:00:00Z",
            "credits":{"granted":1000,"used":580,"left":420},"checked_at":"2026-10-21T10:00:00Z"}"#;
        let a: Account = serde_json::from_str(body).unwrap();
        assert!(a.unlocks());
        assert_eq!(a.plan.name, "Studio");
        assert_eq!(a.apps(), "every mayorana.ch app");
        assert_eq!(a.credits.left, 420);
        assert_eq!(a.period_end_day().as_deref(), Some("2026-11-21"));
    }

    #[test]
    fn a_lapsed_licence_or_none_at_all_is_not_pro() {
        assert!(Status::Pro(account(true, &["*"])).is_pro());
        assert!(!Status::Free.is_pro());
        assert!(!Status::Lapsed(Lapse::Offline(account(true, &["*"]))).is_pro());
    }
}
