//! GitAgent — an agentic graph for commit and deploy workflows.
//!
//! The window is two screens: a repository list, and a run view for one
//! repository. The flow it runs is hardcoded today (see `services::flow`); the
//! engine underneath it is not.

mod components;
mod screens;
mod services;

use dioxus::desktop::LogicalSize;
use dioxus::prelude::*;

use screens::{welcome::Welcome, workspace::Workspace};
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
                .with_inner_size(LogicalSize::new(1240.0, 820.0)),
        )
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

    let open = workspace.read().clone();

    rsx! {
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

/// Shared by both screens: `LlmConfig` lives in a signal so the settings panel
/// can be opened from either one. Each window holds its own copy; `settings.json`
/// on disk is the point they agree, and a run re-reads it before starting.
pub type ConfigSignal = Signal<LlmConfig>;
