//! Test fixture: a Plex DB with the real PMS 1.43 schema, plus the matching files on disk.
//! Plex paths live under /data; tests map them to the tempdir with path_map.
#![allow(dead_code, clippy::too_many_arguments)]

use rusqlite::{Connection, params};
use std::fs;
use std::path::{Path, PathBuf};

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub db: PathBuf,
}

impl Fixture {
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Config with path_map to the tempdir, plus `extra` TOML merged on top.
    pub fn config(&self, extra: &str) -> plexfilerenamer::config::Config {
        plexfilerenamer::config::Config::parse(&self.config_toml(extra)).unwrap()
    }

    pub fn config_toml(&self, extra: &str) -> String {
        format!(
            "path_map = [{{ from = \"/data\", to = \"{}\" }}]\n{extra}",
            self.root().join("data").display()
        )
    }

    /// Relative listing of every file under the tempdir (excluding the db itself).
    pub fn tree(&self) -> Vec<String> {
        let mut out = vec![];
        walk(&self.root().join("data"), self.root(), &mut out);
        out.sort();
        out
    }

    /// Replace the tempdir prefix so snapshots are stable.
    pub fn redact(&self, s: &str) -> String {
        let r = self.root().display().to_string();
        s.replace(&r, "<TMP>")
            .replace(&r.replace('/', "\\"), "<TMP>")
    }
}

fn walk(d: &Path, base: &Path, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(d) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let rel = p.strip_prefix(base).unwrap().display().to_string();
        if p.is_dir() {
            out.push(format!("{rel}/"));
            walk(&p, base, out);
        } else {
            out.push(format!("{rel} ({}b)", fs::metadata(&p).unwrap().len()));
        }
    }
}

struct B<'a> {
    c: &'a Connection,
    root: &'a Path,
    tag_ids: std::collections::HashMap<(i64, String), i64>,
}

impl B<'_> {
    fn meta(
        &self,
        id: i64,
        section: i64,
        mtype: i64,
        parent: Option<i64>,
        index: Option<i64>,
        title: &str,
        year: Option<i64>,
        guid: &str,
    ) {
        self.c
            .execute(
                "INSERT INTO metadata_items (id, library_section_id, metadata_type, parent_id, \"index\", title, title_sort, original_title, year, guid, studio, content_rating, originally_available_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, '', ?7, ?8, 'Studio', 'PG', ?9)",
                params![id, section, mtype, parent, index, title, year, guid, year.map(|y| (y - 1970) * 31_556_952 + 86_400 * 120)],
            )
            .unwrap();
    }

    fn tag(&mut self, meta: i64, tag_type: i64, tag: &str) {
        let n = self.tag_ids.len() as i64 + 1;
        let id = *self.tag_ids.entry((tag_type, tag.to_string())).or_insert(n);
        self.c
            .execute(
                "INSERT OR IGNORE INTO tags (id, tag, tag_type) VALUES (?1, ?2, ?3)",
                params![id, tag, tag_type],
            )
            .unwrap();
        self.c
            .execute(
                "INSERT INTO taggings (metadata_item_id, tag_id, \"index\") VALUES (?1, ?2, 0)",
                params![meta, id],
            )
            .unwrap();
    }

    /// Creates the media item; returns its id.
    fn media(
        &self,
        meta: i64,
        w: i64,
        h: i64,
        container: &str,
        vcodec: &str,
        acodec: &str,
        ch: i64,
    ) -> i64 {
        self.c
            .execute(
                "INSERT INTO media_items (metadata_item_id, width, height, container, video_codec, audio_codec, audio_channels, bitrate, duration, display_aspect_ratio, frames_per_second)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 6000, 7_260_000, 1.78, 25.0)",
                params![meta, w, h, container, vcodec, acodec, ch],
            )
            .unwrap();
        self.c.last_insert_rowid()
    }

    /// Creates the part and the file on disk (`size` bytes); returns the part id.
    fn part(&self, media: i64, index: i64, plex_path: &str, size: usize) -> i64 {
        let host = self.host(plex_path);
        fs::create_dir_all(host.parent().unwrap()).unwrap();
        fs::write(&host, vec![b'x'; size]).unwrap();
        self.c
            .execute("INSERT INTO media_parts (media_item_id, \"index\", file, size) VALUES (?1, ?2, ?3, ?4)", params![media, index, plex_path, size as i64])
            .unwrap();
        self.c.last_insert_rowid()
    }

    fn stream(
        &self,
        part: i64,
        kind: i64,
        codec: &str,
        lang: &str,
        ch: i64,
        default: bool,
        forced: bool,
        url: &str,
        extra: &str,
    ) {
        self.c
            .execute(
                "INSERT INTO media_streams (media_part_id, stream_type_id, codec, language, channels, \"default\", forced, url, \"index\", extra_data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, (SELECT COUNT(*) FROM media_streams WHERE media_part_id = ?1), ?9)",
                params![part, kind, codec, lang, ch, default, forced, url, extra],
            )
            .unwrap();
    }

    fn host(&self, plex_path: &str) -> PathBuf {
        self.root.join(plex_path.trim_start_matches('/'))
    }

    /// A file next to the media that Plex doesn't index (poster, nfo…).
    fn file(&self, plex_path: &str, size: usize) {
        let h = self.host(plex_path);
        fs::create_dir_all(h.parent().unwrap()).unwrap();
        fs::write(h, vec![b's'; size]).unwrap();
    }
}

