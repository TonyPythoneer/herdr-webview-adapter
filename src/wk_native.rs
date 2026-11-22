//! Native (AppKit/WebKit) rendered-page snapshots for an embedded WKWebView.
//! No DOM canvas or synthetic JavaScript screenshot: this is WebKit's own
//! takeSnapshotWithConfiguration:completionHandler: on the real child view.
//! Call start_tiff_snapshot only on GPUI's main thread; conversion and I/O run
//! off-thread after the WebKit completion block posts owned bytes.
use std::{fs, io::{Cursor, Write}, os::unix::fs::{OpenOptionsExt, PermissionsExt}, path::PathBuf, sync::mpsc::Sender};

use block2::RcBlock;
use objc2_app_kit::NSImage;
use objc2_foundation::NSError;
use serde_json::{json, Value};
use wry::WebViewExtMacOS;

const MAX_TIFF_BYTES: usize = 64 * 1024 * 1024;
const MAX_PNG_BYTES: usize = 12 * 1024 * 1024;
const MAX_PIXEL_COUNT: u64 = 25_000_000;

/// The WebKit API calls the block asynchronously. The block converts
/// NSImage->TIFF on AppKit's thread and sends an owned Vec; no NS objects
/// are retained by background work.
pub fn start_tiff_snapshot(
    view: &gpui_wry::WebView,
    tx: Sender<Result<Vec<u8>, String>>,
) -> Result<(), String> {
    if !view.visible() {
        return Err("BROWSER_NOT_VISIBLE: WKWebView snapshot requires a visible pane".into());
    }
    let wk = view.raw().webview();
    let block = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
        let result = (|| -> Result<Vec<u8>, String> {
            if !error.is_null() {
                return Err("WK_SNAPSHOT_FAILED: WebKit returned an error".into());
            }
            // SAFETY: WebKit supplies the NSImage for this completion call.
            // We copy the TIFF data before the block returns.
            let image = unsafe { image.as_ref() }
                .ok_or("WK_SNAPSHOT_FAILED: WebKit returned no image")?;
            let tiff = unsafe { image.TIFFRepresentation() }
                .ok_or("WK_SNAPSHOT_FAILED: image has no TIFF data")?;
            if tiff.len() > MAX_TIFF_BYTES {
                return Err("RESULT_TOO_LARGE: native screenshot TIFF exceeds 64MiB".into());
            }
            Ok(tiff.to_vec())
        })();
        let _ = tx.send(result);
    });
    // SAFETY: WKWebView is an owned Wry child on the AppKit UI thread.
    // The Objective-C block retains its captured Sender until completion.
    unsafe { wk.takeSnapshotWithConfiguration_completionHandler(None, &block) };
    Ok(())
}

/// A bounded PNG saved under Herdr's existing home. Never accepts arbitrary
/// output paths from a website or a CLI/Agent caller.
pub fn encode_and_save(tiff: &[u8], request_id: &str) -> Result<Value, String> {
    if request_id.len() != 32 || !request_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("INVALID_ARGUMENT: invalid browser screenshot ID".into());
    }
    if tiff.len() > MAX_TIFF_BYTES {
        return Err("RESULT_TOO_LARGE: native screenshot exceeds 64MiB".into());
    }
    let image = image::load_from_memory_with_format(tiff, image::ImageFormat::Tiff)
        .map_err(|e| format!("SCREENSHOT_INVALID: {e}"))?;
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXEL_COUNT {
        return Err("RESULT_TOO_LARGE: WKWebView screenshot has unsafe dimensions".into());
    }
    let mut png = Cursor::new(Vec::<u8>::new());
    image.write_to(&mut png, image::ImageFormat::Png)
        .map_err(|e| format!("SCREENSHOT_ENCODE_FAILED: {e}"))?;
    let bytes = png.into_inner();
    if bytes.len() > MAX_PNG_BYTES {
        return Err("RESULT_TOO_LARGE: PNG screenshot exceeds 12MiB".into());
    }
    let directory: PathBuf = crate::app_home::app_dir().join("browser-screenshots");
    fs::create_dir_all(&directory).map_err(|e| format!("SCREENSHOT_WRITE_FAILED: {e}"))?;
    let meta = fs::symlink_metadata(&directory).map_err(|e| e.to_string())?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err("SCREENSHOT_WRITE_FAILED: screenshot directory must not be a symlink".into());
    }
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("SCREENSHOT_WRITE_FAILED: {e}"))?;
    let file = directory.join(format!("wk-{request_id}.png"));
    let mut out = fs::OpenOptions::new().create_new(true).write(true)
        .mode(0o600).custom_flags(libc::O_NOFOLLOW)
        .open(&file).map_err(|e| format!("SCREENSHOT_WRITE_FAILED: {e}"))?;
    if let Err(error) = out.write_all(&bytes).and_then(|_| out.sync_all()) {
        let _ = fs::remove_file(&file);
        return Err(format!("SCREENSHOT_WRITE_FAILED: {error}"));
    }
    Ok(json!({"path": file.to_string_lossy(), "width":width, "height":height,
        "format":"png", "source":"native-wkwebview"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn screenshots_validate_id_and_real_tiff_bytes() {
        assert!(encode_and_save(b"fake TIFF", "not-hex").is_err());
        assert!(encode_and_save(b"fake TIFF", &"a".repeat(32)).unwrap_err().contains("SCREENSHOT_INVALID"));
        assert!(encode_and_save(&vec![0;MAX_TIFF_BYTES+1], &"a".repeat(32)).unwrap_err().contains("RESULT_TOO_LARGE"));
    }
}
