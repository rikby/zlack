use std::{
    collections::HashMap,
    ffi::{c_char, c_void, CStr, CString},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use serde::Deserialize;
use tauri::{
    ipc::{CallbackFn, Invoke, InvokeResponse, InvokeResponseBody},
    webview::{InvokeRequest, NewWindowResponse, PermissionResponse},
    AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};
use url::Url;

use crate::security::{self, Destination};

use super::{is_local_page, PageEvent, PRODUCT_NAME};
const SIGNIN_PREFIX: &str = "signin-";
const SIGNIN_RETURN_COOLDOWN: Duration = Duration::from_secs(4);
const STORE_ID: [u8; 16] = *b"ZlackTrialRikby1";
const TRANSPORT_HEADER: &str = "zlack-native-transport";

pub(crate) struct Windows {
    transport_secret: tauri::http::HeaderValue,
    workspaces: Mutex<HashMap<String, Arc<WorkspaceView>>>,
}

impl Default for Windows {
    fn default() -> Self {
        Self {
            transport_secret: uuid::Uuid::new_v4().to_string().parse().unwrap(),
            workspaces: Mutex::default(),
        }
    }
}

#[derive(Default)]
struct WorkspaceView {
    generation: AtomicU64,
    signin: Mutex<SignInState>,
}

#[derive(Default)]
struct SignInState {
    window: Option<String>,
    last_handoff: Option<Instant>,
}

struct ViewContext {
    app: AppHandle,
    label: String,
    workspace: Arc<WorkspaceView>,
    source: Option<String>,
}

pub(crate) fn is_signin_label(label: &str) -> bool {
    label.starts_with(SIGNIN_PREFIX)
}

pub(crate) fn forget(app: &AppHandle, label: &str) {
    let windows = app.state::<Windows>();
    let mut workspaces = windows.workspaces.lock().unwrap();
    if let Some(view) = workspaces.remove(label) {
        let signin = view.signin.lock().unwrap().window.clone();
        drop(workspaces);
        if let Some(window) = signin.and_then(|label| app.get_webview_window(&label)) {
            let _ = window.destroy();
        }
    } else if is_signin_label(label) {
        let source = workspaces.iter().find_map(|(source, view)| {
            let mut signin = view.signin.lock().unwrap();
            if signin.window.as_deref() == Some(label) {
                signin.window = None;
                Some(source.clone())
            } else {
                None
            }
        });
        drop(workspaces);
        if let Some(source) = source {
            recover_blank_workspace(app, &source);
        }
    }
}

fn recover_blank_workspace(app: &AppHandle, source: &str) {
    if let Some(window) = app.get_webview_window(source) {
        let target = CString::new(super::local_url("signed-out.html").as_str()).unwrap();
        let _ = window.with_webview(move |view| unsafe {
            zlack_recover_blank_view(view.inner(), target.as_ptr());
        });
    }
}

fn current_workspace(app: &AppHandle, label: &str, expected: &Arc<WorkspaceView>) -> bool {
    app.state::<Windows>()
        .workspaces
        .lock()
        .unwrap()
        .get(label)
        .is_some_and(|view| Arc::ptr_eq(view, expected))
}

pub(crate) fn build_workspace(
    app: &AppHandle,
    label: &str,
    target: Url,
    visible: bool,
    maximized: bool,
) -> tauri::Result<WebviewWindow> {
    debug_assert!(security::is_client_page(&target) || is_local_page(&target));
    let window = base_builder(app, label)
        .user_agent(crate::user_agent())
        .initialization_script(crate::INIT_SCRIPT)
        .title(PRODUCT_NAME)
        .inner_size(1200.0, 800.0)
        .visible(visible)
        .maximized(maximized)
        .build()?;

    let workspace = Arc::new(WorkspaceView::default());
    app.state::<Windows>()
        .workspaces
        .lock()
        .unwrap()
        .insert(label.to_owned(), workspace.clone());
    install(&window, workspace, None, target)?;
    Ok(window)
}

fn base_builder<'a>(
    app: &'a AppHandle,
    label: &str,
) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    WebviewWindowBuilder::new(
        app,
        label,
        WebviewUrl::External(Url::parse("about:blank").unwrap()),
    )
    .data_store_identifier(STORE_ID)
    .disable_drag_drop_handler()
    .on_new_window(|_, _| NewWindowResponse::Deny)
}

