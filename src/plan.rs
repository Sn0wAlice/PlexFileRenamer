//! From items to a list of file operations: rendering, filters, sidecars, collisions.
//! Read-only: looks at the disk but never changes it.

use crate::config::{Collision, Config};
use crate::db::Db;
use crate::mapping::Mapping;
use crate::model::Item;
use crate::template::Engine;
use anyhow::{Result, bail};
use regex::Regex;
use serde::Serialize;
use serde_json::Value as Json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "mov", "wmv", "mpg", "mpeg", "ts", "m2ts", "vob", "iso", "webm",
    "flv", "ogm", "divx",
];

/// Selection options shared by plan/script/apply/vars/render.
#[derive(Debug, Clone, Default)]
pub struct Select {
    pub profile: Option<String>,
    /// Library names or ids.
    pub libraries: Vec<String>,
    pub filters: Vec<Filter>,
    pub limit: Option<usize>,
    /// Plex id (movie, episode or show) or title substring.
    pub item: Option<String>,
}

/// An item with its profile and template context (what `vars` shows).
pub struct Loaded {
    pub item: Item,
    pub profile: Option<String>,
    pub ctx: Json,
}

pub fn load(db: &Db, cfg: &Config, engine: &Engine, sel: &Select) -> Result<Vec<Loaded>> {
    let libs = db.libraries()?;
    let mut sections = vec![];
    for want in &sel.libraries {
        match libs
            .iter()
            .find(|l| l.id.to_string() == *want || l.name.eq_ignore_ascii_case(want))
        {
            Some(l) => sections.push(l.id),
            None => bail!("unknown library {want:?} (see `plexfilerenamer libraries`)"),
        }
    }
    let roots: HashMap<i64, Vec<String>> = libs
        .iter()
        .map(|l| (l.id, l.roots.iter().map(|r| cfg.map_path(r)).collect()))
        .collect();
    let mut out = vec![];
    for mut item in db.items(&sections)? {
        if let Some(q) = &sel.item {
            let ql = q.to_lowercase();
            let by_id = q.parse::<i64>().is_ok_and(|id| {
                id == item.ids.plex || item.show.as_ref().is_some_and(|s| s.ids.plex == id)
            });
            let by_title = item.title.to_lowercase().contains(&ql)
                || item
                    .show
                    .as_ref()
                    .is_some_and(|s| s.title.to_lowercase().contains(&ql));
            if !by_id && !by_title {
                continue;
            }
        }
        item.path = cfg.map_path(&item.plex_path);
        item.root = roots
            .get(&item.library_id)
            .into_iter()
            .flatten()
            .filter(|r| is_under(&item.path, r))
            .max_by_key(|r| r.len())
            .cloned();
        for s in &mut item.subs {
            if let Some(p) = &s.path {
                // Some Plex versions percent-encode file:// urls.
                let raw = cfg.map_path(p);
                let decoded = cfg.map_path(&crate::db::pct_decode(p));
                s.path = Some(if !exists(&raw) && exists(&decoded) {
                    decoded
                } else {
                    raw
                });
            }
        }
        item.finalize(&cfg.mapping);
        let prof = cfg.profile_for(
            sel.profile.as_deref(),
            &item.kind,
            item.library_id,
            &item.library,
        );
        let ctx = match prof {
            Some((_, p)) => engine.context(&item, p)?,
            None => serde_json::to_value(&item)?,
        };
        if !sel.filters.iter().all(|f| f.matches(&ctx, &cfg.mapping)) {
            continue;
        }
        out.push(Loaded {
            profile: prof.map(|(n, _)| n.to_string()),
            item,
            ctx,
        });
        if sel.limit.is_some_and(|l| out.len() >= l) {
            break;
        }
    }
    if sel.profile.is_some() && !cfg.profiles.contains_key(sel.profile.as_deref().unwrap()) {
        bail!("unknown profile {:?}", sel.profile.as_deref().unwrap());
    }
    Ok(out)
}

fn is_under(p: &str, root: &str) -> bool {
    let r = root.trim_end_matches('/');
    p.strip_prefix(r).is_some_and(|rest| rest.starts_with('/'))
}

// ---------- filters ----------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Cmp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Re,
}

#[derive(Debug, Clone)]
pub struct Filter {
    path: Vec<String>,
    cmp: Cmp,
    value: String,
    re: Option<Regex>,
}

