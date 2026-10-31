use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::types::DocKind;

#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub id: String,
    pub document_id: String,
    pub doc_name: String,
    pub kind: DocKind,
    pub section: String,
    pub text: String,
}

static HEADING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(#{1,6}\s+.+|[A-ZА-ЯІЇЄҐ][A-ZА-ЯІЇЄҐ0-9 &/,-]{2,60}:?|.{2,60}:)$").unwrap());

/// I split on headings and blank lines into ~max_chars pieces, keeping the nearest heading as the section label.
pub fn chunk_text(text: &str, max_chars: usize) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut section = String::new();
    let mut buf: Vec<String> = Vec::new();
    let buf_len = |b: &Vec<String>| b.iter().map(|l| l.chars().count()).sum::<usize>() + b.len().saturating_sub(1);
    let flush = |buf: &mut Vec<String>, out: &mut Vec<(String, String)>, section: &str| {
        let t = buf.join("\n").trim().to_string();
        if !t.is_empty() {
            out.push((section.to_string(), t));
        }
        buf.clear();
    };
    for raw in text.replace('\r', "").split('\n') {
        let line = raw.trim_end();
        let trimmed = line.trim();
        let tlen = trimmed.chars().count();
        if tlen > 0 && tlen <= 60 && HEADING.is_match(trimmed) && !trimmed.ends_with(['.', '!', '?']) {
            flush(&mut buf, &mut out, &section);
            section = trimmed.trim_start_matches('#').trim().trim_end_matches(':').to_string();
            continue;
        }
        if trimmed.is_empty() {
            if buf_len(&buf) > max_chars / 2 {
                flush(&mut buf, &mut out, &section);
            }
            continue;
        }
        let llen = line.chars().count();
        if buf_len(&buf) + llen > max_chars {
            flush(&mut buf, &mut out, &section);
        }
        if llen > max_chars {
            // I prefer a break at whitespace; a run without any (minified text, long URLs) I cut hard so nothing is lost.
            for piece in split_long(line, max_chars) {
                buf.push(piece);
                flush(&mut buf, &mut out, &section);
            }
            continue;
        }
        buf.push(line.to_string());
    }
    flush(&mut buf, &mut out, &section);
    out
}

fn split_long(line: &str, max: usize) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let end = (i + max).min(chars.len());
        let cut = if end == chars.len() { end } else { (i..end).rev().find(|&k| chars[k].is_whitespace()).map(|k| k + 1).unwrap_or(end) };
        out.push(chars[i..cut].iter().collect());
        i = cut;
    }
    out
}

static STOP: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    "the a an and or of to in on for with is are was were be been it this that you your i my me we our what how why when where which who do does did can could would should about tell \
     і й та у в на з із до для про що як це цей ця ці ви ваш ваша ваші я мій моя мені ми наш чи не так але або же б би розкажіть розкажи опишіть \
     experience досвід досвіду роботи робота"
        .split_whitespace()
        .collect()
});

static TOKEN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\p{L}\p{N}+#.]+").unwrap());

/// Cheap stemmer: prefix truncation — enough for BM25 over Ukrainian morphology on a few pages of CV.
pub fn tokenize(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    TOKEN
        .find_iter(&lower)
        .map(|m| m.as_str().trim_matches('.').to_string())
        .filter(|w| w.chars().count() > 1 && !STOP.contains(w.as_str()))
        .map(|w| {
            let n = w.chars().count();
            let cyr = w.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c));
            if n > 6 && cyr {
                w.chars().take(6).collect()
            } else if n > 8 {
                w.chars().take(8).collect()
            } else {
                w
            }
        })
        .collect()
}

pub struct Bm25Index {
    docs: Vec<(Chunk, HashMap<String, usize>, usize)>,
    df: HashMap<String, usize>,
    avg_len: f64,
}

impl Bm25Index {
    pub fn new(chunks: Vec<Chunk>) -> Self {
        let mut df: HashMap<String, usize> = HashMap::new();
        let mut docs = Vec::new();
        for c in chunks {
            let toks = tokenize(&format!("{} {}", c.section, c.text));
            let mut tf: HashMap<String, usize> = HashMap::new();
            for t in &toks {
                *tf.entry(t.clone()).or_default() += 1;
            }
            for t in tf.keys() {
                *df.entry(t.clone()).or_default() += 1;
            }
            docs.push((c, tf, toks.len()));
        }
        let avg_len = docs.iter().map(|d| d.2).sum::<usize>() as f64 / docs.len().max(1) as f64;
        Self { docs, df, avg_len }
    }

    pub fn search(&self, query: &str, k: usize) -> Vec<(Chunk, f64)> {
        let q: HashSet<String> = tokenize(query).into_iter().collect();
        let n = self.docs.len() as f64;
        let (k1, b) = (1.2, 0.75);
        let mut scored: Vec<(Chunk, f64)> = self
            .docs
            .iter()
            .map(|(c, tf, len)| {
                let mut score = 0.0;
                for t in &q {
                    let Some(&f) = tf.get(t) else { continue };
                    let df = *self.df.get(t).unwrap_or(&0) as f64;
                    let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
                    let f = f as f64;
                    score += idf * f * (k1 + 1.0) / (f + k1 * (1.0 - b + b * *len as f64 / if self.avg_len > 0.0 { self.avg_len } else { 1.0 }));
                }
                (c.clone(), score)
            })
            .filter(|(_, s)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        scored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_line_without_spaces_keeps_every_char() {
        let line = "x".repeat(1515);
        assert_eq!(chunk_text(&line, 700).into_iter().map(|c| c.1).collect::<String>(), line);
    }

    #[test]
    fn headings_become_sections_and_bm25_finds_relevant() {
        let chunks = chunk_text("ДОСВІД РОБОТИ\nPython FastAPI PostgreSQL сервіс звітів\n\nОСВІТА\nКПІ, прикладна математика", 700);
        assert_eq!(chunks[0].0, "ДОСВІД РОБОТИ");
        let idx = Bm25Index::new(
            chunks
                .into_iter()
                .enumerate()
                .map(|(i, (section, text))| Chunk { id: i.to_string(), document_id: "d".into(), doc_name: "cv".into(), kind: DocKind::Cv, section, text })
                .collect(),
        );
        let r = idx.search("Який у вас досвід з PostgreSQL?", 3);
        assert!(r[0].0.text.contains("PostgreSQL"));
    }
}