fn install(
    window: &WebviewWindow,
    workspace: Arc<WorkspaceView>,
    source: Option<String>,
    target: Url,
) -> tauri::Result<()> {
    let view_window = window.clone();
    let context = ViewContext {
        app: window.app_handle().clone(),
        label: window.label().to_owned(),
        workspace,
        source,
    };
    let start_url = CString::new(target.as_str()).expect("a URL has no interior NUL");
    let result = window.with_webview(move |view| {
        let sign_in = context.source.is_some();
        let context = Box::into_raw(Box::new(context)).cast();
        // Tauri passes a live WKWebView on its main thread. The adapter owns the
        // context on success and gets installed before any remote request starts.
        let installed = unsafe {
            zlack_install_webview(
                view.inner(),
                sign_in,
                context,
                policy,
                invoke,
                advance,
                drop_context,
            )
        };
        if !installed {
            unsafe { drop_context(context) };
            let _ = view_window.destroy();
            eprintln!("Zlack Trial: could not install WebKit navigation policy");
            return;
        }
        // Use the WK handle directly while the runtime holds its dispatch lock.
        if !unsafe { zlack_load_url(view.inner(), start_url.as_ptr()) } {
            let _ = view_window.destroy();
            eprintln!("Zlack Trial: could not start navigation");
        }
    });
    if result.is_err() {
        let _ = window.destroy();
    }
    result
}

fn schedule(context: &ViewContext, task: impl FnOnce(AppHandle) + Send + 'static) {
    let app = context.app.clone();
    let task: Box<Box<dyn FnOnce() + Send>> = Box::new(Box::new(move || task(app)));
    // Dispatch any follow-up after the WebKit policy callback returns.
    unsafe { zlack_dispatch(Box::into_raw(task).cast(), run_task) };
}

unsafe extern "C" fn run_task(task: *mut c_void) {
    let task = unsafe { Box::from_raw(task.cast::<Box<dyn FnOnce() + Send>>()) };
    task();
}

fn start_signin(app: &AppHandle, source: String, workspace: Arc<WorkspaceView>, url: Url) {
    if !current_workspace(app, &source, &workspace) {
        return;
    }
    let logout = security::is_signout(&url);
    let label = {
        let mut signin = workspace.signin.lock().unwrap();
        if let Some(window) = signin
            .window
            .as_ref()
            .and_then(|label| app.get_webview_window(label))
        {
            drop(signin);
            if logout {
                crate::reset_workspace_session(app, &source);
                let _ = window.navigate(url);
            }
            crate::restore_window(&window);
            return;
        }
        if !logout
            && signin
                .last_handoff
                .is_some_and(|time| time.elapsed() < SIGNIN_RETURN_COOLDOWN)
        {
            drop(signin);
            if let Some(window) = app.get_webview_window(&source) {
                let _ = window.navigate(super::local_url("signed-out.html"));
            }
            return;
        }
        let label = format!("{SIGNIN_PREFIX}{}", uuid::Uuid::new_v4());
        signin.window = Some(label.clone());
        label
    };

    let title = format!(
        "{PRODUCT_NAME} sign-in — {}",
        url.host_str().unwrap_or_default()
    );
    let result = base_builder(app, &label)
        .title(title)
        .inner_size(960.0, 720.0)
        .on_permission_request(|_, _| PermissionResponse::Deny)
        .build()
        .and_then(|window| install(&window, workspace.clone(), Some(source.clone()), url));

    if result.is_err() {
        workspace.signin.lock().unwrap().window = None;
        recover_blank_workspace(app, &source);
        eprintln!("Zlack Trial: could not open sign-in window");
    } else if logout {
        crate::reset_workspace_session(app, &source);
    }
}

