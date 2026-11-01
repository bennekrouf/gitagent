//! GitAgent Pro: the licence on this computer.
//!
//! Pro comes with a monthly plan from mayorana.ch. Subscribing emails a
//! licence number, `ABCDE-FGHJK-LMNPQ-RSTUV`, which the buyer pastes here. The
//! app asks api0's gateway what the licence unlocks — at start and then daily
//! — and keeps the last answer on disk, so Pro carries on for a week after the
//! last check that got through. A licence is Pro while its plan is active and
//! lists GitAgent (or every app, `"*"`).
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
//!
//! The number's format, the file every Mayorana app shares, GitAgent's copy of
//! api0's last answer and the requests are `desktop_kit::licence`. What is
//! GitAgent's own is how it reads that answer: Pro, AI credits only, or why
//! not ([`Lapse`]).

use desktop_kit::licence::{self as kit, FreeError, Saved, GRACE};
use std::path::PathBuf;

use super::store;

pub use desktop_kit::licence::{Answer as Account, CHECK_EVERY};
#[cfg(test)]
use desktop_kit::licence::{Credits, Plan};

/// The product name in a plan's list of apps, and what GitAgent calls itself
/// to api0 (`X-Client-App`).
pub const PRODUCT: &str = "gitagent";

/// Where "Buy GitAgent Pro" leads.
pub const BUY_URL: &str = "https://mayorana.ch/en/apps/gitagent";

/// The plans and what each gives, for someone with no licence yet.
pub const PRICING_URL: &str = "https://mayorana.ch/pricing";

/// After a check that couldn't reach the gateway, how soon to try again.
pub const RETRY_EVERY: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// The letters a licence number is made of: no 0, 1, I or O, which are easily
/// misread.
const ALPHABET: &str = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const LICENCE_LEN: usize = 20;

/// Where the kit keeps GitAgent's copy of api0's last answer.
pub fn licence_file() -> Option<PathBuf> {
    Some(store::data_dir().join("licence.json"))
}

/// What GitAgent says about the number in `input` before asking api0: the
/// shape is checked by the kit too, but these say what is wrong with it.
fn check_typed(input: &str) -> Result<(), String> {
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
    Ok(())
}

/// How GitAgent words an answer.
pub trait AccountExt {
    /// The apps the plan includes, for reading.
    fn apps(&self) -> String;
    /// How a licence that isn't Pro is headed in the licence window.
    fn credits_headline(&self) -> String;
}

impl AccountExt for Account {
    fn apps(&self) -> String {
        if self.products.iter().any(|p| p == "*") {
            "every mayorana.ch app".into()
        } else if self.products.is_empty() {
            "none".into()
        } else {
            self.products.join(", ")
        }
    }

