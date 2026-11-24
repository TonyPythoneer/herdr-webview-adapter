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
    use serde_json::json;
    let url=fixture();
    let binary=discover().unwrap();
    let root=std::path::PathBuf::from(std::env::var_os("HERDR_APP_HOME").unwrap());
    for i in 0..6 {
        for warm in [i%2==0,i%2!=0] {
            let profile=root.join(format!("{i}-{warm}"));
            let t=Instant::now();
            let client=Client::start(&binary,&profile).unwrap();
            let start=t.elapsed().as_secs_f64()*1000.;
            let t=Instant::now();
            if warm {
                // A disposable blank renderer only; never navigate real targets
                // from about:blank or change the proven open order.
                let r=client.request("Target.createTarget",json!({"url":"about:blank"}),None).unwrap();
                let target=r["targetId"].as_str().unwrap();
                let r=client.request("Target.attachToTarget",json!({"targetId":target,"flatten":true}),None).unwrap();
                let session=r["sessionId"].as_str().unwrap();
                client.send("Runtime.runIfWaitingForDebugger",json!({}),Some(session)).unwrap();
                let proof=client.request("Runtime.evaluate",json!({"expression":"1+1","returnByValue":true}),Some(session)).unwrap();
                assert_eq!(proof["result"]["value"],2);
                client.request("Target.closeTarget",json!({"targetId":target}),None).unwrap();
            }
            let prewarm=t.elapsed().as_secs_f64()*1000.;
            let (open,_)=visit(&client,&url);
            println!("{}",json!({"round":i,"prewarm":warm,"start_ms":start,"prewarm_ms":prewarm,"open_frame_ms":open,"total_ms":start+prewarm+open}));
            drop(client);
        }
    }
}
}
#[cfg(target_os="macos")]
fn main(){probe::run();}
#[cfg(not(target_os="macos"))]
fn main(){}
