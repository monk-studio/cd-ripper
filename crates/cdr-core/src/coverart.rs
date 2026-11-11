//! Front covers from the Cover Art Archive (https://musicbrainz.org/doc/Cover_Art_Archive/API).

use std::io::Read;

use crate::USER_AGENT;
use crate::musicbrainz::Release;

const API: &str = "https://coverartarchive.org";
/// Largest pre-sized thumbnail; originals can be tens of megabytes.
const SIZE: u32 = 1200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cover {
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// The release's front cover, else its release group's. `Ok(None)` if neither has one.
pub fn fetch(release: &Release) -> anyhow::Result<Option<Cover>> {
    let mut urls = vec![format!("{API}/release/{}/front-{SIZE}", release.id)];
    if let Some(group) = &release.release_group_id {
        urls.push(format!("{API}/release-group/{group}/front-{SIZE}"));
    }
    for url in urls {
        match ureq::get(&url).set("User-Agent", USER_AGENT).call() {
            Ok(resp) => {
                let mime_type = resp.content_type().to_owned();
                let mut data = Vec::new();
                resp.into_reader().read_to_end(&mut data)?;
                return Ok(Some(Cover { mime_type, data }));
            }
            Err(ureq::Error::Status(404, _)) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(None)
}
