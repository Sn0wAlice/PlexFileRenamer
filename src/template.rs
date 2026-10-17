//! minijinja environment, custom filters, and post-processing of rendered names.

use crate::config::{Config, Profile};
use crate::model::Item;
use anyhow::{Context, Result, anyhow, bail};
use deunicode::deunicode;
use minijinja::value::{Serde, Value};
use minijinja::{Environment, UndefinedBehavior};
use regex::Regex;
use serde_json::Value as Json;
use std::sync::{Arc, LazyLock};

/// Stand-in for '/' and '\' inside values, so "Face/Off" can't create a sub-folder.
const SLASH: char = '\u{E000}';
const FORBIDDEN: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
const RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];
/// Elided words before an apostrophe that become a separator (d'Amélie → d.Amelie); otherwise dropped (Ocean's → Oceans).
const ELISIONS: &[&str] = &[
    "l", "d", "j", "m", "n", "s", "t", "c", "qu", "jusqu", "lorsqu", "puisqu", "quoiqu",
];

fn is_apostrophe(c: char) -> bool {
    matches!(c, '\'' | '’' | '‘' | '`' | '´' | 'ʼ')
}

fn text(v: &Value) -> String {
    if v.is_undefined() || v.is_none() {
        String::new()
    } else {
        v.to_string()
    }
}

/// Scene style: ASCII letters/digits separated by single dots.
pub fn scene(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut pre = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == SLASH {
            pre.push('.');
        } else if is_apostrophe(c) {
            let left: String = pre
                .chars()
                .rev()
                .take_while(|c| c.is_alphabetic())
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let next_alpha = chars.get(i + 1).is_some_and(|c| c.is_alphabetic());
            if next_alpha && ELISIONS.contains(&left.to_lowercase().as_str()) {
                pre.push('.');
            }
        } else {
            pre.push(c);
        }
    }
    let mut out = String::new();
    for c in deunicode(&pre).chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('.') {
            out.push('.');
        }
    }
    out.trim_end_matches('.').to_string()
}

/// Soft: keeps accents and spaces, replaces only what filesystems forbid with `sep`.
pub fn sanitize(s: &str, sep: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c == SLASH || FORBIDDEN.contains(&c) || c.is_control() {
            out.push_str(sep);
        } else {
            out.push(c);
        }
    }
    let mut prev = String::new();
    while prev != out {
        prev = out.clone();
        out = out.replace("  ", " ");
        if !sep.is_empty() {
            out = out.replace(&format!("{sep}{sep}"), sep);
        }
        out = out
            .trim_start_matches(|c: char| c == ' ' || sep.contains(c))
            .trim_end_matches(|c: char| c == ' ' || c == '.' || sep.contains(c))
            .to_string();
    }
    fix_reserved(out)
}

fn fix_reserved(s: String) -> String {
    let stem_len = s.find('.').unwrap_or(s.len());
    if RESERVED.contains(&s[..stem_len].trim_end().to_uppercase().as_str()) {
        let at = s[..stem_len].trim_end().len();
        return format!("{}_{}", &s[..at], &s[at..]);
    }
    s
}

static COLLAPSE: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (r"\(\s*\)|\[\s*\]|\{\s*\}", ""), // empty brackets: "()" from an empty year
        (r"\.{2,}", "."),
        (r" {2,}", " "),
        (r"-{2,}", "-"),
        (r"_{2,}", "_"),
        (r"\.-+|-+\.", "."),  // scene style with an empty value: AC3-.FR
        (r" -( -)+ ", " - "), // Plex style with an empty value
    ]
    .iter()
    .map(|(p, r)| (Regex::new(p).unwrap(), *r))
    .collect()
});

/// Collapse separators left by empty values. Never touches letters/digits.
pub fn collapse(s: &str) -> String {
    let mut cur = s.to_string();
    loop {
        let mut next = cur.clone();
        for (re, rep) in COLLAPSE.iter() {
            next = re.replace_all(&next, *rep).into_owned();
        }
        next = next
            .trim_matches(|c: char| matches!(c, ' ' | '.' | '-' | '_'))
            .to_string();
        if next == cur {
            return cur;
        }
        cur = next;
    }
}

/// Make one path segment safe on Windows, macOS and Linux, whatever the template produced.
pub fn safe_segment(s: &str) -> String {
    let s = s.replace(SLASH, "-").replace(": ", " - ");
    let mut out = String::new();
    for c in s.chars() {
        match c {
            ':' | '|' => out.push('-'),
            '"' => out.push('\''),
            c if FORBIDDEN.contains(&c) || c.is_control() => {}
            c => out.push(c),
        }
    }
    fix_reserved(collapse(&out))
}