fn finish_signin(context: &ViewContext, source: &str, url: Url) {
    let source = source.to_owned();
    let label = context.label.clone();
    let workspace = context.workspace.clone();
    schedule(context, move |app| {
        if !current_workspace(&app, &source, &workspace) {
            return;
        }
        let mut signin = workspace.signin.lock().unwrap();
        if signin.window.as_deref() != Some(&label) {
            return;
        }
        let Some(target) = app.get_webview_window(&source) else {
            return;
        };
        if target.navigate(url).is_err() {
            return;
        }
        signin.window = None;
        signin.last_handoff = Some(Instant::now());
        drop(signin);
        if let Some(window) = app.get_webview_window(&label) {
            let _ = window.destroy();
        }
        crate::restore_window(&target);
    });
}

unsafe extern "C" fn policy(
    context: *mut c_void,
    url: *const c_char,
    get: bool,
    response: bool,
    popup: bool,
    link: bool,
) -> bool {
    let context = unsafe { &*context.cast::<ViewContext>() };
    let Some(url) = parse_url(url) else {
        return false;
    };
    if let Some(source) = context.source.as_deref() {
        if !security::is_signin_page(&url) {
            return false;
        }
        if get && security::is_signin_return(&url) {
            finish_signin(context, source, url);
            return false;
        }
        if get && popup {
            schedule_on_label(context, url);
        }
        return !popup;
    }

    if !popup && is_local_page(&url) {
        return true;
    }

    match security::workspace_destination(&url) {
        Destination::Workspace if popup && get => schedule_on_label(context, url),
        Destination::Workspace => return !popup,
        Destination::SignIn if get => {
            let source = context.label.clone();
            let workspace = context.workspace.clone();
            schedule(context, move |app| {
                start_signin(&app, source, workspace, url)
            });
        }
        Destination::External if get && link && !response => {
            schedule(context, move |_| {
                let _ = open::that(url.as_str());
            });
        }
        _ => {}
    }
    false
}

fn schedule_on_label(context: &ViewContext, url: Url) {
    let label = context.label.clone();
    let source = context.source.clone();
    let workspace = context.workspace.clone();
    schedule(context, move |app| {
        let current = match source {
            Some(source) => {
                current_workspace(&app, &source, &workspace)
                    && workspace.signin.lock().unwrap().window.as_deref() == Some(&label)
            }
            None => current_workspace(&app, &label, &workspace),
        };
        if current {
            if let Some(window) = app.get_webview_window(&label) {
                let _ = window.navigate(url);
            }
        }
    });
}

fn parse_url(raw: *const c_char) -> Option<Url> {
    if raw.is_null() {
        return None;
    }
    Url::parse(unsafe { CStr::from_ptr(raw) }.to_str().ok()?).ok()
}

