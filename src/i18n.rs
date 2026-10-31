//! Two UI languages: Ukrainian (default) and English. Every user-visible string is written at its call
//! site in both languages (`t("…", "…")` / `tr!`), so a missing translation is a compile error, not a gap.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::types::Lang;

static EN: AtomicBool = AtomicBool::new(false);

pub fn set_lang(l: Lang) {
    EN.store(l == Lang::En, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if en() {
        Lang::En
    } else {
        Lang::Uk
    }
}

#[inline]
pub fn en() -> bool {
    EN.load(Ordering::Relaxed)
}

#[inline]
pub fn t(uk: &'static str, en_text: &'static str) -> &'static str {
    if en() {
        en_text
    } else {
        uk
    }
}

/// `tr!("Крок {} з {}", "Step {} of {}", a, b)` → formatted in the current UI language.
#[macro_export]
macro_rules! tr {
    ($uk:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::en() { format!($en $(, $arg)*) } else { format!($uk $(, $arg)*) }
    };
}

const MONTHS_UK: [&str; 12] = ["січ.", "лют.", "бер.", "квіт.", "трав.", "черв.", "лип.", "серп.", "вер.", "жовт.", "лист.", "груд."];
const MONTHS_EN: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn local(ms: i64) -> chrono::DateTime<chrono::Local> {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(ms).single().unwrap_or_else(chrono::Local::now)
}

/// Short date for lists: «31 жовт., 14:05» / «Oct 31, 14:05».
pub fn fmt_date(ms: i64) -> String {
    use chrono::{Datelike, Timelike};
    let d = local(ms);
    let m = d.month0() as usize;
    if en() {
        format!("{} {}, {:02}:{:02}", MONTHS_EN[m], d.day(), d.hour(), d.minute())
    } else {
        format!("{} {}, {:02}:{:02}", d.day(), MONTHS_UK[m], d.hour(), d.minute())
    }
}

/// Full local timestamp for session titles and exports.
pub fn fmt_datetime(ms: i64) -> String {
    let d = local(ms);
    if en() {
        d.format("%Y-%m-%d %H:%M:%S").to_string()
    } else {
        d.format("%d.%m.%Y, %H:%M:%S").to_string()
    }
}