impl std::str::FromStr for Filter {
    type Err = anyhow::Error;

    /// `year<2000`, `title~^Star`, `audio.lang=fr`, `resolution=576p`, `video.height>=1080`.
    fn from_str(s: &str) -> Result<Filter> {
        let at = s
            .find(['<', '>', '=', '!', '~'])
            .ok_or_else(|| anyhow::anyhow!("filter {s:?}: no operator (= != < <= > >= ~)"))?;
        let (field, rest) = s.split_at(at);
        let (cmp, n) = match rest.as_bytes() {
            [b'<', b'=', ..] => (Cmp::Le, 2),
            [b'>', b'=', ..] => (Cmp::Ge, 2),
            [b'!', b'=', ..] => (Cmp::Ne, 2),
            [b'<', ..] => (Cmp::Lt, 1),
            [b'>', ..] => (Cmp::Gt, 1),
            [b'=', ..] => (Cmp::Eq, 1),
            [b'~', ..] => (Cmp::Re, 1),
            _ => bail!("filter {s:?}: bad operator"),
        };
        let value = rest[n..].trim().to_string();
        let field = field.trim();
        if field.is_empty() {
            bail!("filter {s:?}: missing field");
        }
        let re = if cmp == Cmp::Re {
            Some(Regex::new(&format!("(?i){value}"))?)
        } else {
            None
        };
        Ok(Filter {
            path: field.split('.').map(String::from).collect(),
            cmp,
            value,
            re,
        })
    }
}

fn resolve<'a>(v: &'a Json, path: &[String], out: &mut Vec<&'a Json>) {
    match v {
        Json::Array(a) => a.iter().for_each(|x| resolve(x, path, out)),
        _ if path.is_empty() => out.push(v),
        Json::Object(o) => {
            let key = if path[0] == "lang" {
                "language"
            } else {
                path[0].as_str()
            };
            if let Some(x) = o.get(key) {
                resolve(x, &path[1..], out)
            }
        }
        _ => {}
    }
}

fn as_text(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        Json::Null => String::new(),
        x => x.to_string(),
    }
}

impl Filter {
    pub fn matches(&self, ctx: &Json, m: &Mapping) -> bool {
        let mut vals = vec![];
        resolve(ctx, &self.path, &mut vals);
        if vals.is_empty() && self.path.len() == 1 {
            resolve(ctx, &["video".to_string(), self.path[0].clone()], &mut vals);
        }
        let is_lang = self
            .path
            .last()
            .is_some_and(|l| l == "lang" || l == "language");
        let eq = |v: &Json| {
            let t = as_text(v);
            if is_lang {
                m.lang(&t, Some("iso1")) == m.lang(&self.value, Some("iso1"))
            } else {
                match (t.parse::<f64>(), self.value.parse::<f64>()) {
                    (Ok(a), Ok(b)) => a == b,
                    _ => t.eq_ignore_ascii_case(&self.value),
                }
            }
        };
        let ord = |v: &Json| -> Option<std::cmp::Ordering> {
            let t = as_text(v);
            match (t.parse::<f64>(), self.value.parse::<f64>()) {
                (Ok(a), Ok(b)) => a.partial_cmp(&b),
                _ => Some(t.to_lowercase().cmp(&self.value.to_lowercase())),
            }
        };
        use std::cmp::Ordering::*;
        match self.cmp {
            Cmp::Eq => vals.iter().any(|v| eq(v)),
            Cmp::Ne => !vals.iter().any(|v| eq(v)),
            Cmp::Re => vals
                .iter()
                .any(|v| self.re.as_ref().unwrap().is_match(&as_text(v))),
            Cmp::Lt => vals.iter().any(|v| ord(v) == Some(Less)),
            Cmp::Le => vals.iter().any(|v| matches!(ord(v), Some(Less | Equal))),
            Cmp::Gt => vals.iter().any(|v| ord(v) == Some(Greater)),
            Cmp::Ge => vals.iter().any(|v| matches!(ord(v), Some(Greater | Equal))),
        }
    }
}

// ---------- plan ----------

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Video,
    Subtitle,
    Sidecar,
    Plexmatch,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Rename,
    Unchanged,
    Skipped,
    Conflict,
}

