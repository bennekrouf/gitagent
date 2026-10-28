//! The way back to mayorana.ch from inside the app.
//!
//! People get GitAgent as a download and may never see the site again: the
//! update banner only shows when there is an update, and the notice only when
//! one is published. These links are always on screen, so anyone with an idea
//! or a problem has somewhere to send it.

use dioxus::prelude::*;

pub use desktop_kit::feedback::contact_url;
use desktop_kit::feedback::site_url;

/// A quiet line of two links, for screens without a toolbar to put them in.
#[component]
pub fn FeedbackLine() -> Element {
    rsx! {
        div { class: "feedback-line",
            a {
                href: "{contact_url()}",
                target: "_blank",
                title: "Ideas, bugs, questions: send them to the person who makes GitAgent",
                "Feedback"
            }
            span { " · " }
            a { href: "{site_url()}", target: "_blank", "mayorana.ch" }
        }
    }
}
