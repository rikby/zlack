use std::sync::atomic::{AtomicU64, Ordering};

use tauri::{
    ipc::Invoke, webview::NewWindowResponse, AppHandle, Manager, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};
use url::Url;

use crate::security::{self, Destination};

use super::{is_local_page, PageEvent, PRODUCT_NAME};

#[derive(Default)]
pub(crate) struct Windows;

pub(crate) fn supported_platform() -> bool {
    true
}

pub(crate) fn is_signin_label(label: &str) -> bool {
    label.starts_with("signin-")
}

pub(crate) fn forget(_: &AppHandle, _: &str) {}

pub(crate) fn authorized_invoke(invoke: &Invoke<tauri::Wry>) -> bool {
    invoke
        .message
        .webview_ref()
        .url()
        .is_ok_and(|url| security::is_client_page(&url))
}

pub(crate) fn build_workspace(
    app: &AppHandle,
    label: &str,
    target: Url,
    visible: bool,
    maximized: bool,
) -> tauri::Result<WebviewWindow> {
    let handle = app.clone();
    let source = label.to_owned();
    WebviewWindowBuilder::new(app, label, WebviewUrl::External(target))
        .additional_browser_args("--disable-background-timer-throttling --disable-backgrounding-occluded-windows --disable-renderer-backgrounding")
        .user_agent(crate::user_agent())
        .title(PRODUCT_NAME)
        .visible(visible)
        .maximized(maximized)
        .inner_size(1200.0, 800.0)
        .initialization_script(crate::INIT_SCRIPT)
        .disable_drag_drop_handler()
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .on_navigation(move |url| match security::workspace_destination(url) {
            Destination::Workspace => true,
            Destination::SignIn => {
                let handle = handle.clone();
                let source = source.clone();
                let url = url.clone();
                std::thread::spawn(move || open_signin(&handle, source, url));
                false
            }
            _ => is_local_page(url),
        })
        .build()
}

fn open_signin(app: &AppHandle, source: String, url: Url) {
    static NEXT_SIGNIN: AtomicU64 = AtomicU64::new(1);
    if security::is_signout(&url) {
        crate::reset_workspace_session(app, &source);
    }
    let label = format!("signin-{}", NEXT_SIGNIN.fetch_add(1, Ordering::Relaxed));
    let sign_in = label.clone();
    let handle = app.clone();
    let _ = WebviewWindowBuilder::new(app, label, WebviewUrl::External(url))
        .title(format!("{PRODUCT_NAME} sign-in"))
        .inner_size(960.0, 720.0)
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .on_navigation(move |url| {
            if security::is_signin_return(url) {
                let handle = handle.clone();
                let source = source.clone();
                let sign_in = sign_in.clone();
                let url = url.clone();
                std::thread::spawn(move || {
                    if let Some(window) = handle.get_webview_window(&source) {
                        let _ = window.navigate(url);
                        crate::restore_window(&window);
                    }
                    if let Some(window) = handle.get_webview_window(&sign_in) {
                        let _ = window.destroy();
                    }
                });
                return false;
            }
            security::is_signin_page(url)
        })
        .build();
}

#[derive(Clone)]
pub(crate) struct Document(Url);

pub(crate) fn current_document(window: &WebviewWindow) -> Option<Document> {
    window
        .url()
        .ok()
        .filter(security::is_client_page)
        .map(Document)
}

pub(crate) fn document_is_current(window: &WebviewWindow, document: &Document) -> bool {
    window.url().is_ok_and(|url| url == document.0)
}

pub(crate) fn send_page_event(window: &WebviewWindow, event: PageEvent, fallback: Option<Url>) {
    if let Some(document) = current_document(window) {
        send_document_event(window, document, event, fallback);
    }
}

pub(crate) fn send_document_event(
    window: &WebviewWindow,
    document: Document,
    event: PageEvent,
    fallback: Option<Url>,
) {
    if !document_is_current(window, &document) {
        return;
    }
    let Ok(json) = serde_json::to_string(&event) else {
        return;
    };
    let Ok(expected) = serde_json::to_string(document.0.as_str()) else {
        return;
    };
    let script = format!(
        "if (window === window.top && window.location.origin === 'https://app.slack.com' && window.location.href === {expected} && typeof window.__ZlackPageEvent === 'function') {{ window.__ZlackPageEvent({json}); }}"
    );
    if window.eval(script).is_err() {
        if let Some(url) = fallback {
            let _ = window.navigate(url);
        }
    }
}
