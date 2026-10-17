mod common;
use plexfilerenamer::plan::Select;

#[test]
fn amelie_matches_the_spec_example() {
    let fx = common::build();
    let cfg = fx.config("[profiles.movies.vars]\nsource = \"DVD\"\n");
    let (engine, loaded) = common::load(
        &fx,
        &cfg,
        &Select {
            item: Some("101".into()),
            ..Default::default()
        },
    );
    assert_eq!(loaded.len(), 1);
    let l = &loaded[0];
    let r = engine
        .render_path(&cfg.profiles["movies"], &l.ctx, &l.item.ext)
        .unwrap();
    assert_eq!(
        format!("{}/{}.{}", r.dirs.join("/"), r.stem, l.item.ext),
        "Le Fabuleux Destin d'Amélie Poulain (2001) {tmdb-194}/Le.Fabuleux.Destin.d.Amelie.Poulain.2001.576p.DVD.MPEG2.AC3-5.1.FR-EN.SUB-FR.mkv"
    );
}

/// Default templates on every fixture item (no custom vars: `source` is empty and must vanish cleanly).
#[test]
fn default_templates_snapshot() {
    let fx = common::build();
    let cfg = fx.config("");
    let (engine, loaded) = common::load(&fx, &cfg, &Select::default());
    let mut out = vec![];
    for l in &loaded {
        let p = &cfg.profiles[l.profile.as_deref().unwrap()];
        let line = match engine.render_path(p, &l.ctx, &l.item.ext) {
            Ok(r) => format!("{}/{}.{}", r.dirs.join("/"), r.stem, l.item.ext),
            Err(e) => format!("ERROR {e:#}"),
        };
        out.push(format!(
            "{} [{}] {}",
            l.item.ids.plex,
            l.profile.as_deref().unwrap(),
            line
        ));
    }
    insta::assert_snapshot!(out.join("\n"));
}

/// Other styles from the README must keep working.
#[test]
fn readme_styles_snapshot() {
    let fx = common::build();
    let styles = [
        (
            "plex",
            r#"folder = "{{ title }} ({{ year }}){% if edition %} {edition-{{ edition }}}{% endif %}"
file = "{{ title }} ({{ year }}){% if versions_total > 1 %} - {{ video.resolution }}{% endif %}{% if parts_total > 1 %} - pt{{ part }}{% endif %}""#,
        ),
        (
            "kodi",
            r#"folder = "{{ title|sanitize }} ({{ year }})"
file = "{{ title|sanitize }} ({{ year }}){% if versions_total > 1 %} - {{ video.resolution }}{% endif %}{% if parts_total > 1 %}-cd{{ part }}{% endif %}""#,
        ),
        (
            "long",
            r#"folder = "{{ year }}/{{ title|translit|upper|truncate(10) }}"
file = "{{ original_title|default(title, true) }} [{{ video.hdr|default('SDR', true) }}{% if video.bit_depth %} {{ video.bit_depth }}bit{% endif %}] [{{ audio|map(attribute='language')|langs('native')|join(', ') }}] {{ genres|join(', ') }}""#,
        ),
    ];
    let mut out = vec![];
    for (name, tpl) in styles {
        let cfg = fx.config(&format!("[profiles.movies]\n{tpl}\n"));
        let (engine, loaded) = common::load(
            &fx,
            &cfg,
            &Select {
                libraries: vec!["Films".into()],
                ..Default::default()
            },
        );
        for l in &loaded {
            let r = engine
                .render_path(&cfg.profiles["movies"], &l.ctx, &l.item.ext)
                .unwrap();
            out.push(format!(
                "{name}: {}/{}.{}",
                r.dirs.join("/"),
                r.stem,
                l.item.ext
            ));
        }
    }
    insta::assert_snapshot!(out.join("\n"));
}

#[test]
fn rules_and_max_length() {
    let fx = common::build();
    let cfg = fx.config(
        r#"max_length = 40
[profiles.movies]
file = "{{ title }}.{{ source }}.{{ video.resolution }}"
[profiles.movies.vars]
source = "DVD"
[[profiles.movies.rules]]
when = "video.height >= 1080"
set = { source = "BluRay" }
"#,
    );
    let (engine, loaded) = common::load(
        &fx,
        &cfg,
        &Select {
            item: Some("Star Wars".into()),
            ..Default::default()
        },
    );
    let names: Vec<String> = loaded
        .iter()
        .map(|l| {
            let r = engine
                .render_path(&cfg.profiles["movies"], &l.ctx, &l.item.ext)
                .unwrap();
            format!("{}.{}", r.stem, l.item.ext)
        })
        .collect();
    // Title is shortened, technical info and extension survive, 40 bytes max.
    assert_eq!(
        names,
        [
            "Star Wars - Episode IV.BluRay.2160p.mkv",
            "Star Wars - Episode IV - A.DVD.480p.mkv"
        ]
    );
    assert!(names.iter().all(|n| n.len() <= 40));
}

#[test]
fn vars_have_everything() {
    let fx = common::build();
    let cfg = fx.config("");
    let (_, loaded) = common::load(
        &fx,
        &cfg,
        &Select {
            item: Some("Kaamelott".into()),
            ..Default::default()
        },
    );
    let ctx = fx.redact(
        &serde_json::to_string_pretty(&loaded.iter().map(|l| &l.ctx).collect::<Vec<_>>()).unwrap(),
    );
    insta::assert_snapshot!(ctx);
}
