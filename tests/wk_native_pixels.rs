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
        sync::mpsc, time::Duration};

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
            let (mut stream, _) = listener.accept().expect("WebKit fixture request");
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut buffer = [0u8; 4096];
            let count = stream.read(&mut buffer).unwrap_or(0);
            assert!(String::from_utf8_lossy(&buffer[..count]).starts_with("GET / HTTP/1.1"));
            let html = r#"<!doctype html><meta name="viewport" content="width=device-width, initial-scale=1">
<body style="margin:0;background:rgb(20,40,60);">
<div id="tile" style="background:rgb(255,0,0);width:100px;height:100px"></div>
<label for="name">Name</label><input id="name" aria-label="Name">
<button id="action">Click</button><div id="status">ready</div>
<script>document.getElementById('action').addEventListener('click',()=> {
 document.getElementById('status').textContent='clicked';});</script>"#;
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",html.len(),html).unwrap();
            stream.flush().unwrap();
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
        for _ in 0..30 {
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
        println!("native WKWebView screenshot pixel test passed: {}x{}, {pixels} red pixels",
            image.width(),image.height());
        Ok(())
    }

    pub fn run() {
        assert!(std::env::var_os("HERDR_APP_HOME").is_some(), "native screenshot E2E must set an isolated HERDR_APP_HOME");
        let (url, server)=fixture();
        let (report_tx,report_rx)=mpsc::channel();
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
            cx.spawn(async move |cx| {
                let outcome=e2e(web,window,cx).await;
                let _=report_tx.send(outcome);
                cx.update(|cx|cx.quit());
            }).detach();
        });
        // Never silently succeed if a scheduled task crashed or was cancelled.
        report_rx.recv_timeout(Duration::from_secs(50))
            .expect("WK native E2E task did not complete").unwrap();
        server.join().expect("fixture HTTP server failed");
    }
}

fn main() {
    #[cfg(target_os="macos")]
    if std::env::var_os("HERDR_WK_NATIVE_SMOKE").is_some() {
        mac::run();
    }
}
