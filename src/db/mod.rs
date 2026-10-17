//! All SQL lives here. The Plex DB is always opened read-only.

use crate::model::{AudioTrack, Ids, Item, Show, Sub, Video, apply_guid};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, types::Value as SqlValue};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

const MOVIE: i64 = 1;
const SHOW: i64 = 2;
const SEASON: i64 = 3;
const EPISODE: i64 = 4;
const TAG_GENRE: i64 = 1;
const TAG_DIRECTOR: i64 = 4;
const TAG_GUID: i64 = 314;

/// Columns we cannot work without. Checked at open so an incompatible Plex version fails clearly.
const REQUIRED: &[(&str, &[&str])] = &[
    ("library_sections", &["id", "name", "section_type"]),
    ("section_locations", &["library_section_id", "root_path"]),
    (
        "metadata_items",
        &[
            "id",
            "library_section_id",
            "metadata_type",
            "title",
            "title_sort",
            "original_title",
            "year",
            "index",
            "parent_id",
            "guid",
            "originally_available_at",
            "studio",
            "content_rating",
            "deleted_at",
        ],
    ),
    (
        "media_items",
        &[
            "id",
            "metadata_item_id",
            "width",
            "height",
            "container",
            "video_codec",
            "audio_codec",
            "audio_channels",
            "bitrate",
            "duration",
            "display_aspect_ratio",
            "deleted_at",
        ],
    ),
    (
        "media_parts",
        &["id", "media_item_id", "file", "size", "deleted_at"],
    ),
    (
        "media_streams",
        &[
            "id",
            "media_part_id",
            "stream_type_id",
            "codec",
            "language",
            "channels",
            "index",
            "url",
        ],
    ),
    ("tags", &["id", "tag", "tag_type"]),
    ("taggings", &["metadata_item_id", "tag_id"]),
];

/// Columns that only some Plex versions have; read as NULL when absent.
const OPTIONAL: &[(&str, &str)] = &[
    ("metadata_items", "edition_title"),
    ("media_items", "frames_per_second"),
    ("media_items", "color_trc"),
    ("media_parts", "index"),
    ("media_streams", "default"),
    ("media_streams", "forced"),
    ("media_streams", "extra_data"),
];

#[derive(Debug, Clone, serde::Serialize)]
pub struct Library {
    pub id: i64,
    pub name: String,
    /// 1 movie, 2 show, 8 music, 13 photo.
    pub section_type: i64,
    pub roots: Vec<String>,
}

pub struct Db {
    conn: Connection,
    cols: HashMap<String, HashSet<String>>,
}

/// Percent-encode the characters that would break a `file:` URI.
fn uri_path(p: &Path) -> String {
    let mut out = String::new();
    for c in p.to_string_lossy().chars() {
        match c {
            '%' | '?' | '#' | ' ' => out.push_str(&format!("%{:02X}", c as u32)),
            _ => out.push(c),
        }
    }
    out
}

pub fn open(path: &Path, immutable: bool) -> Result<Db> {
    if !path.is_file() {
        bail!("Plex database not found: {}", path.display());
    }
    // No -wal next to the file means Plex is not writing it (stopped, or a copy): immutable is safe
    // and avoids needing write access to create -shm.
    let wal = path.with_file_name(format!(
        "{}-wal",
        path.file_name().unwrap().to_string_lossy()
    ));
    let immutable = immutable || !wal.exists();
    let uri = format!(
        "file:{}?mode=ro{}",
        uri_path(path),
        if immutable { "&immutable=1" } else { "" }
    );
    let conn = Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {uri}"))?;
    let db = Db {
        cols: introspect(&conn)?,
        conn,
    };
    db.check_compat()?;
    Ok(db)
}

fn introspect(conn: &Connection) -> Result<HashMap<String, HashSet<String>>> {
    let mut out = HashMap::new();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for t in tables {
        let cols: HashSet<String> = conn
            .prepare(&format!(
                "PRAGMA table_info(\"{}\")",
                t.replace('"', "\"\"")
            ))?
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        out.insert(t, cols);
    }
    Ok(out)
}

impl Db {
    #[cfg(test)]
    pub fn from_conn(conn: Connection) -> Result<Db> {
        let db = Db {
            cols: introspect(&conn)?,
            conn,
        };
        db.check_compat()?;
        Ok(db)
    }

