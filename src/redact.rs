use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use regex::Regex;

static KNOWN: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

static PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"sk-(ant-|proj-|or-v1-)?[A-Za-z0-9_-]{16,}",
        r"AIza[0-9A-Za-z_-]{30,}",
        r"gsk_[A-Za-z0-9]{20,}",
        r"xai-[A-Za-z0-9]{20,}",
        r"(?i)(Bearer|x-api-key[:=]|x-goog-api-key[:=])\s*[A-Za-z0-9._-]{12,}",
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap())
    .collect()
});

static QUERY_KEY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)([?&](key|api_key|token)=)[^&\s]+").unwrap());

pub fn register_secret(value: &str) {
    if value.len() >= 8 {
        KNOWN.lock().unwrap().insert(value.to_string());
    }
}

pub fn unregister_secret(value: &str) {
    KNOWN.lock().unwrap().remove(value);
}

/// I apply this to everything that leaves the process as text: logs, diagnostics, exports, error details.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for s in KNOWN.lock().unwrap().iter() {
        if out.contains(s.as_str()) {
            out = out.replace(s.as_str(), "[secret]");
        }
    }
    for p in PATTERNS.iter() {
        out = p.replace_all(&out, "[secret]").into_owned();
    }
    QUERY_KEY.replace_all(&out, "${1}[secret]").into_owned()
}

pub fn redact_json(v: &serde_json::Value) -> serde_json::Value {
    serde_json::from_str(&redact(&v.to_string())).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_known_and_pattern_secrets() {
        register_secret("my-very-secret-value");
        let s = redact("key my-very-secret-value and sk-proj-abcdefghijklmnopqrstu and https://x/?key=AAA&b=1 Bearer abcdefghijklmnop");
        assert!(!s.contains("my-very-secret-value"));
        assert!(!s.contains("abcdefghijklmnopqrstu"));
        assert!(s.contains("?key=[secret]&b=1"));
        assert!(!s.contains("abcdefghijklmnop"));
    }
}
