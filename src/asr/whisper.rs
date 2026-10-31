use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::Value;

use crate::net::Net;

pub struct WhisperResult {
    pub text: String,
    pub lang: Option<String>,
    pub no_speech: f64,
    pub avg_logprob: f64,
    pub ms: u64,
}

// Whisper's well-known hallucinations on silence/noise, learned from UA/EN subtitles.
static HALLUCINATIONS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)^(thank you( so much)? for watching|thanks for watching|please subscribe|subtitles by)",
        r"(?i)^(дякую за перегляд|субтитри|підписуйтесь|редактор субтитрів)",
        r"^[.\s…♪]*$",
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap())
    .collect()
});

// On music/noise whisper emits non-speech annotations in brackets/parens/asterisks or musical notes,
// e.g. "[музика]", "(applause)", "[BLANK_AUDIO]", "♪♪". Real speech almost never carries them, so I strip them.
static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\[({（【][^\])}）】]*[\])}）】]|\*[^*]+\*|[♪♫🎶🎵]+").unwrap());
static WS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());
static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\p{L}[\p{L}\p{N}]*").unwrap());

pub fn strip_tags(text: &str) -> String {
    WS.replace_all(&TAG.replace_all(text, " "), " ").trim().to_string()
}

pub fn is_hallucination(text: &str) -> bool {
    let t = text.trim().trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | '!' | '-'));
    if t.is_empty() || strip_tags(t).is_empty() {
        return true;
    }
    HALLUCINATIONS.iter().any(|r| r.is_match(t))
}

fn norm(w: &str) -> String {
    w.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

fn lev(a: &[char], b: &[char]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    let mut d: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut prev = d[0];
        d[0] = i;
        for j in 1..=b.len() {
            let tmp = d[j];
            d[j] = (d[j] + 1).min(d[j - 1] + 1).min(prev + usize::from(a[i - 1] != b[j - 1]));
            prev = tmp;
        }
    }
    d[b.len()]
}

// Prompting whisper with the vocabulary made whisper.cpp regurgitate it on noise (phantom "Docker"/"Kubernetes").
// Instead I fix spelling/casing of a known term the model ALREADY produced; a fuzzy match can only rewrite a word
// that was recognized, never insert one that was not spoken. Short terms match on casing only.
pub fn apply_vocabulary(text: &str, vocab: &[String]) -> String {
    let terms: Vec<(&str, Vec<char>)> = vocab.iter().map(|t| (t.as_str(), norm(t).chars().collect::<Vec<_>>())).filter(|(_, k)| k.len() >= 3).collect();
    if terms.is_empty() {
        return text.to_string();
    }
    WORD.replace_all(text, |c: &regex::Captures| {
        let word = &c[0];
        let k: Vec<char> = norm(word).chars().collect();
        if k.len() < 3 {
            return word.to_string();
        }
        for (canon, key) in &terms {
            if &k == key {
                return canon.to_string();
            }
            if key.len() >= 6 && k.len().abs_diff(key.len()) <= 2 {
                let d = lev(&k, key);
                if d > 0 && d <= key.len() * 2 / 10 {
                    return canon.to_string();
                }
            }
        }
        word.to_string()
    })
    .into_owned()
}

