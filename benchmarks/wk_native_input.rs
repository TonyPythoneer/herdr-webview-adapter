//! Native trusted event proof: real GPUI child + WKWebView local fixture.
#![cfg_attr(not(target_os="macos"), allow(dead_code,unused_imports))]

#[cfg(target_os="macos")]
use herdr_webview_adapter::wk_input;

#[cfg(target_os="macos")]
mod mac {
    use gpui::{prelude::*,*};
    use std::{io::{Read,Write}, net::TcpListener, sync::mpsc, time::{Duration,Instant}};
    use super::wk_input;
    use wry::WebViewExtMacOS;

    struct Scene { web: Entity<gpui_wry::WebView> }
    impl Render for Scene {
        fn render(&mut self,_: &mut Window,_: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.web.clone())
        }
    }

    fn fixture() -> (String,std::thread::JoinHandle<()>) {
        let listener=TcpListener::bind("127.0.0.1:0").unwrap();
        let url=format!("http://{}/",listener.local_addr().unwrap());
        let handle=std::thread::spawn(move ||{
            let html=r#"<!doctype html><meta name="viewport" content="width=device-width,initial-scale=1">
<body style="margin:0;background:white;height:2200px">
<button id="trusted" style="position:absolute;left:20px;top:20px;width:120px;height:60px">Tap</button>
<div id="status" style="position:absolute;top:120px">pending</div>
<input id="field" aria-label="Field" style="position:absolute;top:175px;left:20px;width:120px;height:35px">
<script>
window.__pressed=false;window.__trusted=false;window.__keyTrusted=false;window.__textTrusted=false;
window.__wheelTrusted=false;
window.addEventListener('wheel',(e)=>{window.__wheelTrusted=e.isTrusted;});
document.getElementById('field').addEventListener('keydown',(e)=>{
 if(e.key==='H' || e.key==='i')window.__textTrusted=e.isTrusted;
});
document.getElementById('field').addEventListener('keydown',(e)=>{
 if(e.key==='Tab')window.__keyTrusted=e.isTrusted;
});
document.getElementById('trusted').addEventListener('click',(e)=>{
 window.__pressed=true;window.__trusted=e.isTrusted;
 document.getElementById('status').textContent=e.isTrusted?'trusted':'synthetic';
});
</script></body>"#;
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
        (url,handle)
    }

    async fn evaluate(web: &Entity<gpui_wry::WebView>,code: &'static str,cx: &mut AsyncApp) -> Result<String,String>{
        let (tx,rx)=mpsc::channel();
        cx.update(|cx|web.update(cx,|view,_|{
            view.raw().evaluate_script_with_callback(code,move |response|{let _=tx.send(response);})
        })).map_err(|e|format!("failed to invoke WebKit script: {e}"))?;
        cx.background_executor().spawn(async move{
            rx.recv_timeout(Duration::from_millis(800)).map_err(|e|format!("WebKit callback timeout: {e}"))
        }).await
    }

    async fn check(web: Entity<gpui_wry::WebView>,window: WindowHandle<Scene>,cx:&mut AsyncApp)->Result<(),String>{
        cx.update_window(window.into(), |_,win,cx|{let _=win.draw(cx);})
            .map_err(|e|format!("native GPUI frame unavailable: {e}"))?;
        let mut loaded=false;
        let load_deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < load_deadline {
            if evaluate(&web,"document.readyState==='complete' && !!document.getElementById('trusted')",cx).await.ok().as_deref()==Some("true"){
                loaded=true;break;
            }
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
        if !loaded{return Err("WK input fixture did not load".into());}
        // The macOS runner may show a window without giving it key focus.
        // Make the actual visible AppKit window key before sending native input.
        cx.update(|cx|web.update(cx,|view,_| {
            if let Some(window)=view.raw().webview().window() {
                window.makeKeyAndOrderFront(None);
            }
        }));
        cx.background_executor().timer(Duration::from_millis(350)).await;

        cx.update(|cx|web.update(cx,|v,_|wk_input::click_at(v,80.0,50.0)))
            .map_err(|e|format!("native AppKit pointer was rejected: {e}"))?;
        let mut last=String::new();
        for _ in 0..35 {
            last=evaluate(&web,"window.__pressed===true && window.__trusted===true",cx).await.unwrap_or_default();
            if last=="true" {
                println!("WKWebView received trusted native AppKit click: {last}");
                cx.update(|cx|web.update(cx,|v,_|wk_input::click_at(v,80.0,190.0)))
                    .map_err(|e|format!("could not focus test input: {e}"))?;
                cx.background_executor().timer(Duration::from_millis(150)).await;
                cx.update(|cx|web.update(cx,|v,_|wk_input::press_key(v,"Tab")))
                    .map_err(|e|format!("could not dispatch native Tab: {e}"))?;
                let mut observed=String::new();
                for _ in 0..25 {
                    observed=evaluate(&web,"window.__keyTrusted===true",cx).await.unwrap_or_default();
                    if observed=="true" {
                        eprintln!("WK_TRUSTED_KEY_VERIFIED: AppKit keyDown had isTrusted=true");
                        cx.update(|cx|web.update(cx,|v,_|wk_input::click_at(v,80.0,190.0)))
                            .map_err(|e|format!("could not refocus text field: {e}"))?;
                        cx.background_executor().timer(Duration::from_millis(100)).await;
                        cx.update(|cx|web.update(cx,|v,_|wk_input::type_text(v,"Hi")))
                            .map_err(|e|format!("could not type native text: {e}"))?;
                        let mut result=String::new();
                        for _ in 0..25 {
                            result=evaluate(&web,
                                "document.getElementById('field').value==='Hi' && window.__textTrusted===true",cx)
                                .await.unwrap_or_default();
                            if result=="true" {
                                eprintln!("WK_TRUSTED_TEXT_VERIFIED: native AppKit text produced trusted keydown and input value");
                                // A wheel command counts only when the actual
                                // embedded WebKit page both receives a trusted
                                // wheel event AND moves its own scroll position.
                                // A successful native API call alone is not proof.
                                cx.update(|cx|web.update(cx,|v,_|
                                    wk_input::scroll_at(v,80.0,120.0,0.0,260.0)))
                                    .map_err(|e|format!("native WK wheel input rejected: {e}"))?;
                                let mut observed=String::new();
                                for _ in 0..35 {
                                    observed=evaluate(&web,
                                        "window.scrollY>30 && window.__wheelTrusted===true",cx)
                                        .await.unwrap_or_default();
                                    if observed=="true" {
                                        eprintln!("WK_TRUSTED_WHEEL_VERIFIED: native wheel isTrusted=true and page scrollY advanced");
                                        return Ok(());
                                    }
                                    cx.background_executor().timer(Duration::from_millis(100)).await;
                                }
                                let diagnostic=evaluate(&web,
                                    "JSON.stringify({scrollY:window.scrollY,trusted:window.__wheelTrusted,scrollHeight:document.documentElement.scrollHeight,innerHeight:window.innerHeight})",cx)
                                    .await.unwrap_or_else(|err|format!("diagnostic unavailable: {err}"));
                                return Err(format!("native WebKit wheel did not scroll the actual page with isTrusted=true: {observed}; fixture diagnostics={diagnostic}"));
                            }
                            cx.background_executor().timer(Duration::from_millis(100)).await;
                        }
                        return Err(format!("native WK text did not produce trusted input: {result}"));
                    }
                    cx.background_executor().timer(Duration::from_millis(100)).await;
                }
                return Err(format!("WebKit did not receive trusted native Tab key: {observed}"));
            }
            cx.background_executor().timer(Duration::from_millis(100)).await;
        }
        Err(format!("WKWebView did not receive a trusted native click; last observation: {last}"))
    }

    pub fn run(){
        let (url,server)=fixture();
        // The test must be fail-closed. Some AppKit run() implementations
        // exit the process when cx.quit() is called, skipping any assertions
        // after run() and producing false-green CI results.
        gpui_platform::application().run(move|cx|{
            let mut web_slot=None;
            let window=cx.open_window(WindowOptions{
                window_bounds:Some(WindowBounds::Windowed(Bounds::new(
                    point(px(70.),px(70.)),size(px(360.),px(280.))))),
                show:true,focus:true,titlebar:None,..Default::default()
            },|win,cx|{
                let parent=raw_window_handle::HasWindowHandle::window_handle(win).expect("native GPUI handle");
                let native=wry::WebViewBuilder::new().with_url(&url)
                    .build_as_child(&parent).expect("Wry WKWebView child");
                let web=cx.new(|cx|gpui_wry::WebView::new(native,win,cx));
                web_slot=Some(web.clone());
                cx.new(|_|Scene{web})
            }).expect("native GPUI test window");
            let web=web_slot.unwrap();
            cx.activate(true);
            cx.spawn(async move |cx|{
                let outcome=check(web,window,cx).await;
                match outcome {
                    Ok(()) => {
                        eprintln!("WK_TRUSTED_INPUT_VERIFIED: native click had isTrusted=true");
                        std::process::exit(0);
                    }
                    Err(error) => {
                        eprintln!("WK_TRUSTED_INPUT_FAILED: {error}");
                        std::process::exit(1);
                    }
                }
            }).detach();
        });
        let _ = server;
        panic!("WK_TRUSTED_INPUT_FAILED: AppKit exited before proof completed");
    }
}

fn main(){
    #[cfg(target_os="macos")]
    if std::env::var_os("HERDR_WK_NATIVE_INPUT_E2E").is_some() { mac::run(); }
}