fn truncate_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect::<String>().trim_end().to_string()
}

fn truncate_bytes(s: &str, max: usize) -> String {
    let mut end = max.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

pub struct Engine {
    env: Environment<'static>,
    replace: Vec<(Regex, String)>,
    max_length: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    /// Folder segments under the target root.
    pub dirs: Vec<String>,
    /// File name without extension.
    pub stem: String,
}

impl Engine {
    pub fn new(cfg: &Config) -> Engine {
        let m = Arc::new(cfg.mapping.clone());
        let mut env = Environment::new();
        env.set_undefined_behavior(UndefinedBehavior::Chainable);
        env.set_formatter(|out, state, value| {
            if value.is_none() {
                return Ok(());
            }
            minijinja::escape_formatter(out, state, value)
        });
        env.add_filter("scene", |v: Value| scene(&text(&v)));
        env.add_filter("sanitize", |v: Value, sep: Option<String>| {
            sanitize(&text(&v), sep.as_deref().unwrap_or(" "))
        });
        env.add_filter("translit", |v: Value| {
            text(&v)
                .split(SLASH)
                .map(deunicode)
                .collect::<Vec<_>>()
                .join(&SLASH.to_string())
        });
        env.add_filter("title_case", |v: Value| {
            text(&v)
                .split(' ')
                .map(|w| {
                    let mut c = w.chars();
                    c.next()
                        .map(|f| {
                            f.to_uppercase()
                                .chain(c.flat_map(|x| x.to_lowercase()))
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect::<Vec<String>>()
                .join(" ")
        });
        env.add_filter("truncate", |v: Value, n: usize| {
            truncate_chars(&text(&v), n)
        });
        env.add_filter("pad", |v: Value, n: Option<usize>| {
            let n = n.unwrap_or(2);
            match v.as_i64().or_else(|| text(&v).parse().ok()) {
                Some(i) => format!("{i:0n$}"),
                None => text(&v),
            }
        });
        let mm = m.clone();
        env.add_filter("vcodec", move |v: Value| mm.vcodec(&text(&v)));
        let mm = m.clone();
        env.add_filter("acodec", move |v: Value| mm.acodec(&text(&v)));
        let mm = m.clone();
        env.add_filter("lang", move |v: Value, style: Option<String>| {
            mm.lang(&text(&v), style.as_deref())
        });
        let mm = m.clone();
        env.add_filter(
            "langs",
            move |v: Value, style: Option<String>| -> Result<Vec<String>, minijinja::Error> {
                let mut out: Vec<String> = vec![];
                if v.is_undefined() || v.is_none() {
                    return Ok(out);
                }
                for x in v.try_iter()? {
                    let l = mm.lang(&text(&x), style.as_deref());
                    if !l.is_empty() && !out.contains(&l) {
                        out.push(l);
                    }
                }
                Ok(out)
            },
        );
        Engine {
            env,
            replace: cfg
                .replace
                .iter()
                .map(|r| (Regex::new(&r.pattern).unwrap(), r.with.clone()))
                .collect(),
            max_length: cfg.max_length,
        }
    }

    /// Item as JSON + profile vars + matching rules. This is exactly what `vars` prints.
    pub fn context(&self, item: &Item, profile: &Profile) -> Result<Json> {
        let mut ctx = serde_json::to_value(item)?;
        let obj = ctx.as_object_mut().unwrap();
        for (k, v) in &profile.vars {
            obj.insert(k.clone(), v.clone());
        }
        for rule in &profile.rules {
            let expr = self
                .env
                .compile_expression(&rule.when)
                .with_context(|| format!("rule {:?}", rule.when))?;
            if expr
                .eval(Value::from(Serde(&*obj)))
                .with_context(|| format!("rule {:?}", rule.when))?
                .is_true()
            {
                for (k, v) in &rule.set {
                    obj.insert(k.clone(), v.clone());
                }
            }
        }
        Ok(ctx)
    }

    /// Plain Jinja render, no cleanup. Slashes inside values are protected.
    pub fn render_raw(&self, tpl: &str, ctx: &Json) -> Result<String> {
        let mut ctx = ctx.clone();
        protect_slashes(&mut ctx, false);
        self.env
            .render_str(tpl, Value::from(Serde(&ctx)))
            .map_err(|e| anyhow!("template error: {e:#}"))
    }

    /// Clean one rendered segment: user replacements, then mandatory safety.
    pub fn finish_segment(&self, s: &str) -> String {
        let mut s = s.to_string();
        for (re, with) in &self.replace {
            s = re.replace_all(&s, with.as_str()).into_owned();
        }
        safe_segment(&s)
    }

    /// Render folder + file templates into path segments, enforcing `max_length` on the file name.
    pub fn render_path(&self, profile: &Profile, ctx: &Json, ext: &str) -> Result<Rendered> {
        let mut ctx = ctx.clone();
        let ext_len = if ext.is_empty() { 0 } else { ext.len() + 1 };
        for _ in 0..64 {
            let r = self.render_once(profile, &ctx)?;
            let over = (r.stem.len() + ext_len).saturating_sub(self.max_length);
            if over == 0 {
                return Ok(r);
            }
            // Shorten the title first (then the episode title), never technical info.
            let shortened = ["title", "episode_title"].iter().any(|k| {
                let Some(Json::String(t)) = ctx.get(*k) else {
                    return false;
                };
                if t.is_empty() {
                    return false;
                }
                let keep = t.chars().count().saturating_sub(over.max(1));
                let t = truncate_chars(t, keep);
                ctx[*k] = Json::String(t);
                true
            });
            if !shortened {
                bail!(
                    "file name is {} bytes over max_length={} even with an empty title",
                    over,
                    self.max_length
                );
            }
        }
        bail!("could not fit file name in max_length={}", self.max_length)
    }

    fn render_once(&self, profile: &Profile, ctx: &Json) -> Result<Rendered> {
        let folder = self
            .render_raw(&profile.folder, ctx)
            .context("folder template")?;
        let file = self
            .render_raw(&profile.file, ctx)
            .context("file template")?;
        let mut parts: Vec<&str> = folder.split('/').chain(file.split('/')).collect();
        let stem = self.finish_segment(parts.pop().unwrap_or_default());
        if stem.is_empty() {
            bail!("file template rendered an empty name");
        }
        let dirs = parts
            .iter()
            .map(|s| collapse(&truncate_bytes(&self.finish_segment(s), self.max_length)))
            .filter(|s| !s.is_empty())
            .collect();
        Ok(Rendered { dirs, stem })
    }
}

/// Replace '/' and '\' in string values (except real paths) with SLASH.
fn protect_slashes(v: &mut Json, is_path: bool) {
    match v {
        Json::String(s) if !is_path => *s = s.replace(['/', '\\'], &SLASH.to_string()),
        Json::Array(a) => a.iter_mut().for_each(|x| protect_slashes(x, is_path)),
        Json::Object(o) => {
            for (k, x) in o.iter_mut() {
                protect_slashes(x, matches!(k.as_str(), "path" | "plex_path" | "root"));
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_cases() {
        assert_eq!(
            scene("Le Fabuleux Destin d'Amélie Poulain"),
            "Le.Fabuleux.Destin.d.Amelie.Poulain"
        );
        assert_eq!(scene("Ocean's Eleven"), "Oceans.Eleven");
        assert_eq!(scene("L’Île mystérieuse"), "L.Ile.mysterieuse");
        assert_eq!(
            scene("Star Wars: Episode IV - A New Hope"),
            "Star.Wars.Episode.IV.A.New.Hope"
        );
        assert_eq!(scene("  ...Face/Off!! "), "Face.Off");
        assert_eq!(
            scene("Qu'est-ce qu'on a fait au Bon Dieu ?"),
            "Qu.est.ce.qu.on.a.fait.au.Bon.Dieu"
        );
    }

    #[test]
    fn sanitize_cases() {
        assert_eq!(
            sanitize("Star Wars: Episode IV", " "),
            "Star Wars Episode IV"
        );
        assert_eq!(sanitize("What?*", " "), "What");
        assert_eq!(sanitize("CON", " "), "CON_");
        assert_eq!(sanitize("nul.txt", " "), "nul_.txt");
        assert_eq!(sanitize("Amélie", "."), "Amélie");
    }

    #[test]
    fn collapse_cases() {
        assert_eq!(
            collapse("Title.2001.576p..MPEG2.AC3-.FR"),
            "Title.2001.576p.MPEG2.AC3.FR"
        );
        assert_eq!(collapse("Show - S01E01 - "), "Show - S01E01");
        assert_eq!(collapse("Show -  - x"), "Show - x");
        assert_eq!(collapse("Mr. & Mrs. Smith ()"), "Mr. & Mrs. Smith");
        assert_eq!(collapse("Spider-Man.2002"), "Spider-Man.2002");
    }

    #[test]
    fn safe_segment_cases() {
        assert_eq!(
            safe_segment("Star Wars: A New Hope"),
            "Star Wars - A New Hope"
        );
        assert_eq!(safe_segment("Face\u{E000}Off"), "Face-Off");
        assert_eq!(safe_segment("Who? \"Me\""), "Who 'Me'");
    }
}
