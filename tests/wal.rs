mod common;

/// Plex running = DB in WAL mode with data not yet checkpointed. We must see it, read-only.
#[test]
fn reads_uncheckpointed_wal_without_writing() {
    let fx = common::build();
    let w = rusqlite::Connection::open(&fx.db).unwrap();
    w.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
                     INSERT INTO library_sections (id, name, section_type) VALUES (9, 'Nouveau', 1);").unwrap();
    let wal = fx.db.with_file_name("com.plexapp.plugins.library.db-wal");
    assert!(
        wal.metadata().unwrap().len() > 0,
        "data should still be in the WAL"
    );
    let main_before = std::fs::read(&fx.db).unwrap();

    let db = plexfilerenamer::db::open(&fx.db, false).unwrap();
    assert!(db.libraries().unwrap().iter().any(|l| l.name == "Nouveau"));
    drop(db);
    assert_eq!(
        std::fs::read(&fx.db).unwrap(),
        main_before,
        "main DB file was modified"
    );
    drop(w);
}
