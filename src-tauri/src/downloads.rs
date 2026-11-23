use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, TryLockError},
    time::{Duration, Instant},
};

use base64::Engine as _;
use serde::Serialize;
use tauri::Manager;

/// Hard cap on a single decoded image. Slack images are far smaller; this only
/// guards against a pathological payload coming from the page.
const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;
/// General attachments can be much larger than images, but the payload crosses
/// Tauri's JSON IPC as base64 (~1.33x the bytes held in memory at once), so an
/// explicit ceiling still applies instead of accepting unbounded files.
const MAX_FILE_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_STEM: &str = "slack-image";
const DEFAULT_FILE_STEM: &str = "slack-file";

#[derive(Debug, Serialize)]
#[serde(tag = "kind", content = "message")]
pub(crate) enum SaveError {
    EmptyData,
    InvalidData,
    TooLarge,
    NoDownloadDir,
    DownloadBusy,
    QuarantineFailed(String),
    WriteFailed(String),
    OpenFailed(String),
}

/// Persist an image the page already fetched (with its own cookies) into the
/// user's Downloads folder. The bytes arrive base64-encoded because the WebView
/// is the only context that can read authenticated `files.slack.com` images,
/// and JSON IPC makes a compact string cheaper than a raw byte array.
#[tauri::command]
pub(crate) async fn save_image(
    app_handle: tauri::AppHandle,
    filename: Option<String>,
    mime: Option<String>,
    data_base64: String,
) -> Result<String, SaveError> {
    tauri::async_runtime::spawn_blocking(move || {
        save_image_to_downloads(&app_handle, filename, mime, data_base64)
    })
    .await
    .map_err(|error| SaveError::WriteFailed(error.to_string()))?
}

fn save_image_to_downloads(
    app_handle: &tauri::AppHandle,
    filename: Option<String>,
    mime: Option<String>,
    data_base64: String,
) -> Result<String, SaveError> {
    let _slot = acquire_save_slot()?;
    let data = decode_base64_payload(&data_base64, MAX_IMAGE_BYTES)?;
    let dir = downloads_dir(app_handle)?;

    let filename = filename.unwrap_or_default();
    let mime = mime.unwrap_or_default();
    let stem = sanitize_stem(&filename, DEFAULT_STEM);
    let ext = image_extension(&data, &mime, &filename);
    let target = write_download(&dir, &stem, Some(ext), &data, quarantine)?;

    notify_saved(app_handle, "Image saved", &target, DEFAULT_STEM);
    Ok(target.to_string_lossy().into_owned())
}

/// Persist a non-image attachment (PDF, ZIP, ...) the page fetched with its own
/// cookies. Unlike `save_image`, the extension comes from the caller-supplied
/// filename alone — no image sniffing and no forced fallback extension.
#[tauri::command]
pub(crate) async fn save_file(
    app_handle: tauri::AppHandle,
    filename: Option<String>,
    data_base64: String,
) -> Result<String, SaveError> {
    tauri::async_runtime::spawn_blocking(move || {
        save_file_to_downloads(&app_handle, filename, data_base64)
    })
    .await
    .map_err(|error| SaveError::WriteFailed(error.to_string()))?
}

fn save_file_to_downloads(
    app_handle: &tauri::AppHandle,
    filename: Option<String>,
    data_base64: String,
) -> Result<String, SaveError> {
    let _slot = acquire_save_slot()?;
    let data = decode_base64_payload(&data_base64, MAX_FILE_BYTES)?;
    let dir = downloads_dir(app_handle)?;

    let filename = filename.unwrap_or_default();
    let stem = sanitize_stem(&filename, DEFAULT_FILE_STEM);
    let ext = file_extension(&filename);
    let target = write_download(&dir, &stem, ext.as_deref(), &data, quarantine)?;

    notify_saved(app_handle, "File saved", &target, DEFAULT_FILE_STEM);
    Ok(target.to_string_lossy().into_owned())
}

/// Reveal the Downloads folder in the OS file manager, creating it first if it
/// does not exist yet.
#[tauri::command]
pub(crate) fn open_downloads_folder(app_handle: tauri::AppHandle) -> Result<(), SaveError> {
    let dir = downloads_dir(&app_handle)?;
    open::that(&dir).map_err(|error| SaveError::OpenFailed(error.to_string()))
}