#[derive(Debug, Clone, Serialize)]
pub struct Op {
    pub kind: Kind,
    /// Plex metadata id.
    pub item: i64,
    /// Index of the video op this op depends on (itself for videos).
    pub group: usize,
    /// None for files we create (.plexmatch).
    pub src: Option<String>,
    pub dst: String,
    /// media_parts.size, checked before touching the source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Library root: empty-dir cleanup never goes above it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Plan {
    pub ops: Vec<Op>,
}

impl Plan {
    pub fn conflicts(&self) -> usize {
        self.ops
            .iter()
            .filter(|o| o.status == Status::Conflict)
            .count()
    }
    pub fn count(&self, s: Status) -> usize {
        self.ops.iter().filter(|o| o.status == s).count()
    }
}

fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    }
}

fn parent(p: &str) -> &str {
    p.rfind('/').map(|i| &p[..i]).unwrap_or("")
}

fn file_name(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

fn join(dir: &str, name: &str) -> String {
    format!("{}/{}", dir.trim_end_matches('/'), name)
}

/// Same file on disk (case-only rename on a case-insensitive FS).
fn same_file(a: &str, b: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return x.dev() == y.dev() && x.ino() == y.ino();
        }
        false
    }
    #[cfg(not(unix))]
    {
        a.eq_ignore_ascii_case(b) && Path::new(a).exists()
    }
}

fn exists(p: &str) -> bool {
    Path::new(p).symlink_metadata().is_ok()
}

fn suffixed(dst: &str, n: usize) -> String {
    let (dir, name) = (parent(dst), file_name(dst));
    let (stem, ext) = split_name(name);
    let name = if ext.is_empty() {
        format!("{stem}.v{n}")
    } else {
        format!("{stem}.v{n}.{ext}")
    };
    join(dir, &name)
}

/// Mark collisions (same target twice, or target already on disk) and apply the strategy.
/// `taken` holds lowercased targets already claimed by previous passes.
fn resolve_collisions(
    ops: &mut [Op],
    from: usize,
    strategy: Collision,
    taken: &mut HashSet<String>,
) {
    #[allow(clippy::needless_range_loop)]
    for i in from..ops.len() {
        if ops[i].status != Status::Rename && ops[i].status != Status::Unchanged {
            continue;
        }
        if ops[i].status == Status::Unchanged {
            taken.insert(ops[i].dst.to_lowercase());
            continue;
        }
        let busy = |d: &str, taken: &HashSet<String>, src: Option<&str>| {
            taken.contains(&d.to_lowercase())
                || (exists(d) && !src.is_some_and(|s| same_file(s, d)))
        };
        if !busy(&ops[i].dst, taken, ops[i].src.as_deref()) {
            taken.insert(ops[i].dst.to_lowercase());
            continue;
        }
        let why = if taken.contains(&ops[i].dst.to_lowercase()) {
            "another item targets the same path"
        } else {
            "target already exists"
        };
        match strategy {
            Collision::Fail => {
                ops[i].status = Status::Conflict;
                ops[i].reason = Some(why.into());
            }
            Collision::Skip => {
                ops[i].status = Status::Skipped;
                ops[i].reason = Some(format!("collision: {why}"));
            }
            Collision::Suffix => {
                let d = (2..)
                    .map(|n| suffixed(&ops[i].dst, n))
                    .find(|d| !busy(d, taken, None))
                    .unwrap();
                taken.insert(d.to_lowercase());
                ops[i].reason = Some(format!("{why}, suffixed"));
                ops[i].dst = d;
            }
        }
    }
}

fn status_for(src: &str, dst: &str) -> Status {
    if src == dst {
        Status::Unchanged
    } else {
        Status::Rename
    }
}

