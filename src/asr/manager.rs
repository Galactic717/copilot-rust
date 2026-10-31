use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use tokio::sync::Notify;

use super::assets::{model_label, AsrAssets};
use super::silero::SileroGate;
use super::whisper::{apply_vocabulary, is_hallucination, strip_tags, transcribe, WhisperSidecar};
use crate::i18n::t;
use crate::net::Net;
use crate::types::*;

// Silero needs at least this much detected speech in a clip; below it the clip is music/noise/silence and never reaches whisper.
const MIN_SPEECH_S: f64 = 0.2;

pub struct AsrJob {
    pub session_id: String,
    pub utterance_id: String,
    pub source: Source,
    pub pcm: Vec<i16>,
    pub start_ms: i64,
    pub end_ms: i64,
    pub is_final: bool,
}

#[derive(Clone, PartialEq)]
pub struct AsrConfig {
    pub backend: AsrBackend,
    pub model: String,
    pub language: LangPref,
    pub vocabulary: Vec<String>,
    pub threads: u32,
}

impl From<&AsrSettings> for AsrConfig {
    fn from(a: &AsrSettings) -> Self {
        Self { backend: a.backend, model: a.model.clone(), language: a.language, vocabulary: a.vocabulary.clone(), threads: a.threads.max(1) }
    }
}

pub trait AsrEvents: Send + Sync {
    fn segment(&self, seg: TranscriptSegment);
    fn status(&self, st: &AsrStatus);
}

/// I serialize work for the single whisper-server. Finals are never dropped; a partial is superseded by any newer
/// partial or final of the same utterance, so the queue cannot grow.
pub struct AsrManager {
    assets: Arc<AsrAssets>,
    net: Arc<Net>,
    cfg: Mutex<AsrConfig>,
    st: Mutex<AsrStatus>,
    queue: Mutex<VecDeque<AsrJob>>,
    notify: Notify,
    sidecar: Mutex<Option<WhisperSidecar>>,
    ensure_lock: tokio::sync::Mutex<()>,
    running: AtomicBool,
    restarts: AtomicU32,
    ev: Arc<dyn AsrEvents>,
    silero: SileroGate,
    rt: tokio::runtime::Handle,
}

impl AsrManager {
    pub fn new(assets: Arc<AsrAssets>, net: Arc<Net>, cfg: AsrConfig, ev: Arc<dyn AsrEvents>, rt: tokio::runtime::Handle) -> Arc<Self> {
        let st = AsrStatus { binary: false, model: cfg.model.clone(), model_present: false, running: false, last_latency_ms: None, download: None, error: None };
        let me = Arc::new(Self {
            assets,
            net,
            cfg: Mutex::new(cfg),
            st: Mutex::new(st),
            queue: Mutex::new(VecDeque::new()),
            notify: Notify::new(),
            sidecar: Mutex::new(None),
            ensure_lock: tokio::sync::Mutex::new(()),
            running: AtomicBool::new(false),
            restarts: AtomicU32::new(0),
            ev,
            silero: SileroGate::default(),
            rt: rt.clone(),
        });
        me.refresh_status();
        let pump = me.clone();
        rt.spawn(async move { pump.pump().await });
        let watch = me.clone();
        rt.spawn(async move { watch.watchdog().await });
        me
    }

    pub fn status(&self) -> AsrStatus {
        self.st.lock().unwrap().clone()
    }

    pub fn config(&self) -> AsrConfig {
        self.cfg.lock().unwrap().clone()
    }

    pub fn set_config(self: &Arc<Self>, cfg: AsrConfig) {
        let restart = {
            let mut cur = self.cfg.lock().unwrap();
            let r = cfg.model != cur.model || cfg.backend != cur.backend || cfg.threads != cur.threads;
            *cur = cfg;
            r
        };
        if restart && self.running.load(Ordering::SeqCst) {
            self.stop();
            let me = self.clone();
            self.rt.spawn(async move { me.ensure().await });
        }
        self.refresh_status();
    }

    pub fn reset_session(&self) {
        self.queue.lock().unwrap().clear();
    }

    pub fn set_download(&self, d: Option<Download>, error: Option<String>) {
        {
            let mut st = self.st.lock().unwrap();
            st.download = d;
            st.error = error;
        }
        self.refresh_status();
    }

    pub fn refresh_status(&self) {
        let cfg = self.config();
        let st = {
            let mut st = self.st.lock().unwrap();
            st.model = cfg.model.clone();
            st.binary = self.assets.server_path(cfg.backend).is_some();
            st.model_present = self.assets.has_model(&cfg.model);
            st.running = self.running.load(Ordering::SeqCst);
            st.clone()
        };
        self.ev.status(&st);
    }

    fn set_error(&self, e: Option<String>) {
        self.st.lock().unwrap().error = e;
        self.refresh_status();
    }

