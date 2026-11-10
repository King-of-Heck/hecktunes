use crate::identify::musicbrainz::{http_agent, RateLimiter, USER_AGENT};
use anyhow::Result;
use serde_json::Value;
use std::time::Duration;

pub fn parse_lookup(v: &Value) -> Vec<String> {
    if v["status"].as_str() != Some("ok") {
        return vec![];
    }
    let mut results: Vec<&Value> = v["results"].as_array().map(|a| a.iter().collect()).unwrap_or_default();
    results.sort_by(|a, b| b["score"].as_f64().unwrap_or(0.0).total_cmp(&a["score"].as_f64().unwrap_or(0.0)));
    let mut ids: Vec<String> = Vec::new();
    for r in results {
        for rec in r["recordings"].as_array().into_iter().flatten() {
            for rel in rec["releases"].as_array().into_iter().flatten() {
                if let Some(id) = rel["id"].as_str() {
                    if !ids.iter().any(|x| x == id) {
                        ids.push(id.to_string());
                    }
                }
            }
        }
    }
    ids
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// The fingerprint is URL-safe base64 (no escaping needed); the client key is encoded.
fn lookup_url(key: &str, duration_secs: u32, fingerprint: &str) -> String {
    format!(
        "https://api.acoustid.org/v2/lookup?client={}&meta=recordings+releaseids&duration={}&fingerprint={}",
        percent_encode(key),
        duration_secs,
        fingerprint
    )
}

pub struct AcoustId {
    agent: ureq::Agent,
    key: String,
    limiter: RateLimiter,
}

impl AcoustId {
    pub fn new(key: String) -> Self {
        Self { agent: http_agent(), key, limiter: RateLimiter::new(Duration::from_millis(350)) }
    }

    pub fn lookup(&self, fingerprint: &str, duration_secs: u32) -> Result<Vec<String>> {
        self.limiter.wait();
        let url = lookup_url(&self.key, duration_secs, fingerprint);
        let v: Value = self.agent.get(&url).header("User-Agent", USER_AGENT).call()?.body_mut().read_json()?;
        Ok(parse_lookup(&v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_ids_in_score_order_without_duplicates() {
        let v: Value = serde_json::from_str(include_str!("../../tests/fixtures/acoustid_lookup.json")).unwrap();
        assert_eq!(parse_lookup(&v), vec!["rel-A", "rel-B", "rel-C", "rel-D"]);
    }

    #[test]
    fn lookup_url_percent_encodes_the_client_key() {
        let url = lookup_url("a b&c=d#/\u{e9}", 12, "AQAA-_x");
        assert!(url.contains("client=a%20b%26c%3Dd%23%2F%C3%A9&"), "{url}");
        assert!(url.ends_with("&duration=12&fingerprint=AQAA-_x") || url.contains("duration=12&fingerprint=AQAA-_x"));
        assert_eq!(url.matches('&').count(), 3);
    }

    #[test]
    fn error_status_yields_nothing() {
        let v: Value = serde_json::json!({"status": "error", "error": {"message": "invalid API key"}});
        assert!(parse_lookup(&v).is_empty());
    }
}
