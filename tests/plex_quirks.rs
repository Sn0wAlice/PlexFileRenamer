//! Real Plex DBs are built with Plex's own SQLite: extra modules (spellfix1, ICU fts tokenizers)
//! and ICU collations that stock SQLite doesn't know. We must read them anyway.
mod common;
use plexfilerenamer::plan::Select;

fn inject_plex_only_schema(db: &std::path::Path) {
    let c = rusqlite::Connection::open(db).unwrap();
    // Index with a real b-tree, then rename its collation to Plex's ICU one.
    c.execute_batch(
        "CREATE INDEX index_title_sort_icu ON metadata_items (title_sort COLLATE NOCASE);
         CREATE INDEX index_tags_tag_icu ON tags (tag COLLATE NOCASE);
         PRAGMA writable_schema = ON;
         UPDATE sqlite_master SET sql = replace(sql, 'COLLATE NOCASE', 'COLLATE icu_root') WHERE name IN ('index_title_sort_icu', 'index_tags_tag_icu');
         INSERT INTO sqlite_master (type, name, tbl_name, rootpage, sql)
           VALUES ('table', 'spellfix_metadata_titles', 'spellfix_metadata_titles', 0, 'CREATE VIRTUAL TABLE spellfix_metadata_titles USING spellfix1');
         INSERT INTO sqlite_master (type, name, tbl_name, rootpage, sql)
           VALUES ('table', 'fts4_metadata_titles_icu', 'fts4_metadata_titles_icu', 0, 'CREATE VIRTUAL TABLE fts4_metadata_titles_icu USING fts4(content=''metadata_items'', title, title_sort, original_title, tokenize=icu)');
         PRAGMA writable_schema = OFF;",
    )
    .unwrap();
}

#[test]
fn reads_db_with_plex_only_modules_and_collations() {
    let fx = common::build();
    inject_plex_only_schema(&fx.db);
    let db = plexfilerenamer::db::open(&fx.db, false).unwrap();
    assert_eq!(db.libraries().unwrap().len(), 3);
    let cfg = fx.config("");
    let (_, loaded) = common::load(&fx, &cfg, &Select::default());
    assert_eq!(loaded.len(), 9);
    // Lookups through the ICU-collated columns must work too.
    let (_, loaded) = common::load(
        &fx,
        &cfg,
        &Select {
            item: Some("amélie".into()),
            ..Default::default()
        },
    );
    assert_eq!(loaded.len(), 1);
}

/// Plex also stores subtitles it manages itself (agent downloads: blob://, media://) and may
/// percent-encode file:// urls. Only real files next to the video may be planned.
#[test]
fn non_file_subtitle_urls_are_not_moved() {
    let fx = common::build();
    let c = rusqlite::Connection::open(&fx.db).unwrap();
    let part: i64 = c
        .query_row(
            "SELECT id FROM media_parts WHERE file LIKE '%Amelie.DVDRip.mkv'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    std::fs::write(
        fx.root().join("data/movies/amelie/Amelie DVDRip.en.srt"),
        "en",
    )
    .unwrap();
    for (lang, url) in [
        ("ger", "blob://1a2b3c4d"),
        ("spa", "media://subtitles/abc.srt"),
        ("ita", "file:///data/movies/amelie/missing.it.srt"),
        ("eng", "file:///data/movies/amelie/Amelie%20DVDRip.en.srt"),
    ] {
        c.execute(
            "INSERT INTO media_streams (media_part_id, stream_type_id, codec, language, url) VALUES (?1, 3, 'srt', ?2, ?3)",
            rusqlite::params![part, lang, url],
        )
        .unwrap();
    }
    drop(c);
    let cfg = fx.config("");
    let (engine, loaded) = common::load(
        &fx,
        &cfg,
        &plexfilerenamer::plan::Select {
            item: Some("101".into()),
            ..Default::default()
        },
    );
    let p = plexfilerenamer::plan::build(&loaded, &cfg, &engine);
    let text = common::plan_text(&fx, &p);
    assert!(
        !text.contains("blob://") && !text.contains("media://"),
        "{text}"
    );
    assert!(
        text.contains("skipped subtitle <TMP>/data/movies/amelie/missing.it.srt"),
        "{text}"
    );
    assert!(
        text.contains("rename subtitle <TMP>/data/movies/amelie/Amelie DVDRip.en.srt"),
        "{text}"
    );

    let mut j = plexfilerenamer::exec::Journal::create(&fx.root().join("j.jsonl")).unwrap();
    let rep = plexfilerenamer::exec::apply(&p, plexfilerenamer::exec::Mode::Move, &mut j, false);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
}
