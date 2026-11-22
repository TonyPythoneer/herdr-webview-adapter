//! Native macOS browser tabs for the Herdr fork.
//! The tab, pane, and persistence model belong to upstream Herdr. This module
//! owns only URL policy and the child WebKit view; it does not change herdr-core.
use serde_json::Value;

pub const MOD_NAME: &str = "__herdr_native_browser";

pub fn is_browser(name: &str) -> bool {
    name == MOD_NAME
}

/// Accept web navigation only. Never pass file://, javascript:, data:, or
/// credential-bearing URLs to a privileged WKWebView from a command.
pub fn normalize_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 4096 || raw.chars().any(char::is_control) {
        return Err("browser URL must be non-empty, at most 4096 characters, and contain no control characters".into());
    }
    let input = if raw.contains("://") || raw.starts_with("javascript:") || raw.starts_with("data:")
        || raw.starts_with("file:") || raw.starts_with("about:")
    {
        raw.to_owned()
    } else if raw == "localhost" || raw.starts_with("localhost:")
        || raw == "127.0.0.1" || raw.starts_with("127.0.0.1:")
        || raw == "[::1]" || raw.starts_with("[::1]:")
    {
        format!("http://{raw}")
    } else {
        format!("https://{raw}")
    };
    let url = url::Url::parse(&input).map_err(|e| format!("invalid browser URL: {e}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("only http:// and https:// URLs with a host are supported".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("embedded URL credentials are not allowed".into());
    }
    Ok(url.to_string())
}

pub fn url_from_props(props: &Value) -> Result<String, String> {
    let raw = props.get("url").and_then(Value::as_str).ok_or("browser pane has no URL")?;
    normalize_url(raw)
}


#[cfg(target_os = "macos")]
pub fn create<T: 'static>(
    window: &mut gpui::Window,
    cx: &mut gpui::Context<T>,
    url: &str,
) -> Result<gpui::Entity<gpui_wry::WebView>, String> {
    use gpui::AppContext as _;
    let url = normalize_url(url)?;
    let parent = raw_window_handle::HasWindowHandle::window_handle(window)
        .map_err(|e| format!("GPUI window handle unavailable: {e}"))?;
    let native = wry::WebViewBuilder::new()
        .with_url(&url)
        .build_as_child(&parent)
        .map_err(|e| format!("WKWebView creation failed: {e}"))?;
    Ok(cx.new(|cx| gpui_wry::WebView::new(native, window, cx)))
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn valid_urls() {
        assert_eq!(normalize_url(" example.com ").unwrap(), "https://example.com/");
        assert_eq!(normalize_url("http://localhost:3000").unwrap(), "http://localhost:3000/");
        assert_eq!(normalize_url("127.0.0.1:8080/path").unwrap(), "http://127.0.0.1:8080/path");
    }
    #[test]
    fn unsafe_urls_rejected() {
        for url in ["", "javascript:alert(1)", "data:text/html,hello", "file:///etc/passwd",
            "about:blank", "ftp://example.com", "https://user:pass@example.com",
            "https://example.com/\nHeader: injected"] {
            assert!(normalize_url(url).is_err(), "{url:?}");
        }
    }
    #[test]
    fn reserved_pane_marker_stays_stable() {
        assert!(is_browser(MOD_NAME));
        assert!(!is_browser("browser"));
        assert_eq!(url_from_props(&json!({"url":"example.com"})).unwrap(), "https://example.com/");
    }
}
