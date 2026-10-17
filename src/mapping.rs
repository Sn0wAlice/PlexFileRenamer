//! Lookup tables: codecs, languages, resolution buckets, channel labels.
//! Every table can be extended/overridden from `[mapping.*]` in the config.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Mapping {
    /// Plex video codec -> label.
    pub vcodec: BTreeMap<String, String>,
    /// Plex audio codec -> label.
    pub acodec: BTreeMap<String, String>,
    /// Language code (any form) -> label. Wins over `language_style`.
    pub languages: BTreeMap<String, String>,
    /// Bucket label -> nominal height. Width is folded in (scope/4:3 aware).
    pub resolutions: BTreeMap<String, u32>,
    /// Channel count -> label.
    pub channels: BTreeMap<String, String>,
    /// Default style for `lang`/`langs`: iso1 (FR), iso2 (FRE), name (French), native (Français).
    pub language_style: String,
}

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

impl Default for Mapping {
    fn default() -> Self {
        Mapping {
            vcodec: map(&[
                ("mpeg1video", "MPEG1"),
                ("mpeg2video", "MPEG2"),
                ("mpeg4", "MPEG4"),
                ("msmpeg4v3", "DivX"),
                ("h264", "H264"),
                ("hevc", "H265"),
                ("h265", "H265"),
                ("vc1", "VC1"),
                ("av1", "AV1"),
                ("vp8", "VP8"),
                ("vp9", "VP9"),
            ]),
            acodec: map(&[
                ("aac", "AAC"),
                ("ac3", "AC3"),
                ("eac3", "EAC3"),
                ("dca", "DTS"),
                ("dts", "DTS"),
                ("truehd", "TrueHD"),
                ("flac", "FLAC"),
                ("mp2", "MP2"),
                ("mp3", "MP3"),
                ("opus", "Opus"),
                ("vorbis", "Vorbis"),
                ("pcm", "PCM"),
                ("lpcm", "LPCM"),
            ]),
            languages: BTreeMap::new(),
            resolutions: [
                ("480p", 480),
                ("576p", 576),
                ("720p", 720),
                ("1080p", 1080),
                ("2160p", 2160),
            ]
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect(),
            channels: map(&[
                ("1", "1.0"),
                ("2", "2.0"),
                ("3", "2.1"),
                ("4", "4.0"),
                ("5", "5.0"),
                ("6", "5.1"),
                ("7", "6.1"),
                ("8", "7.1"),
            ]),
            language_style: "iso1".into(),
        }
    }
}

/// (iso 639-1, iso 639-2/B, iso 639-2/T, English name, native name)
const LANGS: &[(&str, &str, &str, &str, &str)] = &[
    ("fr", "fre", "fra", "French", "Français"),
    ("en", "eng", "eng", "English", "English"),
    ("de", "ger", "deu", "German", "Deutsch"),
    ("es", "spa", "spa", "Spanish", "Español"),
    ("it", "ita", "ita", "Italian", "Italiano"),
    ("pt", "por", "por", "Portuguese", "Português"),
    ("nl", "dut", "nld", "Dutch", "Nederlands"),
    ("ja", "jpn", "jpn", "Japanese", "日本語"),
    ("zh", "chi", "zho", "Chinese", "中文"),
    ("ko", "kor", "kor", "Korean", "한국어"),
    ("ru", "rus", "rus", "Russian", "Русский"),
    ("ar", "ara", "ara", "Arabic", "العربية"),
    ("he", "heb", "heb", "Hebrew", "עברית"),
    ("hi", "hin", "hin", "Hindi", "हिन्दी"),
    ("tr", "tur", "tur", "Turkish", "Türkçe"),
    ("pl", "pol", "pol", "Polish", "Polski"),
    ("cs", "cze", "ces", "Czech", "Čeština"),
    ("sk", "slo", "slk", "Slovak", "Slovenčina"),
    ("hu", "hun", "hun", "Hungarian", "Magyar"),
    ("ro", "rum", "ron", "Romanian", "Română"),
    ("el", "gre", "ell", "Greek", "Ελληνικά"),
    ("sv", "swe", "swe", "Swedish", "Svenska"),
    ("da", "dan", "dan", "Danish", "Dansk"),
    ("no", "nor", "nor", "Norwegian", "Norsk"),
    ("nb", "nob", "nob", "Norwegian Bokmål", "Norsk bokmål"),
    ("fi", "fin", "fin", "Finnish", "Suomi"),
    ("is", "ice", "isl", "Icelandic", "Íslenska"),
    ("uk", "ukr", "ukr", "Ukrainian", "Українська"),
    ("bg", "bul", "bul", "Bulgarian", "Български"),
    ("hr", "hrv", "hrv", "Croatian", "Hrvatski"),
    ("sr", "srp", "srp", "Serbian", "Српски"),
    ("sl", "slv", "slv", "Slovenian", "Slovenščina"),
    ("et", "est", "est", "Estonian", "Eesti"),
    ("lv", "lav", "lav", "Latvian", "Latviešu"),
    ("lt", "lit", "lit", "Lithuanian", "Lietuvių"),
    ("ca", "cat", "cat", "Catalan", "Català"),
    ("eu", "baq", "eus", "Basque", "Euskara"),
    ("gl", "glg", "glg", "Galician", "Galego"),
    ("br", "bre", "bre", "Breton", "Brezhoneg"),
    ("co", "cos", "cos", "Corsican", "Corsu"),
    ("oc", "oci", "oci", "Occitan", "Occitan"),
    ("ga", "gle", "gle", "Irish", "Gaeilge"),
    ("cy", "wel", "cym", "Welsh", "Cymraeg"),
    ("th", "tha", "tha", "Thai", "ไทย"),
    ("vi", "vie", "vie", "Vietnamese", "Tiếng Việt"),
    ("id", "ind", "ind", "Indonesian", "Bahasa Indonesia"),
    ("ms", "may", "msa", "Malay", "Bahasa Melayu"),
    ("fa", "per", "fas", "Persian", "فارسی"),
    ("ta", "tam", "tam", "Tamil", "தமிழ்"),
    ("la", "lat", "lat", "Latin", "Latina"),
];

