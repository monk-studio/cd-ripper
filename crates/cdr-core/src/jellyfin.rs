//! Tells a Jellyfin server to pick up newly filed albums.
//!
//! Only music libraries are rescanned, the same as "Scan library" on each of
//! them in the web UI: rescanning every library would also crawl films and
//! shows. The API key is created under Dashboard → API Keys.

use serde::Deserialize;

const CLIENT: &str = concat!(
    "MediaBrowser Client=\"CDR\", Version=\"",
    env!("CARGO_PKG_VERSION"),
    "\""
);

pub struct Jellyfin {
    url: String,
    api_key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct VirtualFolder {
    name: String,
    collection_type: Option<String>,
    item_id: String,
}

impl Jellyfin {
    pub fn new(url: &str, api_key: &str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_owned(),
            api_key: api_key.to_owned(),
        }
    }

    fn auth(&self) -> String {
        format!("{CLIENT}, Token=\"{}\"", self.api_key)
    }

    /// The server's music libraries. Fails when the server can't be reached,
    /// rejects the key, or has no music library.
    fn music(&self) -> anyhow::Result<Vec<VirtualFolder>> {
        let resp = match ureq::get(&format!("{}/Library/VirtualFolders", self.url))
            .set("Authorization", &self.auth())
            .call()
        {
            Err(ureq::Error::Status(401, _)) => {
                anyhow::bail!("Jellyfin rejected the API key (Dashboard → API Keys)")
            }
            r => r?,
        };
        let folders: Vec<VirtualFolder> = serde_json::from_reader(resp.into_reader())?;
        let music: Vec<_> = folders
            .into_iter()
            .filter(|f| f.collection_type.as_deref() == Some("music"))
            .collect();
        anyhow::ensure!(
            !music.is_empty(),
            "the Jellyfin server has no music library"
        );
        Ok(music)
    }

    /// The names of the server's music libraries, without scanning them: a
    /// check that the address and key work.
    pub fn music_libraries(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.music()?.into_iter().map(|f| f.name).collect())
    }

    /// Starts a scan of every music library and returns their names. The scan
    /// runs in the background on the server; this returns once it's queued.
    pub fn scan_music(&self) -> anyhow::Result<Vec<String>> {
        let music = self.music()?;
        for folder in &music {
            ureq::post(&format!(
                "{}/Items/{}/Refresh?Recursive=true&MetadataRefreshMode=Default&ImageRefreshMode=Default",
                self.url, folder.item_id
            ))
            .set("Authorization", &self.auth())
            .call()?;
        }
        Ok(music.into_iter().map(|f| f.name).collect())
    }
}
