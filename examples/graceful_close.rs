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
use herdr_webview_adapter::chrome_cdp::{Client,discover};
    let url=fixture();
    let profile=std::path::PathBuf::from(std::env::var_os("HERDR_APP_HOME").unwrap()).join("profile");
    let binary=discover().unwrap();
    let client=Client::start(&binary,&profile).unwrap();
    visit(&client,&url);
    let pane=client.open(&url).unwrap();
    pane.eval("localStorage.setItem('cache-proof','persisted')").unwrap();
    drop(pane);
    let t=Instant::now();drop(client);
    let shutdown=t.elapsed().as_secs_f64()*1000.;
    let t=Instant::now();
    let second=Client::start(&binary,&profile).unwrap();
    let pane=second.open(&url).unwrap();
    let persisted=pane.eval("localStorage.getItem('cache-proof')").unwrap()=="persisted";
    let restart=t.elapsed().as_secs_f64()*1000.;
    drop(pane);drop(second);
    println!("{}",serde_json::json!({"graceful":std::env::var("HERDR_EXPERIMENT_GRACEFUL_CLOSE").unwrap(),"shutdown_ms":shutdown,"restart_ms":restart,"local_storage_persisted":persisted}));
}
}
#[cfg(target_os="macos")]
fn main(){probe::run();}
#[cfg(not(target_os="macos"))]
fn main(){}
