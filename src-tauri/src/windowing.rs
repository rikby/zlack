use serde::Serialize;
use url::Url;

pub(crate) const PRODUCT_NAME: &str = "Zlack Trial";

#[derive(Serialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub(crate) enum PageEvent {
    WorkspaceStatus(crate::WorkspaceStatus),
    Notification(crate::notifications::NotificationActivation),
    Navigate(Url),
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::*;

#[cfg(not(target_os = "macos"))]
mod other;
#[cfg(not(target_os = "macos"))]
pub(crate) use other::*;

pub(crate) fn startup_url() -> Url {
    if crate::slack_reachable() {
        Url::parse(crate::security::SLACK_START).unwrap()
    } else {
        local_url("index.html")
    }
}

pub(crate) fn local_url(page: &str) -> Url {
    let origin = if cfg!(target_os = "windows") {
        "http://tauri.localhost"
    } else {
        "tauri://localhost"
    };
    Url::parse(&format!("{origin}/{page}")).unwrap()
}

fn is_local_page(url: &Url) -> bool {
    *url == local_url("index.html") || *url == local_url("signed-out.html")
}
