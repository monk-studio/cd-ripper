//! Tells a Navidrome server to pick up newly filed albums, through the
//! Subsonic API's `startScan`. Navidrome only lets admin users start scans.
//!
//! Auth uses Subsonic's salted token (md5 of password + salt), so the password
//! itself never goes over the wire.

use std::hash::{BuildHasher, RandomState};

use md5::{Digest, Md5};
use serde::Deserialize;

const API_VERSION: &str = "1.16.1";

pub struct Navidrome {
    url: String,
    user: String,
    password: String,
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "subsonic-response")]
    response: Response,
}

#[derive(Deserialize)]
struct Response {
    status: String,
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct ApiError {
    code: u32,
    message: String,
}

impl Navidrome {
    pub fn new(url: &str, user: &str, password: &str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_owned(),
            user: user.to_owned(),
            password: password.to_owned(),
        }
    }

    /// Queues a library scan; Navidrome runs it in the background.
    pub fn start_scan(&self) -> anyhow::Result<()> {
        // Any unpredictable salt will do; RandomState is seeded per process.
        let salt = format!(
            "{:016x}",
            RandomState::new().hash_one(std::time::SystemTime::now())
        );
        let token = hex(&Md5::digest(format!("{}{salt}", self.password)));
        let resp = ureq::get(&format!("{}/rest/startScan", self.url))
            .query("u", &self.user)
            .query("t", &token)
            .query("s", &salt)
            .query("v", API_VERSION)
            .query("c", "CDR")
            .query("f", "json")
            .call()?;
        let body: Envelope = serde_json::from_reader(resp.into_reader())?;
        check(body.response)
    }
}

/// Subsonic reports failures inside a 200 response.
fn check(r: Response) -> anyhow::Result<()> {
    if r.status == "ok" {
        return Ok(());
    }
    match r.error {
        Some(e) if e.code == 40 => anyhow::bail!("Navidrome rejected the user name or password"),
        Some(e) if e.code == 50 => anyhow::bail!("Navidrome only lets admin users start a scan"),
        Some(e) => anyhow::bail!("Navidrome error {}: {}", e.code, e.message),
        None => anyhow::bail!("Navidrome returned status {}", r.status),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> anyhow::Result<()> {
        check(serde_json::from_str::<Envelope>(json).unwrap().response)
    }

    #[test]
    fn subsonic_token_matches_spec_example() {
        // From the Subsonic API docs: password "sesame", salt "c19b2d".
        assert_eq!(
            hex(&Md5::digest("sesamec19b2d")),
            "26719a1196d2a940705a59634eb18eab"
        );
    }

    #[test]
    fn errors_inside_ok_responses_are_reported() {
        assert!(parse(r#"{"subsonic-response":{"status":"ok","version":"1.16.1"}}"#).is_ok());
        let err = parse(
            r#"{"subsonic-response":{"status":"failed","error":{"code":40,"message":"Wrong username or password"}}}"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("user name or password"), "{err}");
        let err =
            parse(r#"{"subsonic-response":{"status":"failed","error":{"code":50,"message":"x"}}}"#)
                .unwrap_err();
        assert!(err.to_string().contains("admin"), "{err}");
    }
}