    /// Fails with the full list of missing tables/columns.
    pub fn check_compat(&self) -> Result<()> {
        let mut missing = vec![];
        for (table, cols) in REQUIRED {
            match self.cols.get(*table) {
                None => missing.push(format!("table {table}")),
                Some(have) => missing.extend(
                    cols.iter()
                        .filter(|c| !have.contains(**c))
                        .map(|c| format!("{table}.{c}")),
                ),
            }
        }
        if !missing.is_empty() {
            bail!(
                "unsupported Plex database schema, missing: {}. Please report your Plex version.",
                missing.join(", ")
            );
        }
        Ok(())
    }

    /// `alias."col"` if the column exists, else NULL.
    fn opt(&self, table: &str, alias: &str, col: &str) -> String {
        debug_assert!(OPTIONAL.contains(&(table, col)));
        if self.cols.get(table).is_some_and(|c| c.contains(col)) {
            format!("{alias}.\"{col}\"")
        } else {
            "NULL".into()
        }
    }

    pub fn libraries(&self) -> Result<Vec<Library>> {
        let mut roots: HashMap<i64, Vec<String>> = HashMap::new();
        let mut st = self
            .conn
            .prepare("SELECT library_section_id, root_path FROM section_locations ORDER BY id")?;
        for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
            let (id, p) = r?;
            roots.entry(id).or_default().push(p);
        }
        let mut st = self
            .conn
            .prepare("SELECT id, name, section_type FROM library_sections ORDER BY id")?;
        let libs = st
            .query_map([], |r| {
                let id: i64 = r.get(0)?;
                Ok(Library {
                    id,
                    name: r.get(1)?,
                    section_type: r.get(2)?,
                    roots: roots.remove(&id).unwrap_or_default(),
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(libs)
    }

    /// One `Item` per media file, for the given sections (all movie/show sections if empty).
    /// Paths are Plex paths; `path`/`root` are filled by the caller after path_map.
    pub fn items(&self, sections: &[i64]) -> Result<Vec<Item>> {
        let libs: BTreeMap<i64, Library> = self
            .libraries()?
            .into_iter()
            .filter(|l| matches!(l.section_type, 1 | 2))
            .filter(|l| sections.is_empty() || sections.contains(&l.id))
            .map(|l| (l.id, l))
            .collect();
        if libs.is_empty() {
            return Ok(vec![]);
        }
        let in_sections = libs
            .keys()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");

        // metadata
        let sql = format!(
            "SELECT id, library_section_id, metadata_type, title, title_sort, original_title, year, \"index\",
                    parent_id, guid, originally_available_at, studio, content_rating, {}
             FROM metadata_items m
             WHERE deleted_at IS NULL AND metadata_type IN ({MOVIE},{SHOW},{SEASON},{EPISODE})
               AND library_section_id IN ({in_sections})",
            self.opt("metadata_items", "m", "edition_title")
        );
        let mut metas: HashMap<i64, Meta> = HashMap::new();
        let mut st = self.conn.prepare(&sql)?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            metas.insert(
                id,
                Meta {
                    id,
                    section: r.get(1)?,
                    mtype: r.get(2)?,
                    title: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    title_sort: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    original_title: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    year: r.get(6)?,
                    index: r.get(7)?,
                    parent: r.get(8)?,
                    guid: r.get::<_, Option<String>>(9)?.unwrap_or_default(),
                    aired: date_of(r.get::<_, SqlValue>(10)?),
                    studio: r.get::<_, Option<String>>(11)?.unwrap_or_default(),
                    content_rating: r.get::<_, Option<String>>(12)?.unwrap_or_default(),
                    edition: r.get::<_, Option<String>>(13)?.filter(|s| !s.is_empty()),
                    tags: vec![],
                },
            );
        }

        // tags (genres, directors, external guids)
        let mut st = self.conn.prepare(&format!(
            "SELECT tg.metadata_item_id, t.tag_type, t.tag FROM taggings tg JOIN tags t ON t.id = tg.tag_id
             WHERE t.tag_type IN ({TAG_GENRE},{TAG_DIRECTOR},{TAG_GUID}) ORDER BY tg.metadata_item_id, tg.\"index\", tg.id"
        ))?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            if let Some(m) = metas.get_mut(&r.get::<_, i64>(0)?) {
                m.tags.push((
                    r.get(1)?,
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ));
            }
        }

        // streams, by part
        let sql = format!(
            "SELECT s.media_part_id, s.stream_type_id, s.codec, s.language, s.channels, s.url, {}, {}, {}
             FROM media_streams s WHERE s.stream_type_id IN (1,2,3) ORDER BY s.media_part_id, s.\"index\" IS NULL, s.\"index\", s.id",
            self.opt("media_streams", "s", "default"),
            self.opt("media_streams", "s", "forced"),
            self.opt("media_streams", "s", "extra_data"),
        );
        let mut streams: HashMap<i64, Vec<Stream>> = HashMap::new();
        let mut st = self.conn.prepare(&sql)?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            let Some(part) = r.get::<_, Option<i64>>(0)? else {
                continue;
            };
            streams.entry(part).or_default().push(Stream {
                kind: r.get(1)?,
                codec: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                language: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                channels: r.get::<_, Option<i64>>(4)?.unwrap_or(0) as u32,
                url: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                default: truthy(r.get::<_, SqlValue>(6)?),
                forced: truthy(r.get::<_, SqlValue>(7)?),
                extra: parse_extra(&r.get::<_, Option<String>>(8)?.unwrap_or_default()),
            });
        }

        // media items + parts
        let sql = format!(
            "SELECT mi.id, mi.metadata_item_id, mi.width, mi.height, mi.container, mi.video_codec, mi.bitrate,
                    mi.duration, mi.display_aspect_ratio, {}, {}, mp.id, mp.file, mp.size
             FROM media_items mi JOIN media_parts mp ON mp.media_item_id = mi.id
             WHERE mi.deleted_at IS NULL AND mp.deleted_at IS NULL AND mp.file IS NOT NULL AND mp.file != ''
             ORDER BY mi.metadata_item_id, mi.id, {} , mp.id",
            self.opt("media_items", "mi", "frames_per_second"),
            self.opt("media_items", "mi", "color_trc"),
            self.opt("media_parts", "mp", "index"),
        );
        let mut parts: Vec<PartRow> = vec![];
        let mut st = self.conn.prepare(&sql)?;
        let mut rows = st.query([])?;
        while let Some(r) = rows.next()? {
            parts.push(PartRow {
                media_id: r.get(0)?,
                meta_id: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                width: r.get::<_, Option<i64>>(2)?.unwrap_or(0) as u32,
                height: r.get::<_, Option<i64>>(3)?.unwrap_or(0) as u32,
                container: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                vcodec: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                bitrate: r.get::<_, Option<i64>>(6)?.unwrap_or(0).max(0) as u64,
                duration: r.get::<_, Option<i64>>(7)?.unwrap_or(0).max(0) as u64,
                aspect: r.get(8)?,
                fps: r.get(9)?,
                color_trc: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
                part_id: r.get(11)?,
                file: r.get(12)?,
                size: r.get::<_, Option<i64>>(13)?.unwrap_or(0).max(0) as u64,
            });
        }

        // versions / parts counts per metadata item
        let mut versions: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut parts_of: HashMap<i64, Vec<i64>> = HashMap::new();
        for p in &parts {
            let v = versions.entry(p.meta_id).or_default();
            if !v.contains(&p.media_id) {
                v.push(p.media_id);
            }
            parts_of.entry(p.media_id).or_default().push(p.part_id);
        }

        let mut items: Vec<Item> = vec![];
        let mut by_file: HashMap<String, usize> = HashMap::new();
        for p in &parts {
            let Some(m) = metas.get(&p.meta_id) else {
                continue;
            };
            if m.mtype != MOVIE && m.mtype != EPISODE {
                continue;
            }
            let lib = &libs[&m.section];
            let vers = &versions[&p.meta_id];
            let prts = &parts_of[&p.media_id];
            let mut it = Item {
                kind: if m.mtype == MOVIE { "movie" } else { "episode" }.into(),
                library: lib.name.clone(),
                library_id: lib.id,
                ids: m.ids(),
                title: m.title.clone(),
                original_title: m.original_title.clone(),
                sort_title: if m.title_sort.is_empty() {
                    m.title.clone()
                } else {
                    m.title_sort.clone()
                },
                year: m.year,
                edition: m.edition.clone(),
                ext: Path::new(&p.file)
                    .extension()
                    .map(|e| e.to_string_lossy().to_string())
                    .unwrap_or_default(),
                container: p.container.clone(),
                part: prts.iter().position(|x| *x == p.part_id).unwrap() as u32 + 1,
                parts_total: prts.len() as u32,
                version: vers.iter().position(|x| *x == p.media_id).unwrap() as u32 + 1,
                versions_total: vers.len() as u32,
                size: p.size,
                bitrate: p.bitrate,
                duration: p.duration,
                genres: m.tag_list(TAG_GENRE),
                directors: m.tag_list(TAG_DIRECTOR),
                studio: m.studio.clone(),
                content_rating: m.content_rating.clone(),
                path: p.file.clone(),
                plex_path: p.file.clone(),
                video: Video {
                    codec: p.vcodec.clone(),
                    width: p.width,
                    height: p.height,
                    fps: p
                        .fps
                        .filter(|f| *f > 0.0)
                        .map(|f| (f * 1000.0).round() / 1000.0),
                    aspect: p.aspect.filter(|a| *a > 0.0),
                    ..Default::default()
                },
                ..Default::default()
            };
            fill_streams(
                &mut it,
                streams.get(&p.part_id).map(|v| v.as_slice()).unwrap_or(&[]),
                &p.color_trc,
            );
            if m.mtype == EPISODE {
                let season = m.parent.and_then(|id| metas.get(&id));
                let show = season.and_then(|s| s.parent).and_then(|id| metas.get(&id));
                it.season = season.and_then(|s| s.index);
                it.episode = m.index;
                it.episode_title = Some(m.title.clone());
                it.air_date = m.aired.clone();
                if let Some(s) = show {
                    it.show = Some(Show {
                        title: s.title.clone(),
                        year: s.year,
                        ids: s.ids(),
                    });
                    it.genres = s.tag_list(TAG_GENRE);
                    if it.studio.is_empty() {
                        it.studio = s.studio.clone();
                    }
                    if it.content_rating.is_empty() {
                        it.content_rating = s.content_rating.clone();
                    }
                }
                // Several episodes in one file: Plex lists the same file under each episode.
                if let Some(&i) = by_file.get(&p.file) {
                    let first = &mut items[i];
                    let (a, b) = (first.episode.unwrap_or(0), it.episode.unwrap_or(0));
                    first.episode = Some(a.min(b));
                    first.episode_end = Some(first.episode_end.unwrap_or(a).max(b));
                    continue;
                }
                by_file.insert(p.file.clone(), items.len());
            }
            items.push(it);
        }
        Ok(items)
    }
}

