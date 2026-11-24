//! Diagnostic only: same real CDP client, fresh target each time.
#[cfg(target_os = "macos")]
fn main() {
    use herdr_webview_adapter::chrome_cdp::{Client, discover};
    use serde_json::json;
    use std::{collections::HashMap, path::PathBuf, process::Command, sync::Arc,
        time::{Duration, Instant}};
    fn visit(client: &Arc<Client>, url: &str) -> (f64, f64, String) {
        let t = Instant::now();
        let pane = client.open(url).expect("fresh target");
        let open = t.elapsed().as_secs_f64() * 1000.0;
        let frame_start = Instant::now();
        let frame = pane.frame_timeout(Duration::from_secs(10)).expect("real frame");
        let frame_ms = frame_start.elapsed().as_secs_f64() * 1000.0;
        assert!(frame.starts_with(b"\x89PNG\r\n\x1a\n"));
        // Script might still be loading at interactive readiness. Measure its
        // verified completion separately; never mistake a cached image for a page.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if pane.eval("window.cacheProof || null").unwrap() == "cache-fixture-v1" { break; }
            assert!(Instant::now() < deadline, "fixture script did not execute");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(pane.eval("document.title").unwrap(), "Cache experiment");
        assert_eq!(pane.eval("typeof window.previousTargetMarker").unwrap(), "undefined");
        pane.eval("window.previousTargetMarker = true").unwrap();
        let target = pane.target.clone();
        drop(pane);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let targets = client.request("Target.getTargets", json!({}), None).unwrap();
            if !targets["targetInfos"].as_array().unwrap().iter()
                .any(|v| v["targetId"].as_str() == Some(target.as_str())) { break; }
            assert!(Instant::now() < deadline, "target not closed");
            std::thread::sleep(Duration::from_millis(5));
        }
        (open, frame_ms, target)
    }
    fn idle_rss() -> u64 {
        let output = Command::new("ps").args(["-axo","pid=,ppid=,rss="]).output().unwrap();
        assert!(output.status.success());
        let mut rows = HashMap::<u32,(u32,u64)>::new();
        for line in String::from_utf8(output.stdout).unwrap().lines() {
            let mut fields = line.split_whitespace();
            let pid = fields.next().unwrap().parse().unwrap();
            let parent = fields.next().unwrap().parse().unwrap();
            let rss = fields.next().unwrap().parse().unwrap();
            rows.insert(pid,(parent,rss));
        }
        let root = std::process::id();
        let mut owned = std::collections::HashSet::from([root]);
        loop {
            let n = owned.len();
            let children: Vec<_> = rows.iter().filter(|(_, (parent,_))| owned.contains(parent))
                .map(|(&pid,_)| pid).collect();
            owned.extend(children);
            if n == owned.len() { break; }
        }
        let rss = owned.iter().filter(|&&p| p != root).filter_map(|p| rows.get(p))
            .map(|(_,rss)| *rss).sum();
        assert!(rss > 0, "no Chrome descendants");
        rss
    }
    let mode = std::env::args().nth(1).expect("mode");
    assert!(matches!(mode.as_str(), "seed" | "restart" | "retained"));
    let profile = PathBuf::from(std::env::var_os("CACHE_PROFILE").unwrap());
    let url = std::env::var("CACHE_URL").unwrap();
    let binary = discover().unwrap();
    let t = Instant::now();
    let client = Client::start(&binary, &profile).unwrap();
    let start = t.elapsed().as_secs_f64() * 1000.0;
    let (mut open, mut frame, first) = visit(&client, &url);
    let mut rss = None;
    if mode == "retained" {
        // Model a short idle lease without changing production ownership.
        std::thread::sleep(Duration::from_millis(500));
        rss = Some(idle_rss());
        let (o, f, second) = visit(&client, &url);
        assert_ne!(first, second);
        open = o;
        frame = f;
    }
    // Exercise today's actual shutdown path; do not pretend this flushes disk.
    drop(client);
    println!("CACHE_RESULT {}", json!({
        "mode":mode, "chrome_version":binary.version,
        "start_ms":if mode == "retained" {0.0} else {start},
        "open_ms":open, "frame_ms":frame,
        "segments_ms":if mode == "retained" {open+frame} else {start+open+frame},
        "idle_descendant_rss_kib":rss
    }));
}
#[cfg(not(target_os = "macos"))]
fn main() { panic!("macOS Chrome required"); }
