//! UI languages. Ukrainian and English are written at each call site (`t("…", "…")` / `tr!`), so a gap in those two is a
//! compile error. Every other language is a catalog in `locales/<code>.json` keyed by the English text: a missing entry
//! falls back to English, and `catalogs_cover_every_string` fails the build's tests if a shipped catalog has a hole.

use std::collections::HashMap;
use std::fmt::Display;
use std::sync::atomic::{AtomicPtr, AtomicU8, Ordering};
use std::sync::{LazyLock, Mutex};

use crate::types::Lang;

type Catalog = HashMap<&'static str, &'static str>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Uk,
    En,
    Other,
}

static MODE: AtomicU8 = AtomicU8::new(0);
static CATALOG: AtomicPtr<Catalog> = AtomicPtr::new(std::ptr::null_mut());
static CURRENT: Mutex<Lang> = Mutex::new(Lang::UK);
static LOADED: LazyLock<Mutex<HashMap<&'static str, &'static Catalog>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn catalog_source(code: &str) -> Option<&'static str> {
    Some(match code {
        "de" => include_str!("../locales/de.json"),
        "es" => include_str!("../locales/es.json"),
        "fr" => include_str!("../locales/fr.json"),
        "it" => include_str!("../locales/it.json"),
        "pt" => include_str!("../locales/pt.json"),
        "pl" => include_str!("../locales/pl.json"),
        "tr" => include_str!("../locales/tr.json"),
        "zh" => include_str!("../locales/zh.json"),
        "ja" => include_str!("../locales/ja.json"),
        "ko" => include_str!("../locales/ko.json"),
        _ => return None,
    })
}

/// Parsed once per language and kept for the life of the process: there are few languages and few switches.
fn load(l: Lang) -> Option<&'static Catalog> {
    let mut cache = LOADED.lock().unwrap();
    if let Some(c) = cache.get(l.code()) {
        return Some(c);
    }
    let parsed: HashMap<String, String> = serde_json::from_str(catalog_source(l.code())?).ok()?;
    let map: Catalog = parsed.into_iter().map(|(k, v)| (&*Box::leak(k.into_boxed_str()), &*Box::leak(v.into_boxed_str()))).collect();
    let map: &'static Catalog = Box::leak(Box::new(map));
    cache.insert(l.code(), map);
    Some(map)
}

pub fn set_lang(l: Lang) {
    *CURRENT.lock().unwrap() = l;
    let (mode, cat) = match l {
        Lang::UK => (Mode::Uk, None),
        Lang::EN => (Mode::En, None),
        _ => match load(l) {
            Some(c) => (Mode::Other, Some(c)),
            None => (Mode::En, None),
        },
    };
    CATALOG.store(cat.map_or(std::ptr::null_mut(), |c| c as *const Catalog as *mut Catalog), Ordering::Relaxed);
    MODE.store(mode as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    *CURRENT.lock().unwrap()
}

#[inline]
pub fn mode() -> Mode {
    match MODE.load(Ordering::Relaxed) {
        0 => Mode::Uk,
        1 => Mode::En,
        _ => Mode::Other,
    }
}

/// Not Ukrainian: picks the English side of content that has only those two versions (the screenshot demo).
pub fn en() -> bool {
    mode() != Mode::Uk
}

fn lookup(en_text: &'static str) -> &'static str {
    let p = CATALOG.load(Ordering::Relaxed);
    if p.is_null() {
        return en_text;
    }
    // Set only from `set_lang`, always to a leaked catalog.
    unsafe { &*p }.get(en_text).copied().unwrap_or(en_text)
}

#[inline]
pub fn t(uk: &'static str, en_text: &'static str) -> &'static str {
    match mode() {
        Mode::Uk => uk,
        Mode::En => en_text,
        Mode::Other => lookup(en_text),
    }
}

/// `tr!("Крок {} з {}", "Step {} of {}", a, b)` → formatted in the current UI language.
#[macro_export]
macro_rules! tr {
    ($uk:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        match $crate::i18n::mode() {
            $crate::i18n::Mode::Uk => format!($uk $(, $arg)*),
            $crate::i18n::Mode::En => format!($en $(, $arg)*),
            $crate::i18n::Mode::Other => $crate::i18n::render($en, &[$(&$arg as &dyn std::fmt::Display),*]),
        }
    };
}

/// Runtime `format!` for catalog templates: `{}`, `{1}` (a translation may reorder arguments), `{:.1}`, `{{`, `}}`.
pub fn render(en_text: &'static str, args: &[&dyn Display]) -> String {
    format_template(lookup(en_text), args)
}

fn format_template(tpl: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(tpl.len() + 16);
    let mut next = 0;
    let mut rest = tpl;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        let Some(close) = tail.find('}').filter(|_| tail.starts_with('{')) else {
            out.push_str(&tail[..1]);
            rest = &tail[1..];
            continue;
        };
        let (index, spec) = tail[1..close].split_once(':').unwrap_or((&tail[1..close], ""));
        let n = index.parse().unwrap_or_else(|_| {
            next += 1;
            next - 1
        });
        if let Some(a) = args.get(n) {
            match spec.strip_prefix('.').and_then(|p| p.parse::<usize>().ok()) {
                Some(prec) => out.push_str(&format!("{a:.prec$}")),
                None => out.push_str(&a.to_string()),
            }
        }
        rest = &tail[close + 1..];
    }
    out.push_str(rest);
    out
}