    pub async fn ensure(self: &Arc<Self>) -> bool {
        let _g = self.ensure_lock.lock().await;
        if self.running.load(Ordering::SeqCst) {
            return true;
        }
        let cfg = self.config();
        let server = self.assets.server_path(cfg.backend);
        let Some(server) = server.filter(|_| self.assets.has_model(&cfg.model)) else {
            self.set_error(Some(if self.assets.server_path(cfg.backend).is_none() {
                t("Рушій розпізнавання не встановлено", "Speech engine is not installed").into()
            } else {
                crate::tr!("Немає моделі {}", "Model {} is missing", model_label(&cfg.model))
            }));
            return false;
        };
        match WhisperSidecar::start(server, self.assets.model_path(&cfg.model), cfg.threads, self.net.clone()).await {
            Ok(sc) => {
                let (net, port) = sc.endpoint();
                *self.sidecar.lock().unwrap() = Some(sc);
                self.running.store(true, Ordering::SeqCst);
                self.set_error(None);
                // GPU builds pay 13–27 s on the first request, so I warm it up before the user speaks.
                self.rt.spawn(async move {
                    let _ = transcribe(&net, port, &vec![0i16; 16000], "en").await;
                });
                true
            }
            Err(e) => {
                self.set_error(Some(e));
                false
            }
        }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(mut sc) = self.sidecar.lock().unwrap().take() {
            sc.stop();
        }
        self.queue.lock().unwrap().clear();
        self.refresh_status();
    }

    pub fn push(&self, job: AsrJob) {
        {
            let mut q = self.queue.lock().unwrap();
            q.retain(|j| j.is_final || j.utterance_id != job.utterance_id);
            if !job.is_final {
                q.retain(|j| j.is_final || j.source != job.source);
            }
            q.push_back(job);
            q.make_contiguous().sort_by_key(|j| !j.is_final);
        }
        self.notify.notify_one();
    }

    async fn watchdog(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let code = {
                let mut g = self.sidecar.lock().unwrap();
                let code = g.as_mut().and_then(|s| s.exited());
                if code.is_some() {
                    *g = None;
                }
                code
            };
            if let Some(code) = code {
                self.running.store(false, Ordering::SeqCst);
                crate::db::diag("asr_exit", json!({ "code": code }));
                self.set_error(Some(crate::tr!("Розпізнавання зупинилося (код {}) — перезапуск", "Recognition stopped (code {}) — restarting", code)));
                if self.restarts.fetch_add(1, Ordering::SeqCst) < 3 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    self.ensure().await;
                }
            }
        }
    }

    async fn pump(self: Arc<Self>) {
        loop {
            let job = self.queue.lock().unwrap().pop_front();
            let Some(job) = job else {
                self.notify.notified().await;
                continue;
            };
            if !self.ensure().await {
                if job.is_final {
                    self.emit(&job, String::new(), None);
                }
                continue;
            }
            // Silero speech gate: drop music/noise/silence before whisper so it can't hallucinate on them.
            if let Some(speech) = self.silero.speech_seconds(&job.pcm, 0.5) {
                if speech < MIN_SPEECH_S {
                    if job.is_final {
                        crate::db::diag("asr_nonspeech", json!({ "audioMs": job.pcm.len() / 16, "speechMs": (speech * 1000.0) as u64 }));
                        self.emit(&job, String::new(), None);
                    }
                    continue;
                }
            }
            let cfg = self.config();
            let endpoint = self.sidecar.lock().unwrap().as_ref().map(|s| s.endpoint());
            let Some((net, port)) = endpoint else {
                if job.is_final {
                    self.emit(&job, String::new(), None);
                }
                continue;
            };
            match transcribe(&net, port, &job.pcm, cfg.language.code()).await {
                Ok(r) => {
                    self.st.lock().unwrap().last_latency_ms = Some(r.ms);
                    if job.is_final {
                        self.refresh_status();
                        crate::db::diag(
                            "asr_latency",
                            json!({ "ms": r.ms, "audioMs": job.pcm.len() / 16, "lang": cfg.language.code(), "model": cfg.model, "backend": cfg.backend.as_str() }),
                        );
                    }
                    let garbage = is_hallucination(&r.text) || (r.no_speech > 0.6 && r.avg_logprob < -1.0);
                    let mut text = if garbage { String::new() } else { strip_tags(&r.text) };
                    if !text.is_empty() {
                        text = apply_vocabulary(&text, &cfg.vocabulary);
                    }
                    let lang = r.lang.filter(|l| l == "uk" || l == "en");
                    self.emit(&job, text, lang);
                }
                Err(e) => {
                    crate::db::diag("asr_error", json!({ "message": e }));
                    if job.is_final {
                        self.emit(&job, String::new(), None);
                    }
                }
            }
        }
    }

    fn emit(&self, job: &AsrJob, text: String, lang: Option<String>) {
        self.ev.segment(TranscriptSegment {
            session_id: job.session_id.clone(),
            segment_id: job.utterance_id.clone(),
            source: job.source,
            start_ms: job.start_ms,
            end_ms: job.end_ms,
            text,
            is_final: job.is_final,
            lang,
        });
    }
}