struct Meta {
    id: i64,
    section: i64,
    mtype: i64,
    title: String,
    title_sort: String,
    original_title: String,
    year: Option<i64>,
    index: Option<i64>,
    parent: Option<i64>,
    guid: String,
    aired: Option<String>,
    studio: String,
    content_rating: String,
    edition: Option<String>,
    tags: Vec<(i64, String)>,
}

impl Meta {
    fn ids(&self) -> Ids {
        let mut ids = Ids {
            plex: self.id,
            ..Default::default()
        };
        if !self.guid.is_empty() {
            ids.plex_guid = Some(self.guid.clone());
        }
        for (t, tag) in &self.tags {
            if *t == TAG_GUID {
                apply_guid(&mut ids, tag);
            }
        }
        apply_guid(&mut ids, &self.guid);
        ids
    }

    fn tag_list(&self, kind: i64) -> Vec<String> {
        self.tags
            .iter()
            .filter(|(t, _)| *t == kind)
            .map(|(_, s)| s.clone())
            .collect()
    }
}

struct PartRow {
    media_id: i64,
    meta_id: i64,
    width: u32,
    height: u32,
    container: String,
    vcodec: String,
    bitrate: u64,
    duration: u64,
    aspect: Option<f64>,
    fps: Option<f64>,
    color_trc: String,
    part_id: i64,
    file: String,
    size: u64,
}

