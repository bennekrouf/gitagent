//! Names GitAgent to desktop-kit, the code every mayorana desktop app shares
//! (usage statistics, the licence, the update check and the notice), and
//! lends it GitAgent's HTTP client.

use std::future::Future;
use std::pin::Pin;

use desktop_kit::http::{Method, Request, Response};

/// First thing in `main`: until then, nothing in desktop-kit records or sends.
pub fn init() {
    desktop_kit::init(desktop_kit::App {
        id: "gitagent",
        name: "GitAgent",
        version: env!("CARGO_PKG_VERSION"),
        telemetry_folder: "gitagent",
        product: "gitagent",
        licence_file: crate::services::licence::licence_file,
        http,
    });
}

fn http(req: Request) -> Pin<Box<dyn Future<Output = Option<Response>> + Send>> {
    Box::pin(async move {
        let client = reqwest::Client::new();
        let mut request = match req.method {
            Method::Get => client.get(&req.url),
            Method::Post => client.post(&req.url),
        }
        .header(reqwest::header::USER_AGENT, req.user_agent)
        .timeout(req.timeout);
        for (name, value) in req.headers {
            request = request.header(name, value);
        }
        if let Some(json) = req.json {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(json);
        }
        let response = request.send().await.ok()?;
        let status = response.status().as_u16();
        let body = response.text().await.ok()?;
        Some(Response { status, body })
    })
}
