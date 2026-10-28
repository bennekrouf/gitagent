//! Names GitAgent to desktop-kit, the code every mayorana desktop app shares
//! (the update check and the notice), and lends it GitAgent's HTTP client.
//! GitAgent's usage statistics and licence are its own (see `telemetry` and
//! `services::licence`).

use std::future::Future;
use std::pin::Pin;

use desktop_kit::http::{Request, Response};

/// First thing in `main`: until then, nothing in desktop-kit records or sends.
pub fn init() {
    desktop_kit::init(desktop_kit::App {
        id: "gitagent",
        name: "GitAgent",
        version: env!("CARGO_PKG_VERSION"),
        // Unused: GitAgent keeps its own usage statistics (see `telemetry`).
        telemetry_folder: "gitagent",
        http_get,
    });
}

fn http_get(req: Request) -> Pin<Box<dyn Future<Output = Option<Response>> + Send>> {
    Box::pin(async move {
        let response = reqwest::Client::new()
            .get(&req.url)
            .header(reqwest::header::USER_AGENT, req.user_agent)
            .timeout(req.timeout)
            .send()
            .await
            .ok()?;
        let status = response.status().as_u16();
        let body = response.text().await.ok()?;
        Some(Response { status, body })
    })
}
