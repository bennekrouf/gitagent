//! GitAgent — an agentic graph for commit and deploy workflows.
//!
//! The window is two screens: a repository list, and a run view for one
//! repository. The flow it runs is hardcoded today (see `services::flow`); the
//! engine underneath it is not.

mod components;
mod screens;
mod services;
mod update_check;

use dioxus::desktop::LogicalSize;
use dioxus::prelude::*;

use screens::{first_run::FirstRun, welcome::Welcome, workspace::Workspace};
use services::llm::LlmConfig;
use services::store;

const MAIN_CSS: &str = include_str!("../assets/main.css");

fn webview_data_dir() -> std::path::PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("GitAgent")
}

fn window_config(title: &str) -> dioxus::desktop::Config {
    dioxus::desktop::Config::new()
        .with_data_directory(webview_data_dir())
        .with_window(
            dioxus::desktop::WindowBuilder::new()
                .with_title(title)
                .with_inner_size(LogicalSize::new(1240.0, 820.0))
                .with_window_icon(window_icon()),
        )
}

/// The window icon, decoded from the embedded logo.
///
/// build.rs embeds `assets/icon.ico` into the .exe resource, which covers the
/// Start menu and shortcuts — but the *window* (title bar, alt-tab, taskbar
/// button) shows only what the app sets at runtime, and Windows falls back to
/// a blank default when it sets nothing.
///
/// Downscaled to 64px on the way in: tao hands Windows this single bitmap for
/// every size it needs, and letting it stretch a 1024px source down to a 16px
/// title bar is what makes the icon look muddy.
fn window_icon() -> Option<dioxus::desktop::tao::window::Icon> {
    const ICON_PNG: &[u8] = include_bytes!("../assets/icon.png");
    const SIZE: u32 = 64;

    let img = image::load_from_memory(ICON_PNG).ok()?.resize_exact(
        SIZE,
        SIZE,
        image::imageops::FilterType::Lanczos3,
    );
    dioxus::desktop::tao::window::Icon::from_rgba(img.into_rgba8().into_raw(), SIZE, SIZE).ok()
}

/// Opens another window on `path`, in this same process.
///
/// One process, many windows — not many processes. Two copies of the binary
/// would race each other on `repos.json` and `settings.json`, and would fight
/// over the webview data directory, which WebView2 locks outright on Windows.
/// Windows inside one process share none of that: each gets its own VirtualDom
/// and its own signals, and the OS sees a single app.
pub fn open_in_new_window(path: String) {
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());
    let dom = VirtualDom::new_with_props(
        WindowRoot,
        WindowRootProps {
            initial: Some(path),
        },
    );
    dioxus::desktop::window().new_window(
        dom,
        window_config(&format!(
            "GitAgent {} — {}",
            env!("CARGO_PKG_VERSION"),
            name
        )),
    );
}

fn main() {
    // Before anything can shell out: a bundle launched from Finder does not
    // inherit the terminal's PATH, so gh, az and cargo would all read as
    // "not installed".
    services::env::adopt_login_path();
    // Claim the notification bundle before any run can raise one — see
    // services::notify for why an unclaimed one is worse than silence.
    services::notify::init();
    services::env::adopt_login_env();

    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info,hyper_util=warn,hyper=warn,reqwest=warn");
    }

    let cfg = window_config(concat!("GitAgent ", env!("CARGO_PKG_VERSION")));
    LaunchBuilder::desktop().with_cfg(cfg).launch(App);
}

/// The first window's root. Every other window is a `WindowRoot` too — this
/// exists only because the launcher needs a component that takes no props.
#[component]
fn App() -> Element {
    rsx! { WindowRoot { initial: Option::<String>::None } }
}

/// One window. Each has its own VirtualDom, so its own signals, its own theme
/// state and its own open workspace — and its own welcome screen to go back to.
#[component]
pub fn WindowRoot(initial: Option<String>) -> Element {
    let mut workspace = use_signal(|| initial);
    let llm_config = use_signal(store::load_settings);

    // Follow the OS theme until the user overrides it, exactly as ais-monitor does.
    let system_light = dark_light::detect() != dark_light::Mode::Dark;
    let mut is_light = use_signal(|| system_light);
    let theme_overridden = use_signal(|| false);

    // ── Auto-update check ──────────────────────────────────────────────────
    // Deliberately after a delay and entirely best-effort: a release check is
    // never worth slowing a cold start, and a failed one is not worth saying
    // anything about.
    let mut update_info = use_signal(|| Option::<update_check::UpdateInfo>::None);
    let mut update_dismissed = use_signal(|| false);
    use_coroutine(move |_rx: UnboundedReceiver<()>| async move {
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        if let Some(info) = update_check::check().await {
            update_info.set(Some(info));
        }
    });

    use_effect(move || {
        let css = MAIN_CSS.replace('`', "\\`").replace("${", "\\${");
        document::eval(&format!(
            "if(!document.getElementById('ga-css')){{var s=document.createElement('style');\
             s.id='ga-css';s.textContent=`{}`;document.head.appendChild(s);}}",
            css
        ));
    });

    use_effect(move || {
        let cls = if *is_light.read() { "light" } else { "" };
        document::eval(&format!("document.body.className = '{}';", cls));
    });

    use_coroutine(move |_rx: UnboundedReceiver<()>| async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
            if *theme_overridden.read() {
                continue;
            }
            let detected =
                tokio::task::spawn_blocking(|| dark_light::detect() != dark_light::Mode::Dark)
                    .await
                    .unwrap_or(*is_light.read());
            if detected != *is_light.read() {
                is_light.set(detected);
            }
        }
    });

    // Asked once, before anything else can be reached: every flow depends on
    // the answer, and a run that discovers it mid-way is a run that has already
    // wasted your time.
    let mut configured = use_signal(store::is_configured);

    let open = workspace.read().clone();

    rsx! {
        // Update banner — fixed top, dismissable per session.
        if let (Some(info), false) = (update_info.read().clone(), *update_dismissed.read()) {
            div { class: "update-banner",
                span { class: "update-banner-text",
                    "GitAgent "
                    strong { "{info.latest_version}" }
                    " is available (you have {env!(\"CARGO_PKG_VERSION\")})."
                }
                a {
                    class: "update-banner-link",
                    href: "{info.release_url}",
                    target: "_blank",
                    "Download"
                }
                button {
                    class: "update-banner-dismiss",
                    onclick: move |_| update_dismissed.set(true),
                    "×"
                }
            }
        }

        if !*configured.read() {
            FirstRun {
                llm_config,
                on_done: move |_| configured.set(true),
            }
        } else {
        match open {
            None => rsx! {
                Welcome {
                    llm_config,
                    is_light,
                    theme_overridden,
                }
            },
            Some(path) => rsx! {
                Workspace {
                    workspace: path,
                    llm_config,
                    is_light,
                    theme_overridden,
                    on_change_workspace: move |_| {
                        dioxus::desktop::window()
                            .set_title(concat!("GitAgent ", env!("CARGO_PKG_VERSION")));
                        workspace.set(None);
                    },
                }
            },
        }
        }
    }
}

/// Shared by both screens: `LlmConfig` lives in a signal so the settings panel
/// can be opened from either one. Each window holds its own copy; `settings.json`
/// on disk is the point they agree, and a run re-reads it before starting.
pub type ConfigSignal = Signal<LlmConfig>;