struct Stream {
    kind: i64,
    codec: String,
    language: String,
    channels: u32,
    url: String,
    default: bool,
    forced: bool,
    extra: HashMap<String, String>,
}

impl Stream {
    fn x(&self, k: &str) -> &str {
        self.extra
            .get(&format!("ma:{k}"))
            .or(self.extra.get(k))
            .map(|s| s.as_str())
            .unwrap_or("")
    }
}

fn fill_streams(it: &mut Item, streams: &[Stream], color_trc: &str) {
    let mut seen_video = false;
    for s in streams {
        match s.kind {
            1 if !seen_video => {
                seen_video = true;
                if it.video.codec.is_empty() {
                    it.video.codec = s.codec.clone();
                }
                it.video.bit_depth = s.x("bitDepth").parse().ok();
                let trc = if s.x("colorTrc").is_empty() {
                    color_trc
                } else {
                    s.x("colorTrc")
                };
                it.video.hdr = if s.x("DOVIPresent") == "1" {
                    "DV".into()
                } else {
                    match trc {
                        "smpte2084" => "HDR10".into(),
                        "arib-std-b67" => "HLG".into(),
                        _ => String::new(),
                    }
                };
            }
            2 => it.audio.push(AudioTrack {
                codec: s.codec.clone(),
                channels: s.channels,
                language: s.language.clone(),
                title: s.x("title").to_string(),
                profile: s.x("profile").to_string(),
                default: s.default,
                ..Default::default()
            }),
            3 => {
                let external = !s.url.is_empty();
                it.subs.push(Sub {
                    language: s.language.clone(),
                    codec: s.codec.clone(),
                    title: s.x("title").to_string(),
                    forced: s.forced || s.x("forced") == "1",
                    default: s.default,
                    external,
                    path: external
                        .then(|| s.url.strip_prefix("file://").unwrap_or(&s.url).to_string()),
                })
            }
            _ => {}
        }
    }
}

