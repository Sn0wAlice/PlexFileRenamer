//! `plexfilerenamer.toml`. `Config::default()` is parsed from `DEFAULT_TOML`, which is also what
//! `init` writes, so the documented defaults can't drift from the real ones.

use crate::mapping::Mapping;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

pub const DEFAULT_TOML: &str = include_str!("default_config.toml");

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Collision {
    /// Leave the colliding files where they are.
    Skip,
    /// Append .v2, .v3… before the extension.
    Suffix,
    /// Refuse to apply anything while a collision exists.
    Fail,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub max_length: usize,
    pub collision: Collision,
    pub clean_empty_dirs: bool,
    pub write_plexmatch: bool,
    #[serde(default)]
    pub path_map: Vec<PathMap>,
    #[serde(default)]
    pub replace: Vec<Replace>,
    pub sidecars: Sidecars,
    #[serde(default, rename = "mapping")]
    mapping_override: MappingOverride,
    #[serde(skip)]
    pub mapping: Mapping,
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathMap {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Replace {
    pub pattern: String,
    #[serde(default)]
    pub with: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sidecars {
    pub enabled: bool,
    /// Files named `<video stem>.<anything>` / `<video stem>-<anything>` with these extensions follow the video.
    pub extensions: Vec<String>,
    /// Folder-level files that move with the folder when it holds a single item.
    pub folder_files: Vec<String>,
}

/// Same shape as `Mapping`, but every table is merged into the defaults instead of replacing them.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct MappingOverride {
    vcodec: BTreeMap<String, String>,
    acodec: BTreeMap<String, String>,
    languages: BTreeMap<String, String>,
    resolutions: BTreeMap<String, u32>,
    channels: BTreeMap<String, String>,
    language_style: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum LibRef {
    Id(i64),
    Name(String),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// "movie" or "episode": which items this profile handles when `libraries` is empty.
    pub kind: String,
    #[serde(default)]
    pub libraries: Vec<LibRef>,
    pub folder: String,
    pub file: String,
    pub subtitle: String,
    /// Target root. Default: the library root the file is already in.
    pub root: Option<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Template expression, e.g. `video.height >= 1080` or `library == "Films DVD"`.
    pub when: String,
    pub set: BTreeMap<String, serde_json::Value>,
}

impl Default for Config {
    fn default() -> Self {
        Config::parse("").expect("default config is valid")
    }
}

fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

impl Config {
    /// User TOML is deep-merged over the defaults, so a config only needs what it changes.
    pub fn parse(s: &str) -> Result<Config> {
        let mut base: toml::Table =
            toml::from_str(DEFAULT_TOML).expect("default config is valid TOML");
        merge(&mut base, toml::from_str(s)?);
        let mut c: Config = base.try_into()?;
        let o = std::mem::take(&mut c.mapping_override);
        let mut m = Mapping::default();
        m.vcodec.extend(o.vcodec);
        m.acodec.extend(o.acodec);
        m.languages.extend(o.languages);
        m.resolutions.extend(o.resolutions);
        m.channels.extend(o.channels);
        if let Some(s) = o.language_style {
            m.language_style = s;
        }
        c.mapping = m;
        for r in &c.replace {
            regex::Regex::new(&r.pattern)
                .with_context(|| format!("bad replace pattern {:?}", r.pattern))?;
        }
        for (name, p) in &c.profiles {
            anyhow::ensure!(
                p.kind == "movie" || p.kind == "episode",
                "profile {name}: kind must be \"movie\" or \"episode\""
            );
        }
        Ok(c)
    }

    /// Explicit path must exist; otherwise `./plexfilerenamer.toml` if present, else defaults.
    pub fn load(path: Option<&Path>) -> Result<Config> {
        let p = match path {
            Some(p) => p.to_path_buf(),
            None if Path::new("plexfilerenamer.toml").exists() => "plexfilerenamer.toml".into(),
            None => return Ok(Config::default()),
        };
        let s = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
        Config::parse(&s).with_context(|| format!("in {}", p.display()))
    }

    /// First entry whose `from` is a path-prefix of `p` wins.
    pub fn map_path(&self, p: &str) -> String {
        for m in &self.path_map {
            let from = m.from.trim_end_matches('/');
            if let Some(rest) = p.strip_prefix(from)
                && (rest.is_empty() || rest.starts_with('/'))
            {
                return format!("{}{}", m.to.trim_end_matches('/'), rest);
            }
        }
        p.to_string()
    }

    /// Profile for an item: `forced` by name, else first profile listing the library, else first of the right kind.
    pub fn profile_for(
        &self,
        forced: Option<&str>,
        kind: &str,
        lib_id: i64,
        lib_name: &str,
    ) -> Option<(&str, &Profile)> {
        if let Some(name) = forced {
            return self
                .profiles
                .get_key_value(name)
                .map(|(k, v)| (k.as_str(), v));
        }
        let listed = self.profiles.iter().find(|(_, p)| {
            p.libraries.iter().any(|l| match l {
                LibRef::Id(i) => *i == lib_id,
                LibRef::Name(n) => n.eq_ignore_ascii_case(lib_name),
            })
        });
        listed
            .or_else(|| {
                self.profiles
                    .iter()
                    .find(|(_, p)| p.libraries.is_empty() && p.kind == kind)
            })
            .map(|(k, v)| (k.as_str(), v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse_and_merge() {
        let c = Config::default();
        assert!(c.profiles.contains_key("movies") && c.profiles.contains_key("series"));
        let c = Config::parse("collision = \"skip\"\n[mapping.vcodec]\nh264 = \"AVC\"\n[profiles.movies]\nfile = \"{{ title }}\"\n").unwrap();
        assert_eq!(c.mapping.vcodec("h264"), "AVC");
        assert_eq!(c.mapping.vcodec("hevc"), "H265");
        assert_eq!(c.collision, Collision::Skip);
        assert_eq!(c.profiles["movies"].file, "{{ title }}");
        assert!(c.profiles["movies"].folder.contains("tmdb"));
        assert!(Config::parse("bogus = 1").is_err());
    }

    #[test]
    fn path_map_first_match_on_boundary() {
        let c = Config {
            path_map: vec![
                PathMap {
                    from: "/movies".into(),
                    to: "/mnt/media/movies".into(),
                },
                PathMap {
                    from: "/".into(),
                    to: "/never".into(),
                },
            ],
            ..Config::default()
        };
        assert_eq!(c.map_path("/movies/A/a.mkv"), "/mnt/media/movies/A/a.mkv");
        assert_eq!(c.map_path("/movies2/a.mkv"), "/never/movies2/a.mkv");
    }
}