fn find_lang(
    code: &str,
) -> Option<&'static (
    &'static str,
    &'static str,
    &'static str,
    &'static str,
    &'static str,
)> {
    let c = code.trim().to_lowercase();
    // Plex sometimes stores regional tags like "pt-BR".
    let base = c.split(['-', '_']).next().unwrap_or("");
    LANGS.iter().find(|l| {
        [l.0, l.1, l.2].contains(&base) || l.3.to_lowercase() == c || l.4.to_lowercase() == c
    })
}

impl Mapping {
    pub fn vcodec(&self, c: &str) -> String {
        lookup(&self.vcodec, c)
    }

    pub fn acodec(&self, c: &str) -> String {
        lookup(&self.acodec, c)
    }

    pub fn channels_label(&self, n: u32) -> String {
        match self.channels.get(&n.to_string()) {
            Some(l) => l.clone(),
            None if n == 0 => String::new(),
            None => format!("{n}ch"),
        }
    }

    /// Equivalent height = max(height, width*9/16) so 1920x800 is 1080p and 720x480 stays 480p.
    pub fn resolution(&self, width: u32, height: u32) -> String {
        if width == 0 && height == 0 {
            return String::new();
        }
        let eq = height.max(width * 9 / 16) as f64;
        let mut buckets: Vec<_> = self.resolutions.iter().collect();
        buckets.sort_by_key(|(_, h)| **h);
        buckets
            .iter()
            .rev()
            .find(|(_, h)| eq >= **h as f64 * 0.9)
            .or(buckets.first())
            .map(|(l, _)| l.to_string())
            .unwrap_or_default()
    }

    /// Render a language code in `style` (None = `language_style`). Empty for und/unknown-empty.
    pub fn lang(&self, code: &str, style: Option<&str>) -> String {
        let c = code.trim();
        if c.is_empty() || matches!(c.to_lowercase().as_str(), "und" | "unk" | "zxx" | "mis") {
            return String::new();
        }
        let known = find_lang(c);
        // Custom map: raw code first, then any canonical form of it.
        let mut keys = vec![c.to_lowercase()];
        if let Some(l) = known {
            keys.extend([l.0, l.1, l.2].iter().map(|s| s.to_string()));
        }
        for k in &keys {
            if let Some(v) = self.languages.get(k) {
                return v.clone();
            }
        }
        let style = style.unwrap_or(&self.language_style);
        match (known, style) {
            (Some(l), "iso1") => l.0.to_uppercase(),
            (Some(l), "iso2") => l.1.to_uppercase(),
            (Some(l), "iso2t") => l.2.to_uppercase(),
            (Some(l), "name") => l.3.to_string(),
            (Some(l), "native") => l.4.to_string(),
            (Some(l), _) => l.0.to_uppercase(),
            (None, _) => c.to_uppercase(),
        }
    }
}

fn lookup(m: &BTreeMap<String, String>, c: &str) -> String {
    let k = c.trim().to_lowercase();
    m.get(&k).cloned().unwrap_or_else(|| k.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_buckets() {
        let m = Mapping::default();
        for (w, h, want) in [
            (720, 576, "576p"),
            (720, 480, "480p"),
            (704, 576, "576p"),
            (1920, 800, "1080p"),
            (1916, 1036, "1080p"),
            (1440, 1080, "1080p"),
            (1280, 536, "720p"),
            (3840, 1600, "2160p"),
            (320, 240, "480p"),
        ] {
            assert_eq!(m.resolution(w, h), want, "{w}x{h}");
        }
    }

    #[test]
    fn languages() {
        let mut m = Mapping::default();
        assert_eq!(m.lang("fre", None), "FR");
        assert_eq!(m.lang("fra", Some("iso2")), "FRE");
        assert_eq!(m.lang("fr", Some("native")), "Français");
        assert_eq!(m.lang("pt-BR", Some("name")), "Portuguese");
        assert_eq!(m.lang("und", None), "");
        assert_eq!(m.lang("xyz", None), "XYZ");
        m.languages.insert("fre".into(), "VFF".into());
        assert_eq!(m.lang("fr", None), "VFF");
    }

    #[test]
    fn codecs_and_channels() {
        let m = Mapping::default();
        assert_eq!(m.vcodec("mpeg2video"), "MPEG2");
        assert_eq!(m.acodec("dca"), "DTS");
        assert_eq!(m.acodec("weird"), "WEIRD");
        assert_eq!(m.channels_label(6), "5.1");
        assert_eq!(m.channels_label(10), "10ch");
    }
}