const MONTHS_UK: [&str; 12] = ["січ.", "лют.", "бер.", "квіт.", "трав.", "черв.", "лип.", "серп.", "вер.", "жовт.", "лист.", "груд."];
const MONTHS_EN: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn local(ms: i64) -> chrono::DateTime<chrono::Local> {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(ms).single().unwrap_or_else(chrono::Local::now)
}

/// Short date for lists: «31 жовт., 14:05» / «Oct 31, 14:05» / «31.10, 14:05».
pub fn fmt_date(ms: i64) -> String {
    use chrono::{Datelike, Timelike};
    let d = local(ms);
    let m = d.month0() as usize;
    match mode() {
        Mode::En => format!("{} {}, {:02}:{:02}", MONTHS_EN[m], d.day(), d.hour(), d.minute()),
        Mode::Uk => format!("{} {}, {:02}:{:02}", d.day(), MONTHS_UK[m], d.hour(), d.minute()),
        Mode::Other => d.format("%d.%m, %H:%M").to_string(),
    }
}

/// Full local timestamp for session titles and exports.
pub fn fmt_datetime(ms: i64) -> String {
    let d = local(ms);
    if mode() == Mode::En {
        d.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        d.format("%d.%m.%Y, %H:%M:%S").to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::Path;

    use regex::Regex;

    use super::*;
    use crate::types::LANGS;

    fn unescape(s: &str) -> String {
        s.replace("\\\"", "\"").replace("\\n", "\n").replace("\\\\", "\\")
    }

    fn walk(dir: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.path().is_dir() {
                walk(&e.path(), out);
            } else if e.path().extension().is_some_and(|x| x == "rs") {
                out.push(std::fs::read_to_string(e.path()).unwrap());
            }
        }
    }

    /// English side of every `t("…", "…")` and `tr!("…", "…")` in the source tree.
    fn source_strings() -> BTreeSet<String> {
        let re = Regex::new(r#"(?:\bt|tr!)\(\s*"((?:[^"\\]|\\.)*)"\s*,\s*"((?:[^"\\]|\\.)*)""#).unwrap();
        let mut files = Vec::new();
        walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        files.iter().flat_map(|f| re.captures_iter(f).map(|c| unescape(&c[2])).collect::<Vec<_>>()).filter(|k| k != "en_text").collect()
    }

    fn placeholders(s: &str) -> usize {
        let cleaned = s.replace("{{", "").replace("}}", "");
        cleaned.matches('{').count()
    }

    #[test]
    fn templates_render_like_format() {
        assert_eq!(format_template("Step {} of {}", &[&1, &2]), "Step 1 of 2");
        assert_eq!(format_template("{1} / {0}", &[&"a", &"b"]), "b / a");
        assert_eq!(format_template("first token {:.1} s", &[&1.26f64]), "first token 1.3 s");
        assert_eq!(format_template("{{literal}} {}", &[&"x"]), "{literal} x");
        assert_eq!(format_template("no args", &[]), "no args");
    }

    #[test]
    fn catalogs_cover_every_string() {
        let keys = source_strings();
        assert!(keys.len() > 500, "the source scan found only {} strings", keys.len());
        for l in LANGS.iter().filter(|l| l.ui && l.code != "uk" && l.code != "en") {
            let cat: HashMap<String, String> = serde_json::from_str(catalog_source(l.code).unwrap()).unwrap();
            let missing: Vec<&String> = keys.iter().filter(|k| !cat.contains_key(*k)).collect();
            assert!(missing.is_empty(), "{}: {} missing, first: {:?}", l.code, missing.len(), missing.first());
            for (k, v) in &cat {
                assert_eq!(placeholders(k), placeholders(v), "{}: placeholders differ in {k:?}", l.code);
                assert!(keys.contains(k), "{}: stale entry {k:?}", l.code);
            }
        }
    }

    #[test]
    fn no_catalog_is_russian() {
        assert!(LANGS.iter().all(|l| l.code != "ru"));
        assert_eq!(Lang::parse("ru"), Some(Lang::UK), "old settings migrate to Ukrainian");
        assert_eq!(Lang::from_system("ru-RU"), Lang::EN, "a Russian Windows gets English, not Russian");
        assert_eq!(Lang::from_system("de-AT"), Lang::parse("de").unwrap());
        assert_eq!(Lang::from_system("nl-NL"), Lang::EN, "no UI catalog for it yet");
    }
}
