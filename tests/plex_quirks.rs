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