fn decode_base64_payload(data_base64: &str, max_bytes: usize) -> Result<Vec<u8>, SaveError> {
    if data_base64.is_empty() {
        return Err(SaveError::EmptyData);
    }
    // base64 expands the payload ~4:3; reject obvious oversizes before decoding.
    if data_base64.len() > max_bytes.div_ceil(3) * 4 {
        return Err(SaveError::TooLarge);
    }

    let data = base64::engine::general_purpose::STANDARD
        .decode(data_base64.as_bytes())
        .map_err(|_| SaveError::InvalidData)?;
    if data.is_empty() {
        return Err(SaveError::EmptyData);
    }
    if data.len() > max_bytes {
        return Err(SaveError::TooLarge);
    }
    Ok(data)
}

fn downloads_dir(app_handle: &tauri::AppHandle) -> Result<PathBuf, SaveError> {
    let dir = app_handle
        .path()
        .download_dir()
        .map_err(|_| SaveError::NoDownloadDir)?;
    std::fs::create_dir_all(&dir).map_err(|error| SaveError::WriteFailed(error.to_string()))?;
    Ok(dir)
}

fn notify_saved(app_handle: &tauri::AppHandle, title: &str, target: &Path, fallback: &str) {
    let display_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(fallback)
        .to_string();
    crate::native_notifications::show_local_notification(
        app_handle,
        title.to_string(),
        display_name,
    );
}

/// Derive a safe file stem from a caller-supplied name, dropping any path parts
/// and characters that are illegal on common filesystems. Everything else is
/// kept so the saved file carries the name the user actually sees in Slack.
fn sanitize_stem(filename: &str, default: &str) -> String {
    let raw = Path::new(filename)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("");
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_control()
                || is_format_char(c)
                || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
            {
                '_'
            } else {
                c
            }
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(|c| c == '.' || c == '_' || c == ' ');
    if trimmed.is_empty() {
        default.to_string()
    } else {
        let mut end = trimmed.len().min(200);
        while !trimmed.is_char_boundary(end) {
            end -= 1;
        }
        trimmed[..end].to_owned()
    }
}

// Bidi control and invisible formatting characters can disguise the extension.
fn is_format_char(c: char) -> bool {
    matches!(c, '\u{00ad}' | '\u{061c}' | '\u{180e}'
        | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{206f}' | '\u{feff}')
}

/// Extension for a general file, taken from the caller-supplied name only.
/// Restricted to short ASCII alphanumerics so a hostile name cannot smuggle in
/// separators; a missing or unusable extension yields `None` (no forced one).
fn file_extension(filename: &str) -> Option<String> {
    let ext = Path::new(filename)
        .extension()
        .and_then(|ext| ext.to_str())?;
    let cleaned: String = ext
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(16)
        .collect::<String>()
        .to_ascii_lowercase();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Pick an extension from the actual bytes first (most reliable), then the
/// response MIME type, then the URL-derived name, defaulting to `png`.
fn image_extension(data: &[u8], mime: &str, filename: &str) -> &'static str {
    sniff_extension(data)
        .or_else(|| extension_for_mime(mime))
        .or_else(|| extension_from_name(filename))
        .unwrap_or("png")
}

fn sniff_extension(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("png");
    }
    if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("jpg");
    }
    if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        return Some("gif");
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return Some("webp");
    }
    if data.starts_with(b"BM") {
        return Some("bmp");
    }
    if data.starts_with(&[0x00, 0x00, 0x01, 0x00]) {
        return Some("ico");
    }
    None
}

fn extension_for_mime(mime: &str) -> Option<&'static str> {
    let mime = mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    match mime.as_str() {
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" | "image/pjpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/bmp" | "image/x-ms-bmp" => Some("bmp"),
        "image/svg+xml" => Some("svg"),
        "image/tiff" => Some("tiff"),
        "image/avif" => Some("avif"),
        "image/x-icon" | "image/vnd.microsoft.icon" => Some("ico"),
        _ => None,
    }
}

fn extension_from_name(filename: &str) -> Option<&'static str> {
    let ext = Path::new(filename)
        .extension()
        .and_then(|ext| ext.to_str())?
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("png"),
        "jpg" | "jpeg" => Some("jpg"),
        "gif" => Some("gif"),
        "webp" => Some("webp"),
        "bmp" => Some("bmp"),
        "svg" => Some("svg"),
        "tif" | "tiff" => Some("tiff"),
        "avif" => Some("avif"),
        "ico" => Some("ico"),
        _ => None,
    }
}

