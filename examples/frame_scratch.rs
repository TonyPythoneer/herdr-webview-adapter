#[cfg(target_os="macos")]
mod probe {
use std::time::Instant;
pub fn run(){
use base64::Engine as _;
    use std::hint::black_box;
    let engine=base64::engine::general_purpose::STANDARD;
    for size in [65536,1048576,4194304] {
        let source=vec![0x5a;size];
        let input=engine.encode(&source);
        for i in 0..6 {
            for reuse in [i%2==0,i%2!=0] {
                let mut scratch=Vec::new();
                let t=Instant::now();
                for _ in 0..50 {
                    if reuse { scratch.clear();engine.decode_vec(black_box(&input),&mut scratch).unwrap();black_box(&scratch); }
                    else {let out=engine.decode(black_box(&input)).unwrap();black_box(out);}
                }
                let ms=t.elapsed().as_secs_f64()*1000.;
                if reuse {assert_eq!(scratch,source);}
                assert_eq!(engine.decode(&input).unwrap(),source);
                println!("{}",serde_json::json!({"round":i,"bytes":size,"reuse":reuse,"batch_ms":ms,"retained_capacity":scratch.capacity()}));
            }
        }
    }
}
}
#[cfg(target_os="macos")]
fn main(){probe::run();}
#[cfg(not(target_os="macos"))]
fn main(){}
