//! A 14px mark for the platform a repository lives on.
//!
//! The GitHub mark is octicons' `mark-github` (MIT). Azure DevOps is drawn as
//! two interlocking rings rather than Microsoft's actual logo — it reads as the
//! same loop at this size without shipping someone's trademark. Anything else
//! falls back to a generic git-branch glyph, also from octicons.

use dioxus::prelude::*;

use crate::services::forge::Forge;

#[derive(Props, Clone, PartialEq)]
pub struct ForgeIconProps {
    pub forge: Forge,
    #[props(default = 14)]
    pub size: u32,
}

#[component]
pub fn ForgeIcon(props: ForgeIconProps) -> Element {
    let size = props.size.to_string();
    let title = props.forge.label();
    let class = match props.forge {
        Forge::GitHub => "forge-icon forge-github",
        Forge::AzureDevOps => "forge-icon forge-azure",
        _ => "forge-icon forge-plain",
    };

    rsx! {
        span { class: "forge-wrap", title: "{title}",
        svg {
            class: "{class}",
            width: "{size}",
            height: "{size}",
            view_box: "0 0 16 16",
            match props.forge {
                Forge::GitHub => rsx! {
                    path {
                        fill: "currentColor",
                        d: "M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 \
                            0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 \
                            1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 \
                            0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27s1.36.09 \
                            2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 \
                            3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.012 8.012 0 0 0 \
                            16 8c0-4.42-3.58-8-8-8z",
                    }
                },
                Forge::AzureDevOps => rsx! {
                    circle {
                        cx: "5.4", cy: "8", r: "2.7",
                        fill: "none", stroke: "currentColor", stroke_width: "1.5",
                    }
                    circle {
                        cx: "10.6", cy: "8", r: "2.7",
                        fill: "none", stroke: "currentColor", stroke_width: "1.5",
                    }
                },
                _ => rsx! {
                    path {
                        fill: "currentColor",
                        d: "M11.75 2.5a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Zm-2.25.75a2.25 2.25 0 1 1 3 \
                            2.122V6A2.5 2.5 0 0 1 10 8.5H6a1 1 0 0 0-1 1v1.128a2.251 2.251 0 1 1-1.5 \
                            0V5.372a2.25 2.25 0 1 1 1.5 0v1.836A2.492 2.492 0 0 1 6 7h4a1 1 0 0 0 \
                            1-1v-.628A2.25 2.25 0 0 1 9.5 3.25ZM4.25 12a.75.75 0 1 0 0 1.5.75.75 0 0 0 \
                            0-1.5ZM3.5 3.25a.75.75 0 1 1 1.5 0 .75.75 0 0 1-1.5 0Z",
                    }
                },
            }
        }
        }
    }
}
