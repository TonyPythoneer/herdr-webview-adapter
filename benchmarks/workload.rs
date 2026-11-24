//! Fixed black-box workload; compiled against both revisions by scripts/performance.py.
use herdr_webview_adapter::{browser, browser_backend::{self, Availability, BrowserEngine}};
use std::{hint::black_box, time::Instant};
use serde_json::{json, Value};

fn sample<T>(name: &str, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let result = black_box(f());
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    println!("PERF {}", json!({"metric":name,"ms":ms}));
    result
}

fn portable() {
    let urls = [" example.com ", "127.0.0.1:8080/path", "https://example.com/a?q=1", "https://user:pass@example.com", "file:///etc/passwd"];
    let run = || {
        for (i, url) in urls.iter().enumerate() {
            let actual = browser::normalize_url(black_box(url));
            assert_eq!(actual.is_ok(), i < 3);
            black_box(actual).ok();
        }
    };
    let resolve = || {
        assert_eq!(browser_backend::resolve_new_tab(black_box(None), black_box(Availability { wkwebview: true, chrome_cdp: true })).unwrap(), BrowserEngine::ChromeCdp);
        assert!(browser_backend::resolve_new_tab(black_box(Some("chrome-cdp")), black_box(Availability { wkwebview: true, chrome_cdp: false })).is_err());
    };
    for _ in 0..1000 { run(); resolve(); }
    for _ in 0..100 {
        sample("url_policy_5000_calls", || { for _ in 0..1000 { run(); } });
        sample("engine_policy_2000_calls", || { for _ in 0..1000 { resolve(); } });
    }
}

#[cfg(target_os="macos")]
fn chrome() {
    use herdr_webview_adapter::{chrome_cdp::{Client,discover}, browser_api};
    use std::{sync::{Arc, atomic::AtomicBool}, time::Duration};
    let url = std::env::var("PERF_FIXTURE_URL").unwrap();
    let profile = std::path::PathBuf::from(std::env::var_os("PERF_PROFILE").unwrap());
    let binary = discover().expect("real Chrome 136+ required");
    println!("PERF_META {}", json!({"chrome_version":binary.version}));
    let client = sample("chrome_client_start", || Client::start(&binary, &profile).unwrap());
    for _ in 0..3 {
        let pane = Arc::new(sample("chrome_session_open", || client.open(&url).unwrap()));
        assert_eq!(pane.eval("document.title").unwrap(), "Herdr performance fixture");
        let frame = sample("chrome_first_frame_receive", || pane.frame_timeout(Duration::from_secs(10)).unwrap());
        assert!(frame.starts_with(b"\x89PNG\r\n\x1a\n"));
        let args = json!({"target":{"css":"#action"}});
        let cancel = AtomicBool::new(false);
        let query = || {
            let result = browser_api::run(&pane, "browser.query", &args, false, &cancel).unwrap();
            assert_eq!(result["count"], 1);
        };
        for _ in 0..3 { query(); }
        for _ in 0..20 {
            sample("chrome_query", query);
            sample("chrome_eval_roundtrip", || assert_eq!(pane.eval("document.title").unwrap(), "Herdr performance fixture"));
        }
        let pos: Value = pane.eval("(()=>{let r=document.querySelector('#action').getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})()").unwrap();
        for i in 1..=5 {
            sample("chrome_click_to_dom_observed", || {
                pane.click_at(pos["x"].as_f64().unwrap(),pos["y"].as_f64().unwrap()).unwrap();
                assert_eq!(pane.eval("window.clicks").unwrap(), i);
                assert_eq!(pane.eval("window.trusted").unwrap(), true);
            });
            let png = sample("chrome_screenshot_png", || pane.screenshot().unwrap());
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        }
        let target = pane.target.clone();
        sample("chrome_session_close_confirmed", || {
            drop(pane);
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let targets = client.request("Target.getTargets", json!({}), None).unwrap();
                if !targets["targetInfos"].as_array().unwrap().iter().any(|t| t["targetId"].as_str() == Some(target.as_str())) { break; }
                assert!(Instant::now() < deadline, "target not closed");
                std::thread::sleep(Duration::from_millis(5));
            }
        });
    }
    sample("chrome_client_shutdown", || drop(client));
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("portable") => portable(),
        #[cfg(target_os="macos")]
        Some("chrome") => chrome(),
        _ => panic!("unknown benchmark suite"),
    }
}
