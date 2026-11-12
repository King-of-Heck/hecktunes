use crate::device::echosync::ECHO_SYNC_VERSION;
use serde::Serialize;
use std::path::Path;

pub const ECHO_SYNC_SOURCE: &str = "https://github.com/BasilSkyWalk/echopod/tree/v0.11.1";

#[derive(Debug, Clone, Serialize)]
pub struct AboutInfo {
    pub version: String,
    pub echo_sync_version: String,
    pub echo_sync_source: String,
    pub echo_sync_license: Option<String>,
    pub third_party_notices: Option<String>,
    /// HeckTunes' own licence; `None` until the owner chooses one.
    pub hecktunes_license: Option<String>,
}

/// Reads the bundled licence files from `<resource dir>/licenses/`. Missing files give `None`.
pub fn about(resource_dir: Option<&Path>) -> AboutInfo {
    let read = |name: &str| resource_dir.and_then(|d| std::fs::read_to_string(d.join("licenses").join(name)).ok());
    AboutInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        echo_sync_version: ECHO_SYNC_VERSION.to_string(),
        echo_sync_source: ECHO_SYNC_SOURCE.to_string(),
        echo_sync_license: read("echo-sync-LICENSE.txt"),
        third_party_notices: read("THIRD_PARTY_NOTICES.md"),
        hecktunes_license: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bundled_licences_and_tolerates_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("licenses")).unwrap();
        std::fs::write(dir.path().join("licenses").join("echo-sync-LICENSE.txt"), "GPL text").unwrap();
        let a = about(Some(dir.path()));
        assert_eq!(a.echo_sync_license.as_deref(), Some("GPL text"));
        assert_eq!(a.third_party_notices, None);
        assert_eq!(a.echo_sync_version, "0.11.1");
        assert!(a.echo_sync_source.ends_with("/tree/v0.11.1"));
        assert_eq!(about(None).echo_sync_license, None);
    }
}
