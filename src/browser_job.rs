//! Browser job state and unguessable request IDs. Owned by Adapter rather
//! than the upstream-facing AppWindow; only the human-approval UI stays in host.
use crate::chrome_cdp::Session;
use serde_json::Value;
use std::sync::{Arc, atomic::AtomicBool};

#[derive(Clone)]
pub enum BrowserJobStatus {
    Running,
    ApprovalRequired(String),
    Finished(Result<Value, String>),
}

pub struct BrowserJob {
    pub pane: String,
    pub action: String,
    pub args: Value,
    pub session: Option<Arc<Session>>,
    pub cancelled: Arc<AtomicBool>,
    pub status: BrowserJobStatus,
}

/// Request IDs are cryptographically unguessable and never sequential.
/// This prevents one local client from predicting unrelated job identifiers.
pub fn new_browser_request_id() -> Result<String, String> {
    use std::io::Read as _;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|error| format!("cannot generate browser request ID: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_ids_are_random_opaque_and_128_bit() {
        let a=new_browser_request_id().unwrap();
        let b=new_browser_request_id().unwrap();
        assert_eq!(a.len(),32);
        assert!(a.bytes().all(|v|v.is_ascii_hexdigit()));
        assert_ne!(a,b);
    }
}
