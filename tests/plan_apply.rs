mod common;
use plexfilerenamer::exec::{self, Journal, Mode};
use plexfilerenamer::plan::{self, Select, Status};
use std::fs;

const DVD: &str = "write_plexmatch = true\n[profiles.movies.vars]\nsource = \"DVD\"\n";

fn plan_for(fx: &common::Fixture, extra: &str, sel: Select) -> plan::Plan {
    let cfg = fx.config(extra);
    let (engine, loaded) = common::load(fx, &cfg, &sel);
    plan::build(&loaded, &cfg, &engine)
}

#[test]
fn full_plan_snapshot() {
    let fx = common::build();
    let p = plan_for(&fx, DVD, Select::default());
    insta::assert_snapshot!(common::plan_text(&fx, &p));
}

#[test]
fn json_plan_is_machine_readable() {
    let fx = common::build();
    let p = plan_for(
        &fx,
        "",
        Select {
            item: Some("101".into()),
            ..Default::default()
        },
    );
    let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(v["ops"][0]["kind"], "video");
    assert_eq!(v["ops"][0]["status"], "rename");
    assert_eq!(v["ops"][0]["size"], 1000);
}

#[test]
fn filters_and_limit() {
    let fx = common::build();
    let cfg = fx.config("");
    let ids = |sel: Select| -> Vec<i64> {
        let (_, l) = common::load(&fx, &cfg, &sel);
        l.iter().map(|l| l.item.ids.plex).collect()
    };
    let f = |s: &[&str]| s.iter().map(|x| x.parse().unwrap()).collect();
    assert_eq!(
        ids(Select {
            filters: f(&["year<2000", "kind=movie"]),
            ..Default::default()
        }),
        [103, 103, 104, 105]
    );
    assert_eq!(
        ids(Select {
            filters: f(&["audio.lang=fr", "resolution=576p"]),
            ..Default::default()
        }),
        [101, 203]
    );
    assert_eq!(
        ids(Select {
            filters: f(&["title~^star"]),
            ..Default::default()
        }),
        [103, 103]
    );
    assert_eq!(
        ids(Select {
            libraries: vec!["Séries".into()],
            limit: Some(1),
            ..Default::default()
        }),
        [203]
    );
}

#[test]
fn already_named_items_are_unchanged() {
    let fx = common::build();
    let cfg = fx.config("[profiles.movies]\nfolder = \"amelie\"\nfile = \"Amelie.DVDRip\"\n");
    let (engine, loaded) = common::load(
        &fx,
        &cfg,
        &Select {
            item: Some("101".into()),
            ..Default::default()
        },
    );
    let p = plan::build(&loaded, &cfg, &engine);
    assert_eq!(p.ops[0].status, Status::Unchanged);
}

/// Two items aiming at the same name, and a target that already exists: fail / skip / suffix.
#[test]
fn collisions() {
    let fx = common::build();
    // Both Star Wars versions render to the same name with this template.
    let same = "[profiles.movies]\nfile = \"{{ title|scene }}\"\n";
    let sel = || Select {
        item: Some("Star Wars".into()),
        ..Default::default()
    };

    let p = plan_for(&fx, &format!("collision = \"fail\"\n{same}"), sel());
    let st: Vec<_> = p.ops.iter().map(|o| o.status).collect();
    assert_eq!(st, [Status::Rename, Status::Conflict]);

    let p = plan_for(&fx, &format!("collision = \"skip\"\n{same}"), sel());
    assert_eq!(p.ops[1].status, Status::Skipped);

    let p = plan_for(&fx, &format!("collision = \"suffix\"\n{same}"), sel());
    assert!(
        p.ops[1]
            .dst
            .ends_with("Star.Wars.Episode.IV.A.New.Hope.v2.mkv"),
        "{}",
        p.ops[1].dst
    );

    // Pre-existing file at the target.
    let first = p.ops[0].dst.clone();
    fs::create_dir_all(std::path::Path::new(&first).parent().unwrap()).unwrap();
    fs::write(&first, "already here").unwrap();
    let p = plan_for(&fx, &format!("collision = \"fail\"\n{same}"), sel());
    assert_eq!(p.ops[0].status, Status::Conflict);
    assert_eq!(p.ops[0].reason.as_deref(), Some("target already exists"));
    let p = plan_for(&fx, &format!("collision = \"suffix\"\n{same}"), sel());
    assert!(p.ops[0].dst.ends_with(".v2.mkv") && p.ops[1].dst.ends_with(".v3.mkv"));
}

