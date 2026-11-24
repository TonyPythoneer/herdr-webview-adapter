#[cfg(target_os="macos")]
mod probe {
use std::{io::{Read,Write}, net::TcpListener, time::{Instant,Duration}};
fn fixture() -> String {
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();
    let url=format!("http://{}/index.html",listener.local_addr().unwrap());
    std::thread::spawn(move || for stream in listener.incoming() {
        let Ok(mut stream)=stream else {break};
        let _=stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut req=Vec::new();
        let mut bytes=[0;1024];
        while req.len()<16384 && !req.windows(4).any(|v|v==b"\r\n\r\n") {
            match stream.read(&mut bytes) {Ok(0)|Err(_)=>break,Ok(n)=>req.extend_from_slice(&bytes[..n])}
        }
        let body=b"<!doctype html><title>Cache probe</title><body>verified</body>";
        let _=write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",body.len());
        let _=stream.write_all(body);
    });
    url
}
fn visit(client:&std::sync::Arc<herdr_webview_adapter::chrome_cdp::Client>,url:&str)->(f64,String) {
    let t=Instant::now();
    let pane=client.open(url).unwrap();
    let frame=pane.frame_timeout(Duration::from_secs(10)).unwrap();
    let ms=t.elapsed().as_secs_f64()*1000.;
    assert!(frame.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(pane.eval("document.title").unwrap(),"Cache probe");
    assert_eq!(pane.eval("typeof window.marker").unwrap(),"undefined");
    pane.eval("window.marker=1").unwrap();
    let target=pane.target.clone();drop(pane);
    let limit=Instant::now()+Duration::from_secs(10);
    while client.request("Target.getTargets",serde_json::json!({}),None).unwrap()["targetInfos"].as_array().unwrap().iter().any(|v|v["targetId"].as_str()==Some(&target)) {
        assert!(Instant::now()<limit);std::thread::sleep(Duration::from_millis(5));
    }
    (ms,target)
}

pub fn run(){
use herdr_webview_adapter::chrome_cdp::shared_client;
    let url=fixture();
    let first=shared_client().unwrap();
    let old=std::sync::Arc::downgrade(&first);
    let (_,first_target)=visit(&first,&url); drop(first);
    std::thread::sleep(Duration::from_millis(500));
    let t=Instant::now();
    let second=shared_client().unwrap();
    let same=old.ptr_eq(&std::sync::Arc::downgrade(&second));
    let (_,second_target)=visit(&second,&url);
    let ms=t.elapsed().as_secs_f64()*1000.;
    assert_ne!(first_target,second_target);
    let retained=std::env::var("HERDR_EXPERIMENT_CHROME_RETENTION").as_deref()==Ok("1");
    assert_eq!(same,retained);
    let weak=std::sync::Arc::downgrade(&second); drop(second);
    let deadline=Instant::now()+Duration::from_secs(12);
    while weak.upgrade().is_some(){assert!(Instant::now()<deadline,"lease did not expire");std::thread::sleep(Duration::from_millis(200));}
    println!("{}",serde_json::json!({"retained":retained,"reopen_verified_ms":ms,"expired":true}));
}
}
#[cfg(target_os="macos")]
fn main(){probe::run();}
#[cfg(not(target_os="macos"))]
fn main(){}