fn acquire_save_slot() -> Result<MutexGuard<'static, Option<Instant>>, SaveError> {
    static SLOT: Mutex<Option<Instant>> = Mutex::new(None);
    const SAVE_INTERVAL: Duration = Duration::from_millis(250);
    const MAX_WAIT: Duration = Duration::from_secs(30);
    let deadline = Instant::now() + MAX_WAIT;
    let mut slot = loop {
        match SLOT.try_lock() {
            Ok(slot) => break slot,
            Err(TryLockError::Poisoned(error)) => break error.into_inner(),
            Err(TryLockError::WouldBlock) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(SaveError::DownloadBusy);
                }
                std::thread::sleep(remaining.min(Duration::from_millis(20)));
            }
        }
    };
    if let Some(started) = *slot {
        let delay = SAVE_INTERVAL.saturating_sub(started.elapsed());
        if delay > deadline.saturating_duration_since(Instant::now()) {
            return Err(SaveError::DownloadBusy);
        }
        std::thread::sleep(delay);
    }
    *slot = Some(Instant::now());
    Ok(slot)
}

/// Never clobber an existing file. Use a numbered suffix when a name is taken.
fn write_download(
    dir: &Path,
    stem: &str,
    extension: Option<&str>,
    data: &[u8],
    mark_quarantine: impl FnOnce(&File) -> io::Result<()>,
) -> Result<PathBuf, SaveError> {
    // Stage in a private directory on the destination filesystem. The final
    // name appears atomically after all bytes are written and quarantine verified.
    let stage = tempfile::Builder::new()
        .prefix(".zlack-download-")
        .tempdir_in(dir)
        .map_err(write_error)?;
    let staged_file = stage.path().join("download");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        #[cfg(target_os = "macos")]
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&staged_file).map_err(write_error)?;
    mark_quarantine(&file).map_err(|error| SaveError::QuarantineFailed(error.to_string()))?;
    file.write_all(data).map_err(write_error)?;
    file.sync_all().map_err(write_error)?;
    drop(file);

    for n in 0..=9999 {
        let mut name = if n == 0 {
            stem.to_owned()
        } else {
            format!("{stem} ({n})")
        };
        if let Some(extension) = extension {
            name.push('.');
            name.push_str(extension);
        }
        let target = dir.join(name);
        match std::fs::hard_link(&staged_file, &target) {
            Ok(()) => return Ok(target),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(write_error(error)),
        }
    }
    Err(SaveError::WriteFailed(
        "No available download filename".into(),
    ))
}

fn write_error(error: io::Error) -> SaveError {
    SaveError::WriteFailed(error.to_string())
}

