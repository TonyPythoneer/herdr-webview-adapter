#[cfg(target_os="macos")]
fn main(){
    use herdr_webview_adapter::chrome_cdp::{Client,discover};
    use serde_json::{json,Value};
    use std::{io::{Read,Write},net::TcpListener,time::{Duration,Instant}};
    let listener=TcpListener::bind("127.0.0.1:0").unwrap();
    let url=format!("http://{}/",listener.local_addr().unwrap());
    std::thread::spawn(move || for stream in listener.incoming() {
        let Ok(mut stream)=stream else{break};
        let _=stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut request=Vec::new();let mut bytes=[0;1024];
        while request.len()<16384&&!request.windows(4).any(|b|b==b"\r\n\r\n"){
            match stream.read(&mut bytes){Ok(0)|Err(_)=>break,Ok(n)=>request.extend_from_slice(&bytes[..n])}
        }
        let body=b"<!doctype html><title>GPU probe</title><canvas id='canvas' width='256' height='256'></canvas>";
        let _=write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",body.len());
        let _=stream.write_all(body);
    });
    let binary=discover().unwrap();
    let profile=std::path::PathBuf::from(std::env::var_os("HERDR_APP_HOME").unwrap()).join("gpu-probe");
    let client=Client::start(&binary,&profile).unwrap();
    let info=client.request("SystemInfo.getInfo",json!({}),None).unwrap();
    let pane=client.open(&url).unwrap();
    assert_eq!(pane.eval("document.title").unwrap(),"GPU probe");
    let webgl=pane.eval(r#"(()=>{
      const gl=document.querySelector('canvas').getContext('webgl',{preserveDrawingBuffer:true});
      if(!gl)return {available:false};
      const ext=gl.getExtension('WEBGL_debug_renderer_info');
      gl.clearColor(1,0,0,1);gl.clear(gl.COLOR_BUFFER_BIT);
      const px=new Uint8Array(4);gl.readPixels(0,0,1,1,gl.RGBA,gl.UNSIGNED_BYTE,px);
      return {available:true,renderer:ext?gl.getParameter(ext.UNMASKED_RENDERER_WEBGL):gl.getParameter(gl.RENDERER),vendor:ext?gl.getParameter(ext.UNMASKED_VENDOR_WEBGL):gl.getParameter(gl.VENDOR),pixel:Array.from(px),error:gl.getError()};
    })()"#).unwrap();
    if webgl["available"]==true{
        assert_eq!(webgl["pixel"],json!([255,0,0,255]));
        assert_eq!(webgl["error"],0);
    }
    let mut screenshots=Vec::new();
    for _ in 0..12 {
        let t=Instant::now();let bytes=pane.screenshot().unwrap();
        let ms=t.elapsed().as_secs_f64()*1000.;
        let rgba=image::load_from_memory_with_format(&bytes,image::ImageFormat::Png).unwrap().to_rgba8();
        if webgl["available"]==true {
            assert!(rgba.pixels().any(|p|p.0==[255,0,0,255]),"rendered red canvas missing");
        }
        screenshots.push(ms);
    }
    // Report capabilities verbatim, never infer hardware from WebGL alone:
    // SwiftShader and ANGLE software renderers can expose WebGL.
    let gpu=&info["gpu"];
    println!("{}",json!({"schema":"herdr.chrome-gpu-probe/v1","status":"complete","chrome_version":binary.version,
      "devices":gpu.get("devices").unwrap_or(&Value::Null),"feature_status":gpu.get("featureStatus").unwrap_or(&Value::Null),
      "webgl":webgl,"screenshot_ms":screenshots,"note":"capability diagnostic only; no CPU/GPU causal comparison or GPUI presentation measurement"}));
}
#[cfg(not(target_os="macos"))]
fn main(){}