pub fn build() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("com.plexapp.plugins.library.db");
    let c = Connection::open(&db).unwrap();
    c.execute_batch(include_str!("../fixtures/plex_schema.sql"))
        .unwrap();
    let mut b = B {
        c: &c,
        root: dir.path(),
        tag_ids: Default::default(),
    };

    c.execute_batch(
        "INSERT INTO library_sections (id, name, section_type) VALUES (1, 'Films', 1), (2, 'Séries', 2), (3, 'Musique', 8);
         INSERT INTO section_locations (library_section_id, root_path) VALUES (1, '/data/movies'), (2, '/data/tv'), (3, '/data/music');",
    )
    .unwrap();

    // Amélie: DVD, two audio tracks, external French srt, poster + nfo sidecars. Modern agent (tag guids).
    b.meta(
        101,
        1,
        1,
        None,
        None,
        "Le Fabuleux Destin d'Amélie Poulain",
        Some(2001),
        "plex://movie/5d776825880197001ec967c4",
    );
    b.tag(101, 314, "tmdb://194");
    b.tag(101, 314, "imdb://tt0211915");
    b.tag(101, 1, "Comédie");
    b.tag(101, 1, "Romance");
    b.tag(101, 4, "Jean-Pierre Jeunet");
    let m = b.media(101, 720, 576, "mkv", "mpeg2video", "ac3", 6);
    let p = b.part(m, 0, "/data/movies/amelie/Amelie.DVDRip.mkv", 1000);
    b.stream(
        p,
        1,
        "mpeg2video",
        "",
        0,
        true,
        false,
        "",
        r#"{"ma:bitDepth":"8","ma:colorTrc":"bt470bg"}"#,
    );
    b.stream(
        p,
        2,
        "ac3",
        "fre",
        6,
        true,
        false,
        "",
        r#"{"ma:title":"Français 5.1"}"#,
    );
    b.stream(p, 2, "ac3", "eng", 2, false, false, "", "");
    b.stream(
        p,
        3,
        "srt",
        "fre",
        0,
        false,
        false,
        "file:///data/movies/amelie/Amelie.DVDRip.fr.srt",
        "",
    );
    b.file("/data/movies/amelie/Amelie.DVDRip.fr.srt", 10);
    b.file("/data/movies/amelie/Amelie.DVDRip.nfo", 11);
    b.file("/data/movies/amelie/poster.jpg", 12);

    // Ocean's Eleven: two parts (CD1/CD2), legacy imdb agent guid.
    b.meta(
        102,
        1,
        1,
        None,
        None,
        "Ocean's Eleven",
        Some(2001),
        "com.plexapp.agents.imdb://tt0240772?lang=en",
    );
    let m = b.media(102, 640, 272, "avi", "mpeg4", "mp3", 2);
    for (i, f) in [
        "/data/movies/Oceans 11/oceans.cd1.avi",
        "/data/movies/Oceans 11/oceans.cd2.avi",
    ]
    .iter()
    .enumerate()
    {
        let p = b.part(m, i as i64, f, 500 + i);
        b.stream(p, 1, "mpeg4", "", 0, true, false, "", "");
        b.stream(p, 2, "mp3", "eng", 2, true, false, "", "");
    }

    // Star Wars: two versions in one folder (UHD Dolby Vision + DVD), colon in title, url-encoded extra_data.
    b.meta(
        103,
        1,
        1,
        None,
        None,
        "Star Wars: Episode IV - A New Hope",
        Some(1977),
        "plex://movie/5d7768",
    );
    b.tag(103, 314, "tmdb://11");
    let m = b.media(103, 3840, 1600, "mkv", "hevc", "truehd", 8);
    let p = b.part(m, 0, "/data/movies/sw/starwars.uhd.mkv", 2000);
    b.stream(
        p,
        1,
        "hevc",
        "",
        0,
        true,
        false,
        "",
        "ma%3AbitDepth=10&ma%3ADOVIPresent=1&ma%3AcolorTrc=smpte2084",
    );
    b.stream(p, 2, "truehd", "eng", 8, true, false, "", "");
    b.stream(p, 2, "ac3", "fre", 6, false, false, "", "");
    b.stream(p, 3, "pgs", "fre", 0, false, true, "", "");
    let m = b.media(103, 720, 480, "mkv", "mpeg2video", "ac3", 2);
    let p = b.part(m, 0, "/data/movies/sw/starwars.dvd.mkv", 900);
    b.stream(p, 1, "mpeg2video", "", 0, true, false, "", "");
    b.stream(p, 2, "ac3", "eng", 2, true, false, "", "");

    // Face/Off: slash in title, forced VobSub idx/sub pair.
    b.meta(
        104,
        1,
        1,
        None,
        None,
        "Face/Off",
        Some(1997),
        "plex://movie/x",
    );
    b.tag(104, 314, "tmdb://754");
    let m = b.media(104, 1920, 1080, "mkv", "h264", "dca", 6);
    let p = b.part(m, 0, "/data/movies/faceoff.mkv", 700);
    b.stream(p, 1, "h264", "", 0, true, false, "", "");
    b.stream(p, 2, "dca", "fre", 6, true, false, "", "");
    b.stream(
        p,
        3,
        "vobsub",
        "fre",
        0,
        false,
        true,
        "file:///data/movies/faceoff.forced.idx",
        "",
    );
    b.file("/data/movies/faceoff.forced.idx", 20);
    b.file("/data/movies/faceoff.forced.sub", 21);

    // A movie whose file is outside every library root: must be skipped.
    b.meta(
        105,
        1,
        1,
        None,
        None,
        "Le Dîner de cons",
        Some(1998),
        "plex://movie/y",
    );
    let m = b.media(105, 720, 576, "avi", "mpeg4", "mp3", 2);
    b.part(m, 0, "/data/elsewhere/diner.avi", 300);

    // A deleted media item: invisible.
    b.meta(106, 1, 1, None, None, "Ghost", Some(1990), "plex://movie/z");
    let m = b.media(106, 720, 576, "avi", "mpeg4", "mp3", 2);
    c.execute("UPDATE media_items SET deleted_at = 1 WHERE id = ?1", [m])
        .unwrap();

    // Kaamelott: show / season / episodes, one file holding two episodes.
    b.meta(
        201,
        2,
        2,
        None,
        None,
        "Kaamelott",
        Some(2005),
        "plex://show/k",
    );
    b.tag(201, 314, "tvdb://79175");
    b.tag(201, 1, "Comédie");
    b.meta(
        202,
        2,
        3,
        Some(201),
        Some(1),
        "Livre I",
        None,
        "plex://season/k1",
    );
    b.meta(
        203,
        2,
        4,
        Some(202),
        Some(1),
        "Heat",
        Some(2005),
        "plex://episode/1",
    );
    b.meta(
        204,
        2,
        4,
        Some(202),
        Some(3),
        "La Tarte aux myrtilles",
        Some(2005),
        "plex://episode/3",
    );
    b.meta(
        205,
        2,
        4,
        Some(202),
        Some(4),
        "Le Duel",
        Some(2005),
        "plex://episode/4",
    );
    let m = b.media(203, 720, 576, "mkv", "h264", "aac", 2);
    let p = b.part(m, 0, "/data/tv/Kaamelott/k101.mkv", 400);
    b.stream(p, 2, "aac", "fre", 2, true, false, "", "");
    for ep in [204, 205] {
        let m = b.media(ep, 720, 576, "mkv", "h264", "aac", 2);
        // Plex lists the same file under each episode; create it once.
        let path = "/data/tv/Kaamelott/k103-104.mkv";
        if ep == 204 {
            b.part(m, 0, path, 450);
        } else {
            c.execute("INSERT INTO media_parts (media_item_id, \"index\", file, size) VALUES (?1, 0, ?2, 450)", params![m, path]).unwrap();
        }
    }
    drop(b);
    drop(c);
    Fixture { dir, db }
}

pub fn load(
    fx: &Fixture,
    cfg: &plexfilerenamer::config::Config,
    sel: &plexfilerenamer::plan::Select,
) -> (
    plexfilerenamer::template::Engine,
    Vec<plexfilerenamer::plan::Loaded>,
) {
    let engine = plexfilerenamer::template::Engine::new(cfg);
    let db = plexfilerenamer::db::open(&fx.db, false).unwrap();
    let loaded = plexfilerenamer::plan::load(&db, cfg, &engine, sel).unwrap();
    (engine, loaded)
}

/// One line per op: "status kind src -> dst (reason)", tempdir redacted.
pub fn plan_text(fx: &Fixture, p: &plexfilerenamer::plan::Plan) -> String {
    p.ops
        .iter()
        .map(|o| {
            let st = serde_json::to_value(o.status).unwrap();
            let k = serde_json::to_value(o.kind).unwrap();
            let mut s = format!(
                "{} {} {} -> {}",
                st.as_str().unwrap(),
                k.as_str().unwrap(),
                o.src.as_deref().unwrap_or("-"),
                o.dst
            );
            if let Some(r) = &o.reason {
                s += &format!("  ({r})");
            }
            fx.redact(&s)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
