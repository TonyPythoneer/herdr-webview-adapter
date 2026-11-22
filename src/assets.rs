//! Browser-specific frontend assets are owned by this adapter, not duplicated
//! as a second editable source in the upstream-derived application.
pub const BROWSER_URL_JS: &str = include_str!("../assets/browser-url/main.js");
pub const BROWSER_URL_MOD_JSON: &str = include_str!("../assets/browser-url/mod.json");

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn browser_url_mod_retains_expected_command_wiring() {
        assert!(BROWSER_URL_JS.contains("run(p.command.name"));
        assert!(BROWSER_URL_JS.contains("url: this.value"));
        assert_eq!(serde_json::from_str::<serde_json::Value>(BROWSER_URL_MOD_JSON).unwrap()["place"], "none");
    }
}