unsafe extern "C" fn advance(context: *mut c_void) -> u64 {
    let context = unsafe { &*context.cast::<ViewContext>() };
    // Sign-in navigation never changes the source workspace's document generation.
    if context.source.is_some() {
        return 0;
    }
    context.workspace.generation.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "C" fn drop_context(context: *mut c_void) {
    drop(unsafe { Box::from_raw(context.cast::<ViewContext>()) });
}

#[derive(Deserialize)]
struct PageRequest {
    command: String,
    args: serde_json::Value,
}

struct Reply(usize);

impl Reply {
    fn complete(self, response: InvokeResponse) {
        let json = match response {
            InvokeResponse::Ok(InvokeResponseBody::Json(json)) => {
                format!(r#"{{"ok":true,"value":{json}}}"#)
            }
            InvokeResponse::Err(error) => format!(r#"{{"ok":false,"value":{}}}"#, error.0),
            InvokeResponse::Ok(InvokeResponseBody::Raw(_)) => {
                r#"{"ok":false,"value":"Unsupported response"}"#.to_string()
            }
        };
        let raw = self.0 as *mut c_void;
        let json = CString::new(json).ok();
        unsafe { zlack_reply(raw, json.as_ref().map_or(std::ptr::null(), |s| s.as_ptr())) };
        std::mem::forget(self);
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        unsafe { zlack_reply(self.0 as *mut c_void, std::ptr::null()) };
    }
}

unsafe extern "C" fn invoke(
    context: *mut c_void,
    url: *const c_char,
    json: *const c_char,
    reply: *mut c_void,
) {
    let reply = Reply(reply as usize);
    let context = unsafe { &*context.cast::<ViewContext>() };
    let Some(url) = parse_url(url).filter(security::is_client_page) else {
        return;
    };
    if context.source.is_some()
        || json.is_null()
        || !current_workspace(&context.app, &context.label, &context.workspace)
    {
        return;
    }
    let Some(window) = context.app.get_webview_window(&context.label) else {
        return;
    };
    let Ok(request) =
        serde_json::from_slice::<PageRequest>(unsafe { CStr::from_ptr(json) }.to_bytes())
    else {
        return;
    };
    if request.command.contains(':') {
        return;
    }
    // The scheme transport can also supply headers, but cannot learn this
    // per-process credential. Tauri's normal ACL still applies to this request.
    let mut headers = tauri::http::HeaderMap::new();
    headers.insert(
        TRANSPORT_HEADER,
        context.app.state::<Windows>().transport_secret.clone(),
    );
    window.on_message(
        InvokeRequest {
            cmd: request.command,
            callback: CallbackFn(0),
            error: CallbackFn(0),
            url,
            body: request.args.into(),
            headers,
            invoke_key: context.app.invoke_key().to_owned(),
        },
        Box::new(move |_, _, response, _, _| reply.complete(response)),
    );
}

pub(crate) fn authorized_invoke(invoke: &Invoke<tauri::Wry>) -> bool {
    invoke.message.headers().get(TRANSPORT_HEADER)
        == Some(
            &invoke
                .message
                .webview_ref()
                .state::<Windows>()
                .transport_secret,
        )
}

#[derive(Clone)]
pub(crate) struct Document {
    workspace: Arc<WorkspaceView>,
    generation: u64,
}

pub(crate) fn current_document(window: &WebviewWindow) -> Option<Document> {
    let windows = window.app_handle().state::<Windows>();
    let workspaces = windows.workspaces.lock().unwrap();
    let workspace = workspaces.get(window.label())?.clone();
    let generation = workspace.generation.load(Ordering::Relaxed);
    Some(Document {
        workspace,
        generation,
    })
}

pub(crate) fn document_is_current(window: &WebviewWindow, document: &Document) -> bool {
    current_workspace(window.app_handle(), window.label(), &document.workspace)
        && document.generation == document.workspace.generation.load(Ordering::Relaxed)
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
    let is_status = matches!(&event, PageEvent::WorkspaceStatus(_));
    let Ok(json) = serde_json::to_string(&event) else {
        return;
    };
    let Ok(json) = CString::new(json) else { return };
    let fallback = fallback.and_then(|url| CString::new(url.as_str()).ok());
    let target = window.clone();
    let _ = window.with_webview(move |view| {
        if !document_is_current(&target, &document) {
            return;
        }
        unsafe {
            zlack_send_page_event(
                view.inner(),
                document.generation,
                json.as_ptr(),
                is_status,
                fallback
                    .as_ref()
                    .map_or(std::ptr::null(), |url| url.as_ptr()),
            );
        }
    });
}

pub(crate) fn supported_platform() -> bool {
    unsafe { zlack_supported_webkit() }
}

extern "C" {
    fn zlack_supported_webkit() -> bool;
    fn zlack_install_webview(
        view: *mut c_void,
        sign_in: bool,
        context: *mut c_void,
        policy: unsafe extern "C" fn(*mut c_void, *const c_char, bool, bool, bool, bool) -> bool,
        invoke: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *mut c_void),
        advance: unsafe extern "C" fn(*mut c_void) -> u64,
        drop_context: unsafe extern "C" fn(*mut c_void),
    ) -> bool;
    fn zlack_dispatch(task: *mut c_void, run: unsafe extern "C" fn(*mut c_void));
    fn zlack_load_url(view: *mut c_void, url: *const c_char) -> bool;
    fn zlack_recover_blank_view(view: *mut c_void, url: *const c_char);
    fn zlack_reply(reply: *mut c_void, json: *const c_char);
    fn zlack_send_page_event(
        view: *mut c_void,
        generation: u64,
        json: *const c_char,
        is_status: bool,
        fallback: *const c_char,
    );
}
