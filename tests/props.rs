use plexfilerenamer::template::{collapse, safe_segment, sanitize, scene};
use proptest::prelude::*;

const FORBIDDEN: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn scene_is_clean_and_idempotent(s in "\\PC{0,40}|[a-zA-Z' .:/-]{0,30}") {
        let r = scene(&s);
        prop_assert!(r.chars().all(|c| c.is_ascii_alphanumeric() || c == '.'), "{r:?}");
        prop_assert!(!r.contains(".."), "{r:?}");
        prop_assert!(!r.starts_with('.') && !r.ends_with('.'), "{r:?}");
        prop_assert_eq!(scene(&r), r);
    }

    #[test]
    fn sanitize_is_clean_and_idempotent(s in "\\PC{0,40}|[a-zA-Z<>:\"/\\\\|?* .\t]{0,30}|(CON|nul|Com1)(\\.[a-z]{1,3})?", sep in prop::sample::select(vec![" ", ".", "-", "_"])) {
        let r = sanitize(&s, sep);
        prop_assert!(!r.chars().any(|c| FORBIDDEN.contains(&c) || c.is_control()), "{r:?}");
        prop_assert!(!r.contains(&format!("{sep}{sep}")), "{r:?}");
        prop_assert!(!r.contains("  "), "{r:?}");
        prop_assert!(!r.ends_with('.') && !r.ends_with(' '), "{r:?}");
        let stem = r.split('.').next().unwrap().to_uppercase();
        prop_assert!(!["CON", "NUL", "COM1"].contains(&stem.as_str()), "{r:?}");
        prop_assert_eq!(sanitize(&r, sep), r);
    }

    #[test]
    fn segments_are_always_safe(s in "\\PC{0,60}") {
        let r = safe_segment(&s);
        prop_assert!(!r.chars().any(|c| FORBIDDEN.contains(&c) || c.is_control()), "{r:?}");
        prop_assert!(!r.ends_with('.') && !r.ends_with(' '), "{r:?}");
        prop_assert!(!r.contains("..") && !r.contains("--") && !r.contains("  "), "{r:?}");
        prop_assert_eq!(collapse(&r), r.clone());
    }
}
