const COMMANDS: &[&str] = &[
    "notify",
    "update_notification_context",
    "save_image",
    "save_file",
    "open_downloads_folder",
    "load_user_css",
    "open_external_url",
    "update_badge",
    "update_workspace_meta",
    "workspace_status",
    "register_workspaces",
    "switch_workspace",
];

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("macos/notification.m")
            .file("macos/webview.m")
            .flag("-fobjc-arc")
            .compile("zlack_macos_notification");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=WebKit");
        println!("cargo:rerun-if-changed=macos/webview.m");
    }

    // Declaring the app manifest makes every command deny-by-default, so only
    // the ones granted in capabilities/slack.json are reachable from Slack.
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS)),
    )
    .expect("failed to run tauri-build");
}
