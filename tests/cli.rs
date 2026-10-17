//! End-to-end through the real binary.
mod common;
use std::process::Command;

fn run(fx: &common::Fixture, args: &[&str]) -> (bool, String, String) {
    let cfg = fx.root().join("plexfilerenamer.toml");
    if !cfg.exists() {
        std::fs::write(
            &cfg,
            fx.config_toml("[profiles.movies.vars]\nsource = \"DVD\"\n"),
        )
        .unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_plexfilerenamer"))
        .current_dir(fx.root())
        .arg("--config")
        .arg(&cfg)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        fx.redact(&String::from_utf8_lossy(&out.stdout)),
        fx.redact(&String::from_utf8_lossy(&out.stderr)),
    )
}

#[test]
fn commands() {
    let fx = common::build();
    let db = fx.db.to_str().unwrap();

    let (ok, out, _) = run(&fx, &["libraries", db]);
    assert!(ok);
    insta::assert_snapshot!("libraries", out);

    let (ok, out, _) = run(&fx, &["vars", db, "--item", "Amélie"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ids"]["tmdb"], "194");
    assert_eq!(v["source"], "DVD");

    let (ok, out, _) = run(
        &fx,
        &[
            "render",
            db,
            "--item",
            "101",
            "--template",
            "{{ title|scene }} [{{ audio|map(attribute='language')|langs('name')|join('+') }}]",
        ],
    );
    assert!(ok);
    assert_eq!(
        out.trim(),
        "Le.Fabuleux.Destin.d.Amelie.Poulain [French+English]"
    );

    let (ok, out, _) = run(
        &fx,
        &["plan", db, "--only-changed", "--filter", "year<2000"],
    );
    assert!(ok);
    insta::assert_snapshot!("plan_filtered", out);

    let (ok, out, _) = run(&fx, &["plan", db, "--json", "--item", "104"]);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ops"].as_array().unwrap().len(), 3);

    for sh in ["bash", "pwsh", "cmd"] {
        let (ok, out, err) = run(
            &fx,
            &["script", db, "--shell", sh, "-o", &format!("rename.{sh}")],
        );
        assert!(ok, "{err}");
        assert!(out.contains("wrote"));
    }
    insta::assert_snapshot!(
        "script_pwsh",
        fx.redact(&std::fs::read_to_string(fx.root().join("rename.pwsh")).unwrap())
    );
    insta::assert_snapshot!(
        "script_cmd",
        fx.redact(&std::fs::read_to_string(fx.root().join("rename.cmd")).unwrap())
    );
    // Never overwrite an existing script.
    let (ok, _, err) = run(&fx, &["script", db, "-o", "rename.bash"]);
    assert!(!ok && err.contains("exists"));

    // apply without --yes and without a "y" on stdin: nothing happens.
    let before = fx.tree();
    let (ok, _, _) = run(&fx, &["apply", db]);
    assert!(!ok);
    assert_eq!(fx.tree(), before);

    let (ok, out, err) = run(
        &fx,
        &[
            "apply",
            db,
            "--yes",
            "--journal",
            "j.jsonl",
            "--clean-empty-dirs",
        ],
    );
    assert!(ok, "{out}\n{err}");
    assert_ne!(fx.tree(), before);
    let (ok, out, err) = run(&fx, &["undo", "j.jsonl", "--yes"]);
    assert!(ok, "{out}\n{err}");
    assert_eq!(fx.tree(), before);
}

#[test]
fn conflicts_block_apply() {
    let fx = common::build();
    std::fs::write(
        fx.root().join("plexfilerenamer.toml"),
        fx.config_toml("[profiles.movies]\nfile = \"same\"\n"),
    )
    .unwrap();
    let before = fx.tree();
    let (ok, out, err) = run(&fx, &["apply", fx.db.to_str().unwrap(), "--yes"]);
    assert!(!ok);
    assert!(err.contains("conflicts: nothing done"), "{err}");
    assert!(out.contains("CONFLICT"));
    assert_eq!(fx.tree(), before);
}

#[test]
fn init_writes_parsable_config_once() {
    let fx = common::build();
    let (ok, _, _) = run(&fx, &["init", "-o", "new.toml"]);
    assert!(ok);
    let s = std::fs::read_to_string(fx.root().join("new.toml")).unwrap();
    plexfilerenamer::config::Config::parse(&s).unwrap();
    let (ok, _, err) = run(&fx, &["init", "-o", "new.toml"]);
    assert!(!ok && err.contains("--force"));
}

#[test]
fn incompatible_db_fails_clearly() {
    let fx = common::build();
    let c = rusqlite::Connection::open(&fx.db).unwrap();
    c.execute_batch("ALTER TABLE media_streams DROP COLUMN url;")
        .unwrap();
    drop(c);
    let (ok, _, err) = run(&fx, &["plan", fx.db.to_str().unwrap()]);
    assert!(!ok);
    assert!(err.contains("missing: media_streams.url"), "{err}");
}
