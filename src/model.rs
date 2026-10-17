//! Data model exposed to templates. Built by `db`, finished by `Item::finalize`.
//! `None` fields are skipped on serialization so templates see them as undefined.

use crate::mapping::Mapping;
use serde::Serialize;

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Ids {
    pub plex: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plex_guid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmdb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub imdb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tvdb: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Video {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub resolution: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fps: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aspect: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bit_depth: Option<u32>,
    /// HDR10, DV, HLG or empty.
    pub hdr: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AudioTrack {
    pub codec: String,
    pub channels: u32,
    pub channels_label: String,
    pub language: String,
    pub title: String,
    /// Codec profile from Plex analysis (e.g. "ma" for DTS-HD MA).
    pub profile: String,
    pub default: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Sub {
    pub language: String,
    pub codec: String,
    pub title: String,
    pub forced: bool,
    pub default: bool,
    pub external: bool,
    /// Host path of an external subtitle file (after path_map).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Show {
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    pub ids: Ids,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Item {
    /// "movie" or "episode".
    pub kind: String,
    pub library: String,
    pub library_id: i64,
    pub ids: Ids,
    pub title: String,
    pub original_title: String,
    pub sort_title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edition: Option<String>,
    pub ext: String,
    pub container: String,
    pub video: Video,
    pub audio: Vec<AudioTrack>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_main: Option<AudioTrack>,
    pub subs: Vec<Sub>,
    pub part: u32,
    pub parts_total: u32,
    pub version: u32,
    pub versions_total: u32,
    /// Bytes, from media_parts.size.
    pub size: u64,
    /// kbps.
    pub bitrate: u64,
    /// Milliseconds.
    pub duration: u64,
    pub duration_min: u64,
    pub genres: Vec<String>,
    pub directors: Vec<String>,
    pub studio: String,
    pub content_rating: String,
    // Episodes only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show: Option<Show>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<i64>,
    /// Last episode when one file holds several (S01E01-E02).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_end: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub air_date: Option<String>,
    /// Current file on the host (after path_map).
    pub path: String,
    /// File path as Plex stores it.
    pub plex_path: String,
    /// Library root containing `path` (after path_map), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

impl Item {
    /// Fill the fields that depend on the mapping tables.
    pub fn finalize(&mut self, m: &Mapping) {
        self.video.resolution = m.resolution(self.video.width, self.video.height);
        for a in &mut self.audio {
            a.channels_label = m.channels_label(a.channels);
        }
        self.audio_main = self
            .audio
            .iter()
            .find(|a| a.default)
            .or(self.audio.first())
            .cloned();
        self.duration_min = (self.duration + 30_000) / 60_000;
    }
}

/// Parse a Plex guid (`plex://`, legacy agents, or tag form `tmdb://194`) into `ids`.
pub fn apply_guid(ids: &mut Ids, guid: &str) {
    let (scheme, rest) = match guid.split_once("://") {
        Some(x) => x,
        None => return,
    };
    let value = rest.split(['?', '/']).next().unwrap_or("").to_string();
    if value.is_empty() {
        return;
    }
    let slot = match scheme {
        "tmdb" | "com.plexapp.agents.themoviedb" => &mut ids.tmdb,
        "imdb" | "com.plexapp.agents.imdb" => &mut ids.imdb,
        "tvdb" | "com.plexapp.agents.thetvdb" => &mut ids.tvdb,
        _ => return,
    };
    // Legacy imdb agent sometimes stores the tmdb id; keep imdb ids only if they look like one.
    if scheme == "com.plexapp.agents.imdb" && !value.starts_with("tt") {
        return;
    }
    slot.get_or_insert(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids() {
        let mut ids = Ids::default();
        apply_guid(&mut ids, "tmdb://194");
        apply_guid(&mut ids, "imdb://tt0211915");
        apply_guid(&mut ids, "com.plexapp.agents.thetvdb://12345/1/2?lang=en");
        apply_guid(&mut ids, "plex://movie/5d776825880197001ec967c4");
        apply_guid(&mut ids, "tmdb://999"); // first wins
        assert_eq!(ids.tmdb.as_deref(), Some("194"));
        assert_eq!(ids.imdb.as_deref(), Some("tt0211915"));
        assert_eq!(ids.tvdb.as_deref(), Some("12345"));
    }
}