pub fn build(loaded: &[Loaded], cfg: &Config, engine: &Engine) -> Plan {
    let mut ops: Vec<Op> = vec![];
    let skip = |l: &Loaded, reason: String| Op {
        kind: Kind::Video,
        item: l.item.ids.plex,
        group: 0,
        src: Some(l.item.path.clone()),
        dst: l.item.path.clone(),
        size: Some(l.item.size),
        content: None,
        root: l.item.root.clone(),
        status: Status::Skipped,
        reason: Some(reason),
    };
    // Phase 1: videos, so sidecars can follow the final (possibly suffixed) names.
    for l in loaded {
        let it = &l.item;
        let Some(pname) = &l.profile else {
            ops.push(skip(l, "no profile matches this library".into()));
            continue;
        };
        let p = &cfg.profiles[pname];
        let Some(root) = p.root.clone().or(it.root.clone()) else {
            ops.push(skip(
                l,
                "file is outside every library root (check path_map)".into(),
            ));
            continue;
        };
        let r = match engine.render_path(p, &l.ctx, &it.ext) {
            Ok(r) => r,
            Err(e) => {
                ops.push(skip(l, format!("{e:#}")));
                continue;
            }
        };
        let mut dst = root.trim_end_matches('/').to_string();
        for d in &r.dirs {
            dst = join(&dst, d);
        }
        let name = if it.ext.is_empty() {
            r.stem.clone()
        } else {
            format!("{}.{}", r.stem, it.ext)
        };
        let dst = join(&dst, &name);
        ops.push(Op {
            kind: Kind::Video,
            item: it.ids.plex,
            group: ops.len(),
            status: status_for(&it.path, &dst),
            src: Some(it.path.clone()),
            dst,
            size: Some(it.size).filter(|s| *s > 0),
            content: None,
            root: it.root.clone(),
            reason: None,
        });
    }
    let mut taken = HashSet::new();
    resolve_collisions(&mut ops, 0, cfg.collision, &mut taken);
    let videos = ops.len();

    // Files Plex knows about are never picked up as sidecars of something else.
    let mut claimed: HashSet<String> = loaded.iter().map(|l| l.item.path.clone()).collect();
    for l in loaded {
        claimed.extend(l.item.subs.iter().filter_map(|s| s.path.clone()));
    }

    // Phase 2: subtitles, sidecars, folder files, .plexmatch.
    let by_item: HashMap<(i64, &str), &Loaded> = loaded
        .iter()
        .map(|l| ((l.item.ids.plex, l.item.path.as_str()), l))
        .collect();
    let mut dir_videos: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut plexmatch: BTreeMap<String, (i64, usize, String)> = BTreeMap::new();
    for g in 0..videos {
        let v = ops[g].clone();
        if !matches!(v.status, Status::Rename | Status::Unchanged) {
            continue;
        }
        let src = v.src.clone().unwrap();
        let l = by_item[&(v.item, src.as_str())];
        let p = &cfg.profiles[l.profile.as_deref().unwrap()];
        dir_videos
            .entry(parent(&src).to_string())
            .or_default()
            .push(g);
        let new_dir = parent(&v.dst).to_string();
        let new_stem = split_name(file_name(&v.dst)).0.to_string();
        let mut sub_n: HashMap<(String, bool), usize> = HashMap::new();
        let mut followed: HashSet<String> = HashSet::new();

        for s in l.item.subs.iter().filter(|s| s.external) {
            let Some(sp) = &s.path else { continue };
            if !exists(sp) {
                ops.push(Op {
                    status: Status::Skipped,
                    reason: Some("subtitle file not found (stale in Plex?)".into()),
                    ..sub_op(&v, g, sp, sp)
                });
                continue;
            }
            let (sstem, sext) = split_name(file_name(sp));
            let lang = cfg.mapping.lang(&s.language, Some("iso1")).to_lowercase();
            let n = sub_n.entry((lang.clone(), s.forced)).or_insert(0);
            *n += 1;
            let mut ctx = l.ctx.clone();
            let o = ctx.as_object_mut().unwrap();
            o.insert("base".into(), new_stem.clone().into());
            o.insert("lang".into(), lang.into());
            o.insert("language".into(), s.language.clone().into());
            o.insert("forced".into(), s.forced.into());
            o.insert("default".into(), s.default.into());
            o.insert("codec".into(), s.codec.clone().into());
            o.insert("title".into(), s.title.clone().into());
            o.insert("n".into(), (*n).into());
            o.insert("ext".into(), sext.into());
            let name = match engine.render_raw(&p.subtitle, &ctx) {
                Ok(n) => engine.finish_segment(&n),
                Err(e) => {
                    ops.push(Op {
                        kind: Kind::Subtitle,
                        status: Status::Skipped,
                        reason: Some(format!("{e:#}")),
                        ..sub_op(&v, g, sp, sp)
                    });
                    continue;
                }
            };
            let dst = join(&new_dir, &name);
            followed.insert(sp.clone());
            ops.push(Op {
                status: status_for(sp, &dst),
                ..sub_op(&v, g, sp, &dst)
            });
            // .idx/.sub pairs must keep the same stem.
            if sext.eq_ignore_ascii_case("idx") || sext.eq_ignore_ascii_case("sub") {
                let other = if sext.eq_ignore_ascii_case("idx") {
                    "sub"
                } else {
                    "idx"
                };
                let (nstem, _) = split_name(&name);
                for e in [other.to_string(), other.to_uppercase()] {
                    let mate = join(parent(sp), &format!("{sstem}.{e}"));
                    if exists(&mate) && !claimed.contains(&mate) {
                        let d = join(&new_dir, &format!("{nstem}.{e}"));
                        followed.insert(mate.clone());
                        ops.push(Op {
                            kind: Kind::Sidecar,
                            status: status_for(&mate, &d),
                            ..sub_op(&v, g, &mate, &d)
                        });
                        break;
                    }
                }
            }
        }
        claimed.extend(followed);

        if cfg.write_plexmatch {
            let (target_dir, body) = plexmatch_for(&l.item, &v.dst, l.item.root.as_deref());
            if let Some(body) = body {
                plexmatch
                    .entry(join(&target_dir, ".plexmatch"))
                    .or_insert((v.item, g, body));
            }
        }
    }

    // Name-based sidecars: <stem>.<x>.<ext> follows the video with the longest matching stem in its dir.
    if cfg.sidecars.enabled {
        let exts: HashSet<String> = cfg
            .sidecars
            .extensions
            .iter()
            .map(|e| e.to_lowercase())
            .collect();
        for (dir, gs) in &dir_videos {
            let Ok(rd) = std::fs::read_dir(dir) else {
                continue;
            };
            let mut names: Vec<String> = rd
                .flatten()
                .filter(|e| e.path().is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            for name in &names {
                let full = join(dir, name);
                let ext = split_name(name).1.to_lowercase();
                if claimed.contains(&full) || !exts.contains(&ext) {
                    continue;
                }
                let best = gs
                    .iter()
                    .map(|&g| {
                        (
                            g,
                            split_name(file_name(ops[g].src.as_deref().unwrap()))
                                .0
                                .to_string(),
                        )
                    })
                    .filter(|(_, stem)| {
                        name.len() > stem.len()
                            && name.starts_with(stem.as_str())
                            && matches!(name.as_bytes()[stem.len()], b'.' | b'-')
                    })
                    .max_by_key(|(_, stem)| stem.len());
                if let Some((g, stem)) = best {
                    let v = ops[g].clone();
                    let new_stem = split_name(file_name(&v.dst)).0;
                    let d = join(
                        parent(&v.dst),
                        &format!("{new_stem}{}", &name[stem.len()..]),
                    );
                    ops.push(Op {
                        kind: Kind::Sidecar,
                        status: status_for(&full, &d),
                        ..sub_op(&v, g, &full, &d)
                    });
                    claimed.insert(full);
                }
            }
            // Folder files move only if this folder is exclusively one destination folder's content.
            let dsts: HashSet<&str> = gs.iter().map(|&g| parent(&ops[g].dst)).collect();
            let foreign_video = names.iter().any(|n| {
                VIDEO_EXTS.contains(&split_name(n).1.to_lowercase().as_str())
                    && !claimed.contains(&join(dir, n))
            });
            if dsts.len() != 1 || foreign_video || dsts.contains(dir.as_str()) {
                continue;
            }
            let g = gs[0];
            let new_dir = parent(&ops[g].dst).to_string();
            for f in &cfg.sidecars.folder_files {
                if cfg.write_plexmatch && f == ".plexmatch" {
                    continue;
                }
                let full = join(dir, f);
                if exists(&full) && !claimed.contains(&full) {
                    let v = ops[g].clone();
                    ops.push(Op {
                        kind: Kind::Sidecar,
                        status: Status::Rename,
                        ..sub_op(&v, g, &full, &join(&new_dir, f))
                    });
                    claimed.insert(full);
                }
            }
        }
    }

    for (dst, (item, g, body)) in plexmatch {
        let status = match std::fs::read_to_string(&dst) {
            Ok(old) if old == body => Status::Unchanged,
            _ => Status::Rename,
        };
        ops.push(Op {
            kind: Kind::Plexmatch,
            item,
            group: g,
            src: None,
            dst,
            size: None,
            content: Some(body),
            root: ops[g].root.clone(),
            status,
            reason: None,
        });
    }

    resolve_collisions(&mut ops, videos, cfg.collision, &mut taken);
    Plan { ops }
}

fn sub_op(v: &Op, g: usize, src: &str, dst: &str) -> Op {
    Op {
        kind: Kind::Subtitle,
        item: v.item,
        group: g,
        src: Some(src.to_string()),
        dst: dst.to_string(),
        size: None,
        content: None,
        root: v.root.clone(),
        status: Status::Rename,
        reason: None,
    }
}

/// Movies: in the movie folder. Episodes: in the show folder (first folder under the root).
fn plexmatch_for(it: &Item, dst: &str, root: Option<&str>) -> (String, Option<String>) {
    let mut body = String::new();
    let dir;
    if let Some(show) = &it.show {
        let root = root.unwrap_or("").trim_end_matches('/');
        let rel = dst
            .strip_prefix(root)
            .unwrap_or(dst)
            .trim_start_matches('/');
        dir = match rel.split_once('/') {
            Some((first, _)) => join(root, first),
            None => parent(dst).to_string(),
        };
        body += &format!("title: {}\n", show.title);
        if let Some(y) = show.year {
            body += &format!("year: {y}\n");
        }
        for (k, v) in [
            ("tvdbid", &show.ids.tvdb),
            ("tmdbid", &show.ids.tmdb),
            ("imdbid", &show.ids.imdb),
        ] {
            if let Some(v) = v {
                body += &format!("{k}: {v}\n");
            }
        }
        let has_id = show.ids.tvdb.is_some() || show.ids.tmdb.is_some() || show.ids.imdb.is_some();
        return (dir, has_id.then_some(body));
    }
    dir = parent(dst).to_string();
    body += &format!("title: {}\n", it.title);
    if let Some(y) = it.year {
        body += &format!("year: {y}\n");
    }
    for (k, v) in [("tmdbid", &it.ids.tmdb), ("imdbid", &it.ids.imdb)] {
        if let Some(v) = v {
            body += &format!("{k}: {v}\n");
        }
    }
    if let Some(e) = &it.edition {
        body += &format!("edition: {e}\n");
    }
    let has_id = it.ids.tmdb.is_some() || it.ids.imdb.is_some();
    (dir, has_id.then_some(body))
}

/// Human-readable before/after listing.
pub fn print(plan: &Plan, only_changed: bool) {
    for o in &plan.ops {
        if only_changed && o.status == Status::Unchanged {
            continue;
        }
        let tag = match o.status {
            Status::Rename if o.src.is_none() => "write",
            Status::Rename => "rename",
            Status::Unchanged => "ok",
            Status::Skipped => "SKIP",
            Status::Conflict => "CONFLICT",
        };
        let kind = match o.kind {
            Kind::Video => "",
            Kind::Subtitle => " (sub)",
            Kind::Sidecar => " (sidecar)",
            Kind::Plexmatch => " (.plexmatch)",
        };
        println!("{tag:>8}{kind} {}", o.src.as_deref().unwrap_or("-"));
        if o.status != Status::Unchanged && o.src.as_deref() != Some(&o.dst) {
            println!("{:>8}  → {}", "", o.dst);
        }
        if let Some(r) = &o.reason {
            println!("{:>8}    {r}", "");
        }
    }
    println!(
        "\n{} to rename, {} unchanged, {} skipped, {} conflicts",
        plan.count(Status::Rename),
        plan.count(Status::Unchanged),
        plan.count(Status::Skipped),
        plan.conflicts()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filters() {
        let m = Mapping::default();
        let ctx = json!({"year": 1999, "title": "Star Wars", "audio": [{"language": "fre"}, {"language": "eng"}],
                          "video": {"resolution": "576p", "height": 576}});
        let t = |s: &str| s.parse::<Filter>().unwrap().matches(&ctx, &m);
        assert!(t("year<2000"));
        assert!(!t("year>=2000"));
        assert!(t("title~^star"));
        assert!(t("audio.lang=fr"));
        assert!(t("audio.lang!=de"));
        assert!(!t("audio.lang=de"));
        assert!(t("resolution=576p"));
        assert!(t("video.height>=576"));
        assert!("nothing".parse::<Filter>().is_err());
    }
}
