//! Configuration home resolution matches upstream Herdr's mods::app_dir.
use std::path::PathBuf;
pub fn app_dir() -> PathBuf {
    let env = |key: &str| std::env::var_os(key)
        .filter(|value| !value.is_empty()).map(PathBuf::from);
    env("HERDR_APP_HOME").unwrap_or_else(|| {
        let base = env("XDG_CONFIG_HOME").unwrap_or_else(|| {
            std::env::home_dir().unwrap_or_else(|| "/".into()).join(".config")
        });
        base.join("herdr-app")
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_path_suffix_matches_upstream() {
        if std::env::var_os("HERDR_APP_HOME").is_none() {
            assert!(app_dir().ends_with("herdr-app"));
        }
    }
}