#[test]
fn apply_move_then_undo_restores_everything() {
    let fx = common::build();
    let before = fx.tree();
    let p = plan_for(
        &fx,
        &format!("clean_empty_dirs = true\n{DVD}"),
        Select::default(),
    );
    assert_eq!(p.conflicts(), 0);
    let jp = fx.root().join("journal.jsonl");
    let mut j = Journal::create(&jp).unwrap();
    let rep = exec::apply(&p, Mode::Move, &mut j, true);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    insta::assert_snapshot!("tree_after_move", fx.tree().join("\n"));

    // Journal lines are valid JSON, one per action.
    let entries = exec::read_journal(&jp).unwrap();
    assert_eq!(
        entries.len(),
        fs::read_to_string(&jp).unwrap().lines().count()
    );

    // A second plan sees everything in place.
    let p2 = plan_for(&fx, DVD, Select::default());
    assert_eq!(
        p2.count(Status::Rename),
        0,
        "{}",
        common::plan_text(&fx, &p2)
    );

    let rep = exec::undo(&entries);
    assert!(rep.errors.is_empty(), "{:?}", rep.errors);
    assert_eq!(fx.tree(), before);
}

#[test]
fn apply_other_modes_keep_sources() {
    for mode in [Mode::Copy, Mode::Hardlink, Mode::Symlink] {
        let fx = common::build();
        let before = fx.tree();
        let p = plan_for(
            &fx,
            "",
            Select {
                item: Some("101".into()),
                ..Default::default()
            },
        );
        let mut j = Journal::create(&fx.root().join("j.jsonl")).unwrap();
        let rep = exec::apply(&p, mode, &mut j, true);
        assert!(rep.errors.is_empty(), "{mode:?}: {:?}", rep.errors);
        let dst = &p.ops[0].dst;
        assert_eq!(fs::read(dst).unwrap().len(), 1000, "{mode:?}");
        assert!(
            fs::metadata(p.ops[0].src.as_ref().unwrap()).is_ok(),
            "{mode:?} removed the source"
        );
        let rep = exec::undo(&exec::read_journal(&fx.root().join("j.jsonl")).unwrap());
        assert!(rep.errors.is_empty(), "{mode:?}: {:?}", rep.errors);
        assert_eq!(fx.tree(), before, "{mode:?}");
    }
}

#[test]
fn apply_refuses_changed_or_clobbering_files() {
    let fx = common::build();
    let p = plan_for(
        &fx,
        "",
        Select {
            item: Some("101".into()),
            ..Default::default()
        },
    );
    // File changed after Plex scanned it: size mismatch, and its sidecars must not move either.
    fs::write(p.ops[0].src.as_ref().unwrap(), "shorter").unwrap();
    let before = fx.tree();
    let mut j = Journal::create(&fx.root().join("j.jsonl")).unwrap();
    let rep = exec::apply(&p, Mode::Move, &mut j, false);
    assert_eq!(rep.done, 0);
    assert!(
        rep.errors[0].contains("differs from Plex"),
        "{:?}",
        rep.errors
    );
    assert_eq!(fx.tree(), before);

    // Target created between plan and apply: never overwritten.
    let fx = common::build();
    let p = plan_for(
        &fx,
        "",
        Select {
            item: Some("104".into()),
            ..Default::default()
        },
    );
    fs::create_dir_all(std::path::Path::new(&p.ops[0].dst).parent().unwrap()).unwrap();
    fs::write(&p.ops[0].dst, "precious").unwrap();
    let mut j = Journal::create(&fx.root().join("j.jsonl")).unwrap();
    let rep = exec::apply(&p, Mode::Move, &mut j, false);
    assert!(rep.errors[0].contains("already exists"));
    assert_eq!(fs::read_to_string(&p.ops[0].dst).unwrap(), "precious");
}

#[test]
fn undo_never_overwrites() {
    let fx = common::build();
    let p = plan_for(
        &fx,
        "",
        Select {
            item: Some("104".into()),
            ..Default::default()
        },
    );
    let jp = fx.root().join("j.jsonl");
    let mut j = Journal::create(&jp).unwrap();
    assert!(exec::apply(&p, Mode::Move, &mut j, false).errors.is_empty());
    let src = p.ops[0].src.clone().unwrap();
    fs::write(&src, "new file at the old name").unwrap();
    let rep = exec::undo(&exec::read_journal(&jp).unwrap());
    assert!(
        rep.errors.iter().any(|e| e.contains("exists again")),
        "{:?}",
        rep.errors
    );
    assert_eq!(
        fs::read_to_string(&src).unwrap(),
        "new file at the old name"
    );
}

/// The bash script must produce exactly the same tree as `apply --mode move`.
#[cfg(unix)]
#[test]
fn bash_script_matches_apply() {
    let a = common::build();
    let pa = plan_for(&a, DVD, Select::default());
    let mut j = Journal::create(&a.root().join("j.jsonl")).unwrap();
    assert!(
        exec::apply(&pa, Mode::Move, &mut j, false)
            .errors
            .is_empty()
    );

    let b = common::build();
    let pb = plan_for(&b, DVD, Select::default());
    let script = b.root().join("rename.sh");
    fs::write(
        &script,
        plexfilerenamer::script::generate(&pb, plexfilerenamer::script::Shell::Bash),
    )
    .unwrap();
    let out = std::process::Command::new("bash")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    fs::remove_file(&script).unwrap();
    assert_eq!(a.tree(), b.tree());
}