pub fn encode_wav(pcm: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (pcm.len() * 2) as u32;
    let mut v = Vec::with_capacity(44 + data_len as usize);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&rate.to_le_bytes());
    v.extend_from_slice(&(rate * 2).to_le_bytes());
    v.extend_from_slice(&2u16.to_le_bytes());
    v.extend_from_slice(&16u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

pub struct WhisperSidecar {
    child: Child,
    port: u16,
    net: Arc<Net>,
}

impl WhisperSidecar {
    pub async fn start(server: PathBuf, model: PathBuf, threads: u32, net: Arc<Net>) -> Result<Self, String> {
        let port = crate::proc::free_port().map_err(|e| e.to_string())?;
        let mut cmd = Command::new(&server);
        cmd.arg("-m").arg(&model).args(["--host", "127.0.0.1", "--port", &port.to_string(), "-t", &threads.to_string()]);
        if let Some(d) = server.parent() {
            cmd.current_dir(d);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
        let mut child = crate::proc::spawn_hidden(&mut cmd).map_err(|e| e.to_string())?;
        if let Some(err) = child.stderr.take() {
            std::thread::spawn(move || {
                use std::io::BufRead;
                for line in std::io::BufReader::new(err).lines().map_while(Result::ok) {
                    let l = line.to_lowercase();
                    if l.contains("error") || l.contains("fail") || l.contains("cuda") || l.contains("gpu") {
                        crate::db::diag("asr_log", Value::from(line.chars().take(300).collect::<String>()));
                    }
                }
            });
        }
        let mut sc = Self { child, port, net };
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            if !sc.running() {
                return Err(crate::i18n::t("whisper-server завершився під час запуску", "whisper-server exited during startup").into());
            }
            if let Ok(rb) = sc.net.get(&format!("http://127.0.0.1:{port}/")) {
                if let Ok(r) = rb.timeout(Duration::from_secs(1)).send().await {
                    if r.status().as_u16() < 500 {
                        return Ok(sc);
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        sc.stop();
        Err(crate::i18n::t("whisper-server не відповів за 60 с", "whisper-server did not respond within 60 s").into())
    }

    pub fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Exit code if the process has died.
    pub fn exited(&mut self) -> Option<i32> {
        self.child.try_wait().ok().flatten().map(|s| s.code().unwrap_or(-1))
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn endpoint(&self) -> (Arc<Net>, u16) {
        (self.net.clone(), self.port)
    }
}

impl Drop for WhisperSidecar {
    fn drop(&mut self) {
        self.stop();
    }
}

pub async fn transcribe(net: &Net, port: u16, pcm: &[i16], language: &str) -> Result<WhisperResult, String> {
    let t0 = Instant::now();
    let part = reqwest::multipart::Part::bytes(encode_wav(pcm, 16000)).file_name("audio.wav").mime_str("audio/wav").map_err(|e| e.to_string())?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("response_format", "verbose_json")
        .text("language", language.to_string())
        .text("temperature", "0");
    let res = net
        .post(&format!("http://127.0.0.1:{port}/inference"))
        .map_err(|e| e.line())?
        .multipart(form)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("whisper HTTP {}", res.status().as_u16()));
    }
    let j: Value = res.json().await.map_err(|e| e.to_string())?;
    let segs = j.get("segments").and_then(Value::as_array).cloned().unwrap_or_default();
    let no_speech = segs.iter().map(|s| s.get("no_speech_prob").and_then(Value::as_f64).unwrap_or(0.0)).fold(f64::INFINITY, f64::min);
    let avg = if segs.is_empty() { 0.0 } else { segs.iter().map(|s| s.get("avg_logprob").and_then(Value::as_f64).unwrap_or(0.0)).sum::<f64>() / segs.len() as f64 };
    Ok(WhisperResult {
        text: WS.replace_all(j.get("text").and_then(Value::as_str).unwrap_or(""), " ").trim().to_string(),
        lang: j.get("detected_language").or_else(|| j.get("language")).and_then(Value::as_str).map(String::from),
        no_speech: if segs.is_empty() { 0.0 } else { no_speech },
        avg_logprob: avg,
        ms: t0.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_hallucinations_vocabulary() {
        assert_eq!(strip_tags("[музика] Привіт (applause) світ ♪♪"), "Привіт світ");
        assert!(is_hallucination("[BLANK_AUDIO]"));
        assert!(is_hallucination("Дякую за перегляд!"));
        assert!(is_hallucination(" ... "));
        assert!(!is_hallucination("Розкажіть про Kafka"));
        let vocab: Vec<String> = ["PostgreSQL", "Kubernetes", "React"].map(String::from).to_vec();
        assert_eq!(apply_vocabulary("ми використовуємо postgresql і kubernetis", &vocab), "ми використовуємо PostgreSQL і Kubernetes");
        assert_eq!(apply_vocabulary("it reacts fast", &vocab), "it reacts fast");
    }

    /// Live check (downloads whisper.cpp + base model once):
    /// `COPILOT_ASR_DIR=<dir> COPILOT_TEST_WAV=<16k mono wav> cargo test live_whisper -- --ignored`
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn live_whisper_end_to_end() {
        use crate::asr::assets::AsrAssets;
        use crate::types::AsrBackend;
        let assets = AsrAssets::new(std::env::var("COPILOT_ASR_DIR").expect("COPILOT_ASR_DIR").into());
        let net = Net::new();
        let progress = |_d: crate::types::Download| {};
        if assets.server_path(AsrBackend::Cpu).is_none() {
            assets.download_binary(&net, AsrBackend::Cpu, &progress).await.expect("binary (sha256 checked)");
        }
        if !assets.has_model("base") {
            assets.download_model(&net, "base", &progress).await.expect("model (sha256 checked)");
        }
        let wav = std::fs::read(std::env::var("COPILOT_TEST_WAV").expect("COPILOT_TEST_WAV")).unwrap();
        let pcm: Vec<i16> = wav[44..].as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
        let speech = crate::asr::silero::SileroGate::default().speech_seconds(&pcm, 0.5).unwrap();
        assert!(speech > 1.0, "silero heard {speech} s of speech");
        let sc = WhisperSidecar::start(assets.server_path(AsrBackend::Cpu).unwrap(), assets.model_path("base"), 4, net.clone()).await.expect("whisper-server starts");
        let (net, port) = sc.endpoint();
        let r = transcribe(&net, port, &pcm, "en").await.expect("transcribe");
        println!("whisper: {:?} in {} ms", r.text, r.ms);
        assert!(r.text.to_lowercase().contains("index"), "{}", r.text);
        assert!(!is_hallucination(&r.text));
    }

    #[test]
    fn wav_header() {
        let w = encode_wav(&[0, 1, -1], 16000);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 16000);
    }
}