#[cfg(target_os = "macos")]
fn quarantine(file: &File) -> io::Result<()> {
    use std::{
        os::fd::AsRawFd,
        time::{SystemTime, UNIX_EPOCH},
    };

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs();
    let value = format!("0081;{timestamp:08x};Zlack Trial;");
    let name = c"com.apple.quarantine";
    let fd = file.as_raw_fd();
    // Both calls use the open staged file descriptor, independent of link names.
    let written =
        unsafe { libc::fsetxattr(fd, name.as_ptr(), value.as_ptr().cast(), value.len(), 0, 0) };
    if written != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut actual = [0; 128];
    let len = unsafe {
        libc::fgetxattr(
            fd,
            name.as_ptr(),
            actual.as_mut_ptr().cast(),
            actual.len(),
            0,
            0,
        )
    };
    if len < 0 {
        return Err(io::Error::last_os_error());
    }
    if actual.get(..len as usize) != Some(value.as_bytes()) {
        return Err(io::Error::other("Could not verify quarantine metadata"));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn quarantine(_: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_known_image_signatures() {
        assert_eq!(
            sniff_extension(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0]),
            Some("png")
        );
        assert_eq!(sniff_extension(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpg"));
        assert_eq!(sniff_extension(b"GIF89a and more"), Some("gif"));
        let mut webp = Vec::from(&b"RIFF"[..]);
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBPVP8 ");
        assert_eq!(sniff_extension(&webp), Some("webp"));
        assert_eq!(sniff_extension(b"not an image"), None);
    }

    #[test]
    fn extension_prefers_bytes_then_mime_then_name() {
        let png = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        // Real PNG bytes beat a misleading name and empty MIME.
        assert_eq!(image_extension(&png, "", "photo.jpg"), "png");
        // Unknown bytes fall back to MIME.
        assert_eq!(image_extension(b"????unknown", "image/webp", "x"), "webp");
        // Unknown bytes and MIME fall back to the URL name.
        assert_eq!(image_extension(b"????unknown", "", "clip.GIF"), "gif");
        // MIME parameters are ignored.
        assert_eq!(
            image_extension(b"????unknown", "image/png; charset=binary", "x"),
            "png"
        );
        // Nothing usable defaults to png.
        assert_eq!(image_extension(b"????unknown", "", "noext"), "png");
    }

    #[test]
    fn sanitizes_stems_and_falls_back() {
        assert_eq!(sanitize_stem("my photo.png", DEFAULT_STEM), "my photo");
        assert_eq!(sanitize_stem("passwd", DEFAULT_STEM), "passwd");
        assert_eq!(sanitize_stem("", DEFAULT_STEM), DEFAULT_STEM);
        assert_eq!(sanitize_stem("....", DEFAULT_FILE_STEM), DEFAULT_FILE_STEM);
        let sanitized = sanitize_stem("a?b<c>d", DEFAULT_STEM);
        assert!(!sanitized.contains('?') && !sanitized.contains('<') && !sanitized.contains('>'));
        // Characters that are legal on disk stay, so the saved name matches Slack.
        assert_eq!(
            sanitize_stem("Q3 report (final)+v2!.pdf", DEFAULT_FILE_STEM),
            "Q3 report (final)+v2!"
        );
        // Unicode letters (e.g. Korean) are preserved.
        assert_eq!(sanitize_stem("사진.png", DEFAULT_STEM), "사진");
        // Multi-dot names keep the inner dots in the stem.
        assert_eq!(
            sanitize_stem("archive.tar.gz", DEFAULT_FILE_STEM),
            "archive.tar"
        );
    }

    #[test]
    fn file_extension_is_sanitized_or_absent() {
        assert_eq!(file_extension("report.PDF"), Some("pdf".to_string()));
        assert_eq!(file_extension("archive.tar.gz"), Some("gz".to_string()));
        assert_eq!(file_extension("noext"), None);
        assert_eq!(file_extension(""), None);
        // Illegal characters are stripped rather than written to disk.
        assert_eq!(file_extension("evil.p?d"), Some("pd".to_string()));
        // An extension with nothing usable left disappears entirely.
        assert_eq!(file_extension("weird.???"), None);
    }

    #[test]
    fn decode_base64_payload_validates_input() {
        assert!(matches!(
            decode_base64_payload("", 16),
            Err(SaveError::EmptyData)
        ));
        assert!(matches!(
            decode_base64_payload("not base64!!", 1024),
            Err(SaveError::InvalidData)
        ));
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"hello");
        assert_eq!(decode_base64_payload(&encoded, 1024).unwrap(), b"hello");
        assert!(matches!(
            decode_base64_payload(&encoded, 4),
            Err(SaveError::TooLarge)
        ));
    }

    #[test]
    fn concurrent_save_waits_for_the_native_slot() {
        let first = acquire_save_slot().unwrap();
        let started = first.unwrap();
        std::thread::scope(|scope| {
            let second = scope.spawn(|| {
                let slot = acquire_save_slot().unwrap();
                slot.unwrap()
            });
            drop(first);
            assert!(second.join().unwrap().duration_since(started) >= Duration::from_millis(250));
        });
    }

    #[test]
    fn publishes_concurrent_downloads_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let first = std::thread::scope(|scope| {
            let work = scope.spawn(|| {
                write_download(root, "report", Some("pdf"), b"first", |_| Ok(())).unwrap()
            });
            let second =
                write_download(root, "report", Some("pdf"), b"second", |_| Ok(())).unwrap();
            let first = work.join().unwrap();
            assert_ne!(first, second);
            assert_eq!(std::fs::read(second).unwrap(), b"second");
            first
        });
        assert_eq!(std::fs::read(first).unwrap(), b"first");
        assert_eq!(std::fs::read_dir(root).unwrap().count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn existing_links_are_treated_as_name_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original");
        std::fs::write(&original, b"unchanged").unwrap();
        std::os::unix::fs::symlink(&original, dir.path().join("README")).unwrap();
        let saved = write_download(dir.path(), "README", None, b"new", |_| Ok(())).unwrap();
        assert_eq!(saved.file_name().unwrap(), "README (1)");
        assert_eq!(std::fs::read(original).unwrap(), b"unchanged");
        assert_eq!(std::fs::read(saved).unwrap(), b"new");
    }

    #[test]
    fn quarantine_failure_never_publishes_a_download() {
        let dir = tempfile::tempdir().unwrap();
        let result = write_download(dir.path(), "report", Some("pdf"), b"data", |_| {
            Err(io::Error::other("simulated quarantine failure"))
        });
        assert!(matches!(result, Err(SaveError::QuarantineFailed(_))));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn published_mac_download_has_quarantine() {
        let dir = tempfile::tempdir().unwrap();
        let saved = write_download(dir.path(), "report", Some("pdf"), b"data", quarantine).unwrap();
        // Check the public path independently of the descriptor used for writing.
        let output = std::process::Command::new("/usr/bin/xattr")
            .arg("-p")
            .arg("com.apple.quarantine")
            .arg(saved)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8(output.stdout)
            .unwrap()
            .contains(";Zlack Trial;"));
    }
}