    fn credits_headline(&self) -> String {
        let kind = if self.products.is_empty() {
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

/// Why a saved licence doesn't make GitAgent Pro.
#[derive(Clone, Debug, PartialEq)]
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
                "GitAgent couldn't check your licence for more than {} days. \
                 Connect to the internet to keep Pro.",
                GRACE.as_secs() / 86_400
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

/// What the saved licence and api0's last answer amount to at `now` (seconds
/// since 1970). Pro and the grace period are the kit's; telling credits-only
/// from a lapse, and an unknown number from an unchecked one, is GitAgent's.
pub fn status_at(saved: Option<&Saved>, now: u64) -> Status {
    let Some(saved) = saved else {
        return Status::Free;
    };
    let Some(account) = saved.answer.clone() else {
        return Status::Lapsed(if saved.checked == 0 {
            Lapse::NotChecked
        } else {
            Lapse::Unknown
        });
    };
    if !account.active {
        return Status::Lapsed(Lapse::Inactive(account));
    }
    match kit::status_at(Some(saved), now, PRODUCT) {
        kit::Status::Pro(a) => Status::Pro(a),
        kit::Status::Offline(a) => Status::Lapsed(Lapse::Offline(a)),
        _ => Status::Credits(account),
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The licence number saved on this computer, by GitAgent or any other
/// Mayorana app.
pub fn saved_key() -> Option<String> {
    kit::saved().map(|s| s.licence)
}

/// The licence on this computer as of its last check — no network. Read
/// afresh every time, so every window agrees.
pub fn current() -> Status {
    status_at(kit::saved().as_ref(), now())
}

/// Asks the gateway about the saved licence and keeps its answer. `Err` when
/// it couldn't be reached; [`current`] then still holds the last answer.
pub async fn check() -> Result<Status, String> {
    kit::check().await.map(|_| current())
}

/// Checks the licence number in `input` and saves it with the answer. A
/// licence that is known but doesn't make GitAgent Pro is saved too, so its
/// plan can be shown and fixed from here.
pub async fn activate(input: &str) -> Result<Status, String> {
    check_typed(input)?;
    kit::activate(input).await.map(|_| current())
}

/// Removes the licence from this computer (to move it to another one), for
/// every Mayorana app on it.
pub fn deactivate() {
    kit::deactivate();
}

/// The address of Stripe's page for changing or cancelling the subscription.
pub async fn portal_url() -> Result<String, String> {
    let licence = saved_key().ok_or("There is no licence on this computer.")?;
    kit::portal_url(&licence).await
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

/// Asks for the free licence for `email`. Its number is only ever emailed,
/// which proves the address is real; asking again re-sends the same one, at
/// most once an hour.
pub async fn request_free_credits(email: &str) -> FreeReply {
    match kit::request_free_credits(email).await {
        Ok(message) => FreeReply::Sent(message),
        Err(FreeError::Unavailable) => FreeReply::Unavailable,
        Err(FreeError::Refused(why)) => FreeReply::Refused(why),
    }
}

/// Opens `url` in the default browser.
pub fn open_in_browser(url: &str) -> Result<(), String> {
    if desktop_kit::browser::open(url) {
        Ok(())
    } else {
        Err("The browser couldn't be opened.".into())
    }
}

/// Before the licence is first read: moves what GitAgent saved before it used
/// the kit — the number in `licence`, the last answer in `licence_check.json`
/// — into the kit's file, so nobody pastes their number again.
pub fn adopt_legacy() {
    let dir = store::data_dir();
    let (Some(target), old) = (licence_file(), dir.join("licence")) else {
        return;
    };
    if target.exists() || !old.exists() {
        return;
    }
    if let Some(saved) = legacy(&dir) {
        if let Ok(json) = serde_json::to_vec_pretty(&saved) {
            if store::write_atomic(&target, &json).is_err() {
                return;
            }
        }
    }
    let _ = std::fs::remove_file(old);
    let _ = std::fs::remove_file(dir.join("licence_check.json"));
}

/// The legacy files in `dir`, as the kit keeps them. `None` when there is no
/// licence number there (a signed key from before licence numbers counts as
/// none).
fn legacy(dir: &std::path::Path) -> Option<Saved> {
    #[derive(serde::Deserialize)]
    struct Check {
        licence: String,
        checked: i64,
        account: Option<serde_json::Value>,
    }
    let licence = kit::normalise(&std::fs::read_to_string(dir.join("licence")).ok()?)?;
    let check = std::fs::read_to_string(dir.join("licence_check.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Check>(&t).ok())
        .filter(|c| c.licence == licence);
    let Some(check) = check else {
        return Some(Saved {
            licence,
            ..Saved::default()
        });
    };
    let answer = check.account.and_then(|mut a| {
        // The old copy kept `period_end` as a date or `null`; the kit keeps
        // the text api0 sent.
        if a["period_end"].is_null() {
            a["period_end"] = serde_json::Value::String(String::new());
        }
        serde_json::from_value::<Account>(a).ok()
    });
    Some(Saved {
        licence,
        answer,
        checked: check.checked.max(1) as u64,
    })
}

#[cfg(test)]
mod tests;