fn truthy(v: SqlValue) -> bool {
    match v {
        SqlValue::Integer(i) => i != 0,
        SqlValue::Text(t) => matches!(t.as_str(), "1" | "t" | "true"),
        _ => false,
    }
}

/// `extra_data` is JSON in recent Plex, url-encoded (`a=b&c=d`) in older ones.
pub fn parse_extra(s: &str) -> HashMap<String, String> {
    let s = s.trim();
    if s.starts_with('{') {
        if let Ok(serde_json::Value::Object(o)) = serde_json::from_str::<serde_json::Value>(s) {
            return o
                .into_iter()
                .map(|(k, v)| {
                    (
                        k,
                        v.as_str()
                            .map(String::from)
                            .unwrap_or_else(|| v.to_string()),
                    )
                })
                .collect();
        }
        return HashMap::new();
    }
    s.split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (pct_decode(k), pct_decode(v)))
        .collect()
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `originally_available_at` is a unix timestamp in recent Plex, "YYYY-MM-DD hh:mm:ss" in older ones.
fn date_of(v: SqlValue) -> Option<String> {
    match v {
        SqlValue::Integer(ts) => {
            let (y, m, d) = civil_from_days(ts.div_euclid(86_400));
            Some(format!("{y:04}-{m:02}-{d:02}"))
        }
        SqlValue::Text(t) if t.len() >= 10 => Some(t[..10].to_string()),
        _ => None,
    }
}

/// Howard Hinnant's days-to-civil.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_data_forms() {
        let j = parse_extra(r#"{"ma:bitDepth":"10","ma:DOVIPresent":"1","n":8}"#);
        assert_eq!(j["ma:bitDepth"], "10");
        assert_eq!(j["n"], "8");
        let u = parse_extra("ma%3AbitDepth=10&ma%3Atitle=Commentaire%20r%C3%A9al");
        assert_eq!(u["ma:bitDepth"], "10");
        assert_eq!(u["ma:title"], "Commentaire réal");
        assert_eq!(pct_decode("100%"), "100%");
    }

    #[test]
    fn dates() {
        assert_eq!(
            date_of(SqlValue::Integer(988_156_800)).as_deref(),
            Some("2001-04-25")
        );
        assert_eq!(
            date_of(SqlValue::Text("2001-04-25 00:00:00".into())).as_deref(),
            Some("2001-04-25")
        );
    }

    #[test]
    fn compat_reports_missing_columns() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../../tests/fixtures/plex_schema.sql"))
            .unwrap();
        assert!(Db::from_conn(c).is_ok());

        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(include_str!("../../tests/fixtures/plex_schema.sql"))
            .unwrap();
        c.execute_batch("DROP INDEX index_media_parts_on_size; ALTER TABLE media_parts DROP COLUMN size; DROP TABLE tags;").unwrap();
        let err = Db::from_conn(c).err().unwrap().to_string();
        assert!(
            err.contains("media_parts.size") && err.contains("table tags"),
            "{err}"
        );
    }
}
