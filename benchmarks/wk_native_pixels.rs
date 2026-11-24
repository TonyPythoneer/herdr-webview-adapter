//! A real GPUI window containing a Wry WKWebView, not DOM mocking.
#![cfg_attr(not(target_os = "macos"), allow(dead_code, unused_imports))]

// Import the very same production snapshot code into the standalone E2E
// binary, while providing an isolated screenshot directory.
#[cfg(target_os="macos")]
use herdr_webview_adapter::{app_home as mods, wk_native};

#[cfg(target_os = "macos")]
mod mac {
    use gpui::{prelude::*, *};
    use std::{io::{Read, Write}, net::TcpListener, path::PathBuf,
        sync::mpsc, time::{Duration, Instant}};

    use crate::{mods, wk_native};

    struct Scene {
        web: Entity<gpui_wry::WebView>,
    }
    impl Render for Scene {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.web.clone())
        }
    }

    fn fixture() -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/",listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let html = r#"<!doctype html><meta name="viewport" content="width=device-width, initial-scale=1">
<body style="margin:0;background:rgb(20,40,60);">
<div id="tile" style="background:rgb(255,0,0);width:100px;height:100px"></div>
<label for="name">Name</label><input id="name" aria-label="Name">
<button id="action">Click</button><div id="status">ready</div>
<script>document.getElementById('action').addEventListener('click',()=> {
 document.getElementById('status').textContent='clicked';});</script>"#;
            // A speculative connection must never hold up the actual document.
            // Keep serving until the native proof exits; a cancelled navigation
            // or retry must not consume the only document response.
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(25);
            let mut accepted = 0usize;
            while Instant::now() < deadline && accepted < 64 {
                let (mut conn, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => panic!("WK fixture accept failed: {error}"),
                };
                accepted += 1;
                std::thread::spawn(move || {
                    if conn.set_nonblocking(false).is_err()
                        || conn.set_read_timeout(Some(Duration::from_millis(250))).is_err()
                        || conn.set_write_timeout(Some(Duration::from_secs(1))).is_err() {
                        return;
                    }
                    let mut request = Vec::new();
                    let request_deadline = Instant::now() + Duration::from_secs(3);
                    while request.len() < 8192 && Instant::now() < request_deadline {
                        let mut chunk = [0u8; 512];
                        match conn.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => {
                                request.extend_from_slice(&chunk[..n]);
                                if request.windows(4).any(|part| part == b"\r\n\r\n") { break; }
                            }
                            Err(error) if matches!(error.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                                | std::io::ErrorKind::Interrupted) => continue,
                            Err(error) => {
                                eprintln!("WK fixture read failed: {error}");
                                return;
                            }
                        }
                    }
                    let request = String::from_utf8_lossy(&request);
                    let first_line = request.lines().next().unwrap_or("");
                    if !request.contains("\r\n\r\n") {
                        eprintln!("WK fixture incomplete/disconnected request: {first_line:?}");
                        return;
                    }
                    if first_line == "GET / HTTP/1.1" {
                        match write!(conn, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", html.len(), html)
                            .and_then(|_| conn.flush()) {
                            Ok(()) => eprintln!("WK fixture served document"),
                            Err(error) => eprintln!("WK fixture document write failed: {error}"),
                        }
                    } else {
                        let _ = conn.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    }
                });
            }
        });
        (url, handle)
    }

    async fn e2e(web: Entity<gpui_wry::WebView>, window: WindowHandle<Scene>,
        cx: &mut AsyncApp) -> Result<(), String>
    {
        cx.update_window(window.into(), |_, window, cx| {
            let _ = window.draw(cx);
        }).map_err(|e|format!("could not draw native WK child: {e}"))?;
        let mut loaded = false;
        let load_deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < load_deadline {
            let (tx,rx)=mpsc::channel();
            cx.update(|cx|web.update(cx,|view,_| {
                view.raw().evaluate_script_with_callback(
                    "document.readyState==='complete' && !!document.getElementById('tile')",
                    move |value| { let _=tx.send(value); })
            })).map_err(|e|format!("WebKit JavaScript launch failed: {e}"))?;
            let result = cx.background_executor().spawn(async move {
                rx.recv_timeout(Duration::from_millis(500))
            }).await;
            if result.ok().as_deref()==Some("true") {
                loaded = true;
                break;
            }
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
        if !loaded {return Err("real embedded WKWebView did not load the HTTP fixture".into());}
        cx.background_executor().timer(Duration::from_millis(400)).await;

        let (tx,rx)=mpsc::channel();
        cx.update(|cx|web.update(cx,|view,_|wk_native::start_tiff_snapshot(view,tx)))
            .map_err(|e|format!("WK snapshot launch failed: {e}"))?;
        let data = cx.background_executor().spawn(async move {
            rx.recv_timeout(Duration::from_secs(12))
        }).await.map_err(|e|format!("native WebKit snapshot timed out: {e}"))??;
        let output=wk_native::encode_and_save(&data,"abbacddcabbaabbd0000111122223333")?;
        let path=PathBuf::from(output["path"].as_str().ok_or("missing screenshot path")?);
        let image=image::open(&path).map_err(|e|e.to_string())?.to_rgba8();
        let pixels=image.pixels().filter(|pixel|
            pixel[0]>230 && pixel[1]<40 && pixel[2]<40 && pixel[3]>240).count();
        let _=std::fs::remove_file(&path);
        let _=std::fs::remove_dir_all(mods::app_dir());
        if pixels < 100 {
            return Err(format!("native WKWebView screenshot missing red fixture tile; red pixels={pixels}"));
        }
        eprintln!("native WKWebView screenshot pixel test passed: {}x{}, {pixels} red pixels",
            image.width(),image.height());
        Ok(())
    }

    pub fn run() {
        assert!(std::env::var_os("HERDR_APP_HOME").is_some(), "native screenshot E2E must set an isolated HERDR_APP_HOME");
        let (url, server)=fixture();
        gpui_platform::application().run(move |cx| {
            let mut view_slot=None;
            let window=cx.open_window(WindowOptions {
                window_bounds:Some(WindowBounds::Windowed(Bounds::new(
                    point(px(45.),px(45.)),size(px(420.),px(300.))))),
                show:true,focus:true,titlebar:None,
                ..Default::default()
            },|win,cx|{
                let parent=raw_window_handle::HasWindowHandle::window_handle(win).expect("native GPUI window handle");
                let native=wry::WebViewBuilder::new().with_url(&url)
                    .build_as_child(&parent).expect("real WKWebView child");
                let web=cx.new(|cx|gpui_wry::WebView::new(native,win,cx));
                view_slot=Some(web.clone());
                cx.new(|_|Scene{web})
            }).expect("GPUI native test window");
            let web=view_slot.expect("WKWebView entity");
            cx.activate(true);
            cx.spawn(async move |cx| {
                // AppKit may terminate during quit(); verify before exiting.
                match e2e(web,window,cx).await {
                    Ok(()) => {
                        eprintln!("WK_PIXEL_VERIFIED: native snapshot contains fixture pixels");
                        std::process::exit(0);
                    }
                    Err(error) => {
                        eprintln!("WK_PIXEL_FAILED: {error}");
                        std::process::exit(1);
                    }
                }
            }).detach();
        });
        let _ = server;
        panic!("WK_PIXEL_FAILED: AppKit exited before proof completed");
    }
}

fn main() {
    #[cfg(target_os="macos")]
    if std::env::var_os("HERDR_WK_NATIVE_SMOKE").is_some() {
        mac::run();
    }
}
