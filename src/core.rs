//! The app core: owns storage, providers, session, ASR, capture and the window side effects. The UI calls into it;
//! hotkeys and the tray reach it through the UI thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use base64::Engine;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::asr::assets::AsrAssets;
use crate::asr::manager::{AsrConfig, AsrEvents, AsrJob, AsrManager};
use crate::audio::{AudioChunk, CaptureController, CaptureOptions, CaptureSink};
use crate::budget::{BudgetManager, Gate};
use crate::context::prompt::{debrief_task, detect_lang, feedback_task, mock_question_task, translate_task, ScreenInput, Task};
use crate::db::{Db, HistoryDetail};
use crate::discover::{self, Found, ModelOffer, Places};
use crate::i18n::t;
use crate::llama::{LlamaManager, LlamaState};
use crate::net::{check_endpoint, Net};
use crate::providers::presets::preset;
use crate::providers::registry::{ProviderRegistry, ProviderTestReport, ResolvedTarget};
use crate::providers::runner::{run_stream, RunEvent, RunOptions};
use crate::screen::Shot;
use crate::secrets::SecretStore;
use crate::session::manager::{SessionDeps, SessionManager};
use crate::settings::SettingsStore;
use crate::tr;
use crate::types::*;
use crate::update::{self, Release};
use crate::win::{self, Rect};

#[derive(Default)]
pub struct PickerState {
    pub shot: Option<Arc<Shot>>,
    pub generation: u64,
    pub has_vision: bool,
    pub ocr: bool,
    overlay_was_visible: bool,
    protected_overlay: bool,
}

#[derive(Default)]
pub struct UiBridge {
    pub ctx: OnceLock<eframe::egui::Context>,
    pub picker: Mutex<PickerState>,
    pub overlay_scroll: Mutex<f32>,
    pub shortcuts_dirty: AtomicBool,
}

impl UiBridge {
    pub fn repaint(&self) {
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
            ctx.request_repaint_of(crate::ui::overlay_id());
        }
    }
}

/// A streamed one-off generation the UI can draw while it runs.
#[derive(Default, Clone)]
pub struct Gen {
    pub text: String,
    pub busy: bool,
    pub error: Option<String>,
}

pub type GenSlot = Arc<Mutex<Gen>>;

#[derive(Default)]
pub struct LocalInstall {
    pub busy: bool,
    pub download: Option<Download>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct UpdateState {
    pub available: Option<Release>,
    pub busy: bool,
    pub download: Option<Download>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct MockState {
    pub question: GenSlot,
    pub feedback: GenSlot,
}

pub struct Core {
    pub data_dir: PathBuf,
    pub rt: tokio::runtime::Runtime,
    pub settings: Arc<SettingsStore>,
    pub db: Arc<Db>,
    pub secrets: Arc<SecretStore>,
    pub net: Arc<Net>,
    pub registry: Arc<ProviderRegistry>,
    pub budget: Arc<Mutex<BudgetManager>>,
    pub session: Arc<SessionManager>,
    pub asr: Arc<AsrManager>,
    pub assets: Arc<AsrAssets>,
    pub capture: CaptureController,
    pub llama: LlamaManager,
    pub interrupted: usize,
    pub shortcut_conflicts: Mutex<Vec<String>>,
    pub ui: UiBridge,
    /// Local models found on this machine; None until the first scan finishes.
    pub found: Mutex<Option<Vec<Found>>>,
    pub scanning: AtomicBool,
    pub local_install: Mutex<LocalInstall>,
    pub update: Mutex<UpdateState>,
    pub debriefs: Mutex<HashMap<String, GenSlot>>,
    pub mock: MockState,
    ocr: Mutex<Option<bool>>,
    asr_idle: AtomicU64,
    me: Weak<Core>,
}

struct Deps {
    settings: Arc<SettingsStore>,
    db: Arc<Db>,
    registry: Arc<ProviderRegistry>,
    budget: Arc<Mutex<BudgetManager>>,
    core: Weak<Core>,
}

impl SessionDeps for Deps {
    fn settings(&self) -> Settings {
        self.settings.get()
    }
    fn db(&self) -> &Db {
        &self.db
    }
    fn profile(&self) -> Profile {
        active_profile(&self.db, &self.settings.get())
    }
    fn resolve(&self, role: ModelRole) -> Option<ResolvedTarget> {
        self.registry.resolve(role, &self.settings.get())
    }
    fn fallback(&self, role: ModelRole, exclude: &str) -> Option<ResolvedTarget> {
        self.registry.fallback(role, exclude, &self.settings.get())
    }
    fn budget(&self) -> &Mutex<BudgetManager> {
        &self.budget
    }
    fn emit(&self) {
        if let Some(c) = self.core.upgrade() {
            c.ui.repaint();
        }
    }
    fn log(&self, kind: &str, data: Value) {
        crate::db::diag(kind, data);
    }
    fn on_question(&self, q: &QuestionEvent) {
        if let Some(c) = self.core.upgrade() {
            c.translate_question(q);
        }
    }
    fn outcome(&self, provider_id: &str, model: &str, ok: bool) {
        let key = format!("{provider_id}/{model}");
        if self.registry.text_ok.lock().unwrap().insert(key, ok) != Some(ok) {
            if let Some(c) = self.core.upgrade() {
                c.update_llm_status();
            }
        }
    }
}

fn active_profile(db: &Db, s: &Settings) -> Profile {
    let all = db.list_profiles();
    all.iter().find(|p| p.id == s.active_profile_id).or(all.first()).cloned().unwrap_or(Profile {
        id: "default".into(),
        name: t("Основний", "Main").into(),
        preset: ProfilePreset::Backend,
        style: AnswerStyle::Natural,
        notes: String::new(),
    })
}

struct Bridge(Weak<Core>);

impl AsrEvents for Bridge {
    fn segment(&self, seg: TranscriptSegment) {
        if let Some(c) = self.0.upgrade() {
            c.session.on_segment(seg);
        }
    }
    fn status(&self, st: &AsrStatus) {
        let Some(c) = self.0.upgrade() else { return };
        let s = if let Some(d) = &st.download {
            ComponentStatus::new(ComponentState::Busy, tr!("завантаження {}: {}%", "downloading {}: {}%", d.name, d.received * 100 / d.total.max(1)))
        } else if let Some(e) = &st.error {
            ComponentStatus::new(ComponentState::Error, e.clone())
        } else if !st.binary || !st.model_present {
            ComponentStatus::new(ComponentState::Warn, t("не встановлено", "not installed"))
        } else if st.running {
            let ms = st.last_latency_ms.map(|m| format!(" · {m} {}", t("мс", "ms"))).unwrap_or_default();
            ComponentStatus::new(ComponentState::Ok, format!("whisper {} · {}{ms}", st.model, t("локально", "local")))
        } else {
            ComponentStatus::new(ComponentState::Ok, format!("whisper {} · {}", st.model, t("встановлено", "installed")))
        };
        c.session.set_status(StatusKey::Asr, s);
    }
}

impl CaptureSink for Bridge {
    fn audio(&self, m: AudioChunk) {
        let Some(c) = self.0.upgrade() else { return };
        if c.session.session_id().as_deref() != Some(m.session_id.as_str()) || !c.session.listening() {
            return;
        }
        let started = c.session.started_at();
        c.asr.push(AsrJob {
            session_id: m.session_id,
            utterance_id: m.utterance_id,
            source: m.source,
            pcm: m.pcm,
            start_ms: (m.start_at - started).max(0),
            end_ms: (m.end_at - started).max(0),
            is_final: m.is_final,
        });
    }
    fn level(&self, source: Source, rms: f32) {
        if let Some(c) = self.0.upgrade() {
            c.session.set_level(source, rms);
            c.ui.repaint();
        }
    }
    fn status(&self, source: Source, state: ComponentState, label: String) {
        let Some(c) = self.0.upgrade() else { return };
        if state == ComponentState::Error {
            crate::db::diag("audio_error", json!({ "source": source.as_str(), "label": label }));
        }
        c.session.set_status(if source == Source::User { StatusKey::Mic } else { StatusKey::System }, ComponentStatus::new(state, label));
    }
}

impl Core {
    pub fn new(data_dir: PathBuf) -> Result<Arc<Core>, String> {
        std::fs::create_dir_all(&data_dir).map_err(|e| e.to_string())?;
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(4).build().map_err(|e| e.to_string())?;
        let settings = Arc::new(SettingsStore::open(data_dir.join("settings.json"), data_dir.join("providers.json")));
        crate::i18n::set_lang(settings.get().ui_lang);
        let db = Db::open(&data_dir.join("kairos.db")).map_err(|e| e.to_string())?;
        let interrupted = db.recover_interrupted();
        let secrets = Arc::new(SecretStore::open(data_dir.join("keys.dpapi.json")));
        let net = Net::new();
        let registry = Arc::new(ProviderRegistry::new(settings.clone(), secrets.clone(), net.clone()));
        let st = settings.get();
        let day_start = {
            use chrono::{Local, TimeZone};
            let d = Local::now().date_naive().and_hms_opt(0, 0, 0).unwrap();
            Local.from_local_datetime(&d).single().map(|x| x.timestamp_millis()).unwrap_or(0)
        };
        let budget = Arc::new(Mutex::new(BudgetManager::new(st.budget.clone(), st.pricing.clone(), db.spent_since(day_start), Box::new(now_ms))));
        let assets = Arc::new(AsrAssets::new(data_dir.join("asr")));
        let handle = rt.handle().clone();
        let core = Arc::new_cyclic(|me: &Weak<Core>| {
            let session = SessionManager::new(
                Arc::new(Deps { settings: settings.clone(), db: db.clone(), registry: registry.clone(), budget: budget.clone(), core: me.clone() }),
                handle.clone(),
            );
            let asr = AsrManager::new(assets.clone(), net.clone(), AsrConfig::from(&st.asr), Arc::new(Bridge(me.clone())), handle.clone());
            Core {
                data_dir,
                rt,
                settings,
                db,
                secrets,
                net,
                registry,
                budget,
                session,
                asr,
                assets,
                capture: CaptureController::default(),
                llama: LlamaManager::default(),
                interrupted,
                shortcut_conflicts: Mutex::new(Vec::new()),
                ui: UiBridge::default(),
                found: Mutex::new(None),
                scanning: AtomicBool::new(false),
                local_install: Mutex::new(LocalInstall::default()),
                update: Mutex::new(UpdateState::default()),
                debriefs: Mutex::new(HashMap::new()),
                mock: MockState::default(),
                ocr: Mutex::new(None),
                asr_idle: AtomicU64::new(0),
                me: me.clone(),
            }
        });
        core.ensure_default_profile();
        core.apply_settings(None);
        core.asr.refresh_status();
        if core.interrupted > 0 {
            core.session.notice(Some(t(
                "Минула сесія завершилася аварійно — її транскрипт збережено в «Історії».",
                "The previous session ended abnormally — its transcript is saved in “History”.",
            )
            .into()));
        }
        // A screenshot run must not depend on what happens to be installed here, or reach out to the network.
        if std::env::var_os("COPILOT_SNAPSHOT").is_none() {
            core.unpack_components();
            core.rescan_local();
            core.check_update();
        }
        Ok(core)
    }

    /// One-file edition: unpack the sidecars from the exe on a background thread, with progress in the ASR status.
    fn unpack_components(&self) {
        let core = self.arc();
        std::thread::spawn(move || {
            let Ok(exe) = std::env::current_exe() else { return };
            let asr = core.asr.clone();
            let r = crate::bundle::unpack(&exe, &core.data_dir, &move |d| asr.set_download(Some(d), None));
            match r {
                Ok(_) => core.asr.set_download(None, None),
                Err(e) => {
                    crate::db::diag("unpack_failed", json!({ "error": e }));
                    core.asr.set_download(None, Some(tr!("Не вдалося розпакувати компоненти: {}", "Could not unpack components: {}", e)));
                }
            }
            core.rescan_local();
        });
    }

    // ---- local models ----

    pub fn places(&self) -> Places {
        Places::from_env(&self.data_dir)
    }

    pub fn rescan_local(&self) {
        if self.scanning.swap(true, Ordering::SeqCst) {
            return;
        }
        let core = self.arc();
        self.rt.spawn(async move {
            let found = discover::scan(&core.net, &core.places(), win::total_ram()).await;
            crate::db::diag("local_scan", json!({ "found": found.len() }));
            *core.found.lock().unwrap() = Some(found);
            core.scanning.store(false, Ordering::SeqCst);
            core.emit();
        });
    }

    fn set_answer_roles(&self, provider_id: &str, model: &str, vision: bool) {
        let target = RoleTarget { provider_id: provider_id.into(), model: model.into() };
        self.update_settings(|s| {
            s.roles.insert(ModelRole::QuickAnswer, Some(target.clone()));
            s.roles.insert(ModelRole::DeepAnswer, Some(target.clone()));
            if vision {
                s.roles.insert(ModelRole::Vision, Some(target.clone()));
            }
        });
    }

    /// Connects a found model in one step: an existing provider for the same server or file is reused, not duplicated.
    pub async fn use_found(&self, f: &Found, model: Option<String>) -> Result<(), String> {
        let existing = self.registry.list();
        let cfg = match f {
            Found::Server { runtime, base_url, .. } => existing.iter().find(|p| &p.base_url == base_url).cloned().unwrap_or(ProviderConfig {
                id: String::new(),
                label: runtime.to_string(),
                preset: "custom".into(),
                protocol: ProviderProtocol::Openai,
                base_url: base_url.clone(),
                has_key: false,
                allow_fallback: false,
                timeout_ms: 30_000,
                llama: None,
            }),
            Found::Stopped { runtime, exe, base_url, .. } => existing.iter().find(|p| &p.base_url == base_url).cloned().unwrap_or(ProviderConfig {
                id: String::new(),
                label: runtime.to_string(),
                preset: "custom".into(),
                protocol: ProviderProtocol::Openai,
                base_url: base_url.clone(),
                has_key: false,
                allow_fallback: false,
                timeout_ms: 30_000,
                llama: Some(LlamaLaunch { server_path: exe.display().to_string(), auto_start: true, ..Default::default() }),
            }),
            Found::Gguf(m) => {
                let path = m.path.display().to_string();
                match existing.iter().find(|p| p.llama.as_ref().is_some_and(|l| l.model_path == path)) {
                    Some(p) => p.clone(),
                    None => {
                        let server = self.llama_server().await?;
                        let port = crate::proc::free_port().map_err(|e| e.to_string())?;
                        ProviderConfig {
                            id: String::new(),
                            label: m.name.clone(),
                            preset: "llamacpp".into(),
                            protocol: ProviderProtocol::Openai,
                            base_url: format!("http://127.0.0.1:{port}/v1"),
                            has_key: false,
                            allow_fallback: false,
                            timeout_ms: 30_000,
                            llama: Some(LlamaLaunch {
                                server_path: server.display().to_string(),
                                model_path: path,
                                mmproj_path: m.mmproj.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
                                ctx_size: 8192,
                                gpu_layers: 0,
                                auto_start: true,
                            }),
                        }
                    }
                }
            }
        };
        let (saved, _) = self.upsert_provider(cfg, None)?;
        let (model, vision) = match f {
            Found::Server { models, .. } | Found::Stopped { models, .. } => (model.or_else(|| models.first().cloned()).unwrap_or_default(), true),
            // llama-server serves the one model it was started with, whatever name the request carries.
            Found::Gguf(m) => (m.name.clone(), m.mmproj.is_some()),
        };
        self.set_answer_roles(&saved.id, &model, vision);
        self.emit();
        Ok(())
    }

    /// The user's llama-server if there is one, otherwise the bundled one; the plain exe downloads it the first time.
    async fn llama_server(&self) -> Result<PathBuf, String> {
        if let Some(p) = self.places().llama_server() {
            return Ok(p);
        }
        let (url, sha) = discover::LLAMA_ZIP;
        let dir = self.data_dir.join("llama");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let zip = dir.join("llama.zip.part");
        let got = crate::asr::assets::download(&self.net, url, &zip, "llama.cpp", &|d| self.local_install.lock().unwrap().download = Some(d)).await?;
        if got != sha {
            let _ = std::fs::remove_file(&zip);
            return Err(tr!("Контрольна сума архіву не збіглася ({}…) — файл видалено", "Archive checksum mismatch ({}…) — file deleted", &got[..12]));
        }
        crate::asr::assets::unzip(&zip, &dir)?;
        let _ = std::fs::remove_file(&zip);
        self.places().llama_server().ok_or_else(|| t("В архіві немає llama-server.exe", "llama-server.exe is missing from the archive").to_string())
    }

    /// One click from nothing to a working local model: download, verify, connect.
    pub async fn install_offer(&self, offer: &'static ModelOffer) -> Result<(), String> {
        let r = async {
            let dir = self.data_dir.join("models");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let dest = dir.join(offer.file);
            if !dest.is_file() {
                let part = dest.with_extension("gguf.part");
                let progress = |d: Download| self.local_install.lock().unwrap().download = Some(d);
                let sha = crate::asr::assets::download(&self.net, offer.url, &part, offer.title, &progress).await?;
                if sha != offer.sha256 {
                    let _ = std::fs::remove_file(&part);
                    return Err(tr!("Контрольна сума моделі не збіглася ({}…) — файл видалено", "Model checksum mismatch ({}…) — file deleted", &sha[..12]));
                }
                std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
            }
            let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(offer.bytes);
            self.use_found(&Found::Gguf(discover::GgufModel { path: dest, name: offer.title.into(), size, mmproj: None, origin: "Copilot" }), None).await
        }
        .await;
        {
            let mut st = self.local_install.lock().unwrap();
            st.busy = false;
            st.download = None;
            st.error = r.as_ref().err().cloned();
        }
        self.rescan_local();
        self.emit();
        r
    }

    // ---- updates ----

    pub fn check_update(&self) {
        if !self.s().check_updates {
            return;
        }
        let core = self.arc();
        self.rt.spawn(async move {
            match update::latest(&core.net).await {
                Ok(r) => core.update.lock().unwrap().available = r,
                Err(e) => crate::db::diag("update_check", json!({ "error": e })),
            }
            core.emit();
        });
    }

    pub fn apply_update(&self) {
        let Some(rel) = self.update.lock().unwrap().available.clone() else { return };
        self.update.lock().unwrap().busy = true;
        let core = self.arc();
        self.rt.spawn(async move {
            let c2 = core.clone();
            let progress = move |d: Download| {
                c2.update.lock().unwrap().download = Some(d);
                c2.emit();
            };
            let r = update::install(&core.net, &rel, &progress).await.and_then(|exe| win::relaunch(&exe));
            match r {
                Ok(()) => core.shutdown(),
                Err(e) => {
                    let mut st = core.update.lock().unwrap();
                    st.busy = false;
                    st.download = None;
                    st.error = Some(e);
                }
            }
            core.emit();
        });
    }

    // ---- one-off generations: debrief, mock interview, translation ----

    fn target_for(&self, role: ModelRole) -> Option<ResolvedTarget> {
        let s = self.s();
        [role, ModelRole::DeepAnswer, ModelRole::QuickAnswer].into_iter().find_map(|r| self.registry.resolve(r, &s))
    }

    /// Same gates as a live answer (offline, cloud consent, spend limit), without the live flow's rate limits.
    async fn run_task(&self, role: ModelRole, task: Task, session_id: Option<&str>, mut on_text: impl FnMut(&str) + Send) -> Result<String, String> {
        let s = self.s();
        let target = self.target_for(role).ok_or_else(|| t("Не вибрано модель для відповідей — відкрийте «Моделі та ключі»", "No answer model selected — open “Models & keys”").to_string())?;
        if !target.is_local && s.offline {
            return Err(t("Офлайн-режим: ця модель працює в хмарі", "Offline mode: this model runs in the cloud").into());
        }
        if !target.is_local && !s.consent.cloud {
            return Err(t("Надсилання в хмару не дозволено — увімкніть його в «Приватність» або виберіть локальну модель", "Sending to the cloud is not allowed — enable it in “Privacy” or pick a local model").into());
        }
        if let Gate::Money(reason) = self.budget.lock().unwrap().can_start(target.is_local) {
            return Err(reason);
        }
        self.llama.ensure(&target.cfg, &self.net).await?;
        self.budget.lock().unwrap().begin();
        let req = ChatRequest {
            model: target.model.clone(),
            messages: vec![ChatMessage { role: Role::System, content: task.system, images: vec![] }, ChatMessage { role: Role::User, content: task.user, images: vec![] }],
            max_tokens: task.max_tokens,
            temperature: task.temperature,
        };
        // Nothing is waiting on these the way a live answer is, and a cold local model takes its time.
        let wait = target.cfg.timeout_ms.max(if target.is_local { 120_000 } else { 30_000 });
        let res = run_stream(target.adapter.as_ref(), &req, &CancellationToken::new(), |e| {
            if let RunEvent::Text(d) = e {
                on_text(d);
            }
        }, RunOptions::new(wait)).await;
        match res {
            Ok(r) => {
                let usage = (r.usage.input_tokens, r.usage.output_tokens);
                let cost = self.budget.lock().unwrap().end(&target.cfg.id, &target.model, target.is_local, Some(usage));
                self.db.add_usage(&new_id(), session_id, &target.cfg.id, &target.model, usage.0, usage.1, cost);
                Ok(r.text)
            }
            Err(e) => {
                self.budget.lock().unwrap().end(&target.cfg.id, &target.model, target.is_local, None);
                Err(e.line())
            }
        }
    }

    fn generate(&self, role: ModelRole, task: Task, session_id: Option<String>, slot: GenSlot, done: impl FnOnce(&Core, &str) + Send + 'static) {
        *slot.lock().unwrap() = Gen { busy: true, ..Default::default() };
        let core = self.arc();
        self.rt.spawn(async move {
            let (s2, c2) = (slot.clone(), core.clone());
            let r = core
                .run_task(role, task, session_id.as_deref(), move |d| {
                    s2.lock().unwrap().text.push_str(d);
                    c2.emit();
                })
                .await;
            {
                let mut g = slot.lock().unwrap();
                g.busy = false;
                match &r {
                    Ok(text) => g.text = text.clone(),
                    Err(e) => g.error = Some(e.clone()),
                }
            }
            if let Ok(text) = &r {
                done(&core, text);
            }
            core.emit();
        });
    }

    fn translate_question(&self, q: &QuestionEvent) {
        let s = self.s();
        if !s.translate_questions || detect_lang(&q.text) == Some(s.ui_lang) || self.target_for(ModelRole::QuickAnswer).is_none() {
            return;
        }
        let core = self.arc();
        let (id, revision, text, sid, lang) = (q.question_id.clone(), q.revision, q.text.clone(), q.session_id.clone(), s.ui_lang);
        self.rt.spawn(async move {
            match core.run_task(ModelRole::QuickAnswer, translate_task(&text, lang), Some(&sid), |_| {}).await {
                Ok(tr) => {
                    core.session.with_snap(|sn| {
                        if sn.active_question.as_ref().is_some_and(|a| a.question_id == id && a.revision == revision) {
                            sn.translation = Some(tr.trim().to_string());
                        }
                    });
                    core.emit();
                }
                Err(e) => crate::db::diag("translate_failed", json!({ "error": e })),
            }
        });
    }

    fn transcript_lines(&self, id: &str) -> Vec<String> {
        self.db.session_segments(id).iter().map(|s| format!("{}: {}", if s.source == Source::Remote { "Other side" } else { "Me" }, s.text)).collect()
    }

    /// Writes (or rewrites) the debrief of a saved session and stores it with the session.
    pub fn debrief(&self, id: &str) -> GenSlot {
        let slot = self.debriefs.lock().unwrap().entry(id.to_string()).or_default().clone();
        if slot.lock().unwrap().busy {
            return slot;
        }
        let mode = self.db.session_detail(id).session.as_ref().map(|r| SessionMode::parse(&crate::db::s(r, "mode"))).unwrap_or(SessionMode::Live);
        let task = debrief_task(mode, &self.transcript_lines(id), self.s().answer_lang);
        let sid = id.to_string();
        self.generate(ModelRole::PostSessionSummary, task, Some(id.to_string()), slot.clone(), move |core, text| core.db.set_summary(&sid, text));
        slot
    }

    /// Automatic only when it costs nothing and sends nothing anywhere; for a cloud model the debrief waits for a click in History.
    fn auto_debrief(&self, id: &str) {
        let words: usize = self.db.session_segments(id).iter().map(|s| s.text.split_whitespace().count()).sum();
        if self.s().auto_debrief && words >= 60 && self.target_for(ModelRole::PostSessionSummary).is_some_and(|t| t.is_local) {
            self.debrief(id);
        }
    }

    pub fn mock_next(&self) {
        if self.session.snapshot().mode != Some(SessionMode::Mock) {
            self.start_session(SessionMode::Mock);
        }
        let snap = self.session.snapshot();
        let mut asked: Vec<String> = snap.recent_questions.iter().map(|(_, q)| q.clone()).collect();
        asked.extend(snap.active_question.map(|q| q.text));
        let s = self.s();
        let p = self.profile();
        let lang = if let LangPref::Is(l) = s.answer_lang { l } else { s.ui_lang };
        *self.mock.feedback.lock().unwrap() = Gen::default();
        let task = mock_question_task(&p, &self.db.chunks_for_profile(&p.id), &asked, lang);
        self.generate(ModelRole::QuickAnswer, task, snap.session_id, self.mock.question.clone(), |core, text| core.session.pose_question(text.trim()));
    }

    /// Reviews what the user said since the question (plus anything typed), and stores the review with the session.
    pub fn mock_feedback(&self, typed: &str) {
        let snap = self.session.snapshot();
        let Some(q) = snap.active_question else { return };
        let answer = format!("{} {}", self.session.speech_since_question(), typed).trim().to_string();
        if answer.is_empty() {
            self.session.notice(Some(t("Спершу дайте відповідь уголос або введіть її текстом.", "Answer aloud or type your answer first.").into()));
            return;
        }
        let s = self.s();
        let p = self.profile();
        let lang = if let LangPref::Is(l) = s.answer_lang { l } else { s.ui_lang };
        let task = feedback_task(&p, &self.db.chunks_for_profile(&p.id), &q.text, &answer, lang);
        let sid = snap.session_id.clone();
        self.generate(ModelRole::DeepAnswer, task, snap.session_id, self.mock.feedback.clone(), move |core, text| {
            let Some(session_id) = sid.filter(|_| core.session.persisting()) else { return };
            core.db.upsert_answer(&AnswerEvent {
                session_id,
                question_id: q.question_id.clone(),
                revision: q.revision,
                answer_id: new_id(),
                kind: AnswerKind::Feedback,
                provider: String::new(),
                model: String::new(),
                prompt_preset: String::new(),
                text: text.to_string(),
                status: AnswerStatus::Done,
                first_token_ms: None,
                latency_ms: None,
                usage: None,
                usage_cost_usd: None,
                error: None,
            });
        });
    }

    /// A bug report that needs no back-and-forth: versions, settings and the redacted log. No keys, no conversation text.
    pub fn export_diagnostics(&self, path: &Path) -> Result<(), String> {
        let providers: Vec<Value> = self.registry.list().iter().map(|p| json!({ "preset": p.preset, "local": crate::net::is_loopback_url(&p.base_url), "hasKey": p.has_key })).collect();
        let log: Vec<Value> = self.db.recent_diag(500).into_iter().map(|(at, kind, data)| json!({ "at": at, "kind": kind, "data": data })).collect();
        let st = self.asr.status();
        let v = json!({
            "app": env!("CARGO_PKG_VERSION"),
            "windowsBuild": win::windows_build(),
            "ramGb": win::total_ram() / 1_073_741_824,
            "settings": { "uiLang": self.s().ui_lang, "asr": { "backend": self.s().asr.backend, "model": self.s().asr.model, "language": self.s().asr.language }, "offline": self.s().offline, "onboarded": self.s().onboarded },
            "asr": { "binary": st.binary, "modelPresent": st.model_present, "error": st.error },
            "providers": providers,
            "log": log,
        });
        std::fs::write(path, crate::redact::redact(&serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?)).map_err(|e| e.to_string())
    }

    fn arc(&self) -> Arc<Core> {
        self.me.upgrade().expect("core alive")
    }

    fn sink(&self) -> Arc<dyn CaptureSink> {
        Arc::new(Bridge(self.me.clone()))
    }

    pub fn s(&self) -> Settings {
        self.settings.get()
    }

    pub fn profile(&self) -> Profile {
        active_profile(&self.db, &self.s())
    }

    fn ensure_default_profile(&self) {
        if self.db.list_profiles().is_empty() {
            self.db.upsert_profile(&Profile {
                id: "default".into(),
                name: t("Основний", "Main").into(),
                preset: ProfilePreset::Backend,
                style: AnswerStyle::Natural,
                notes: String::new(),
            });
        }
    }

    pub fn emit(&self) {
        self.ui.repaint();
    }

    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) {
        let (prev, _) = self.settings.update(f);
        self.apply_settings(Some(&prev));
    }

    pub fn apply_settings(&self, prev: Option<&Settings>) {
        let st = self.s();
        crate::i18n::set_lang(st.ui_lang);
        self.net.set_offline(st.offline);
        self.budget.lock().unwrap().update(st.budget.clone(), st.pricing.clone());
        self.asr.set_config(AsrConfig::from(&st.asr));
        if prev.is_none_or(|p| p.shortcuts != st.shortcuts) {
            self.ui.shortcuts_dirty.store(true, Ordering::SeqCst);
        }
        if let Some(p) = prev {
            if p.overlay.pinned != st.overlay.pinned {
                win::OVERLAY.set_topmost(st.overlay.pinned);
            }
            if p.overlay.protect != st.overlay.protect {
                self.apply_protection();
            }
            if p.window.protect_from_capture != st.window.protect_from_capture {
                self.apply_window_protection();
            }
            if p.ui_lang != st.ui_lang {
                if !self.session.active() {
                    self.session.with_snap(|s| {
                        s.status.mic = crate::session::manager::off();
                        s.status.system = crate::session::manager::off();
                    });
                }
                self.asr.refresh_status();
                self.apply_protection();
                self.set_overlay_status();
            }
        }
        self.update_llm_status();
        self.emit();
    }

    pub fn ocr_installed(&self) -> bool {
        *self.ocr.lock().unwrap().get_or_insert_with(crate::screen::ocr_installed)
    }

    pub fn update_llm_status(&self) {
        let st = self.s();
        let quick = self.registry.resolve(ModelRole::QuickAnswer, &st);
        let vision = self.registry.resolve(ModelRole::Vision, &st);
        let llm = match &quick {
            Some(q) => {
                let ok = self.registry.text_ok.lock().unwrap().get(&format!("{}/{}", q.cfg.id, q.model)).copied();
                let state = if st.offline && !q.is_local {
                    ComponentState::Error
                } else {
                    match ok {
                        Some(true) => ComponentState::Ok,
                        Some(false) => ComponentState::Error,
                        None => ComponentState::Warn,
                    }
                };
                let tail = match ok {
                    None => t(" · не перевірено", " · not checked"),
                    Some(true) => "",
                    Some(false) => t(" · останній запит з помилкою", " · last request failed"),
                };
                ComponentStatus::new(state, format!("{} · {} · {}{}", q.cfg.label, q.model, if q.is_local { t("локально", "local") } else { t("хмара", "cloud") }, tail))
            }
            None => ComponentStatus::new(ComponentState::Warn, t("модель не вибрано", "no model selected")),
        };
        let vis = match &vision {
            Some(v) => {
                let ok = self.registry.vision_ok.lock().unwrap().get(&format!("{}/{}", v.cfg.id, v.model)).copied();
                let (state, tail) = match ok {
                    Some(false) => (ComponentState::Error, t(" · перевірку не пройдено", " · check failed")),
                    Some(true) => (ComponentState::Ok, ""),
                    None => (ComponentState::Warn, t(" · не перевірено", " · not checked")),
                };
                ComponentStatus::new(state, format!("{}{tail}", v.model))
            }
            None if self.ocr_installed() => ComponentStatus::new(ComponentState::Warn, t("лише локальний OCR", "local OCR only")),
            None => ComponentStatus::new(ComponentState::Off, t("вимк.", "off")),
        };
        let network = if st.offline {
            ComponentStatus::new(ComponentState::Warn, t("офлайн-режим: зовнішні виклики заблоковано", "offline mode: external calls blocked"))
        } else {
            ComponentStatus::new(ComponentState::Ok, t("онлайн", "online"))
        };
        self.session.with_snap(|s| {
            s.status.llm = llm;
            s.status.vision = vis;
            s.status.network = network;
        });
    }

    /// Windows gives no feedback on whether a capture tool honours the flag, so it is never shown as a green guarantee.
    pub fn apply_protection(&self) {
        let want = self.s().overlay.protect;
        let supported = win::display_protection_supported();
        let live = win::OVERLAY.set_capture_excluded(want && supported);
        let s = if !want {
            ComponentStatus::new(ComponentState::Off, t("вимк.", "off"))
        } else if !supported {
            ComponentStatus::new(ComponentState::Error, t("не підтримується цією версією Windows", "not supported by this Windows version"))
        } else if live {
            ComponentStatus::new(
                ComponentState::Warn,
                t("прапорець встановлено — працює не з усіма програмами захоплення, перевірте в одержувача", "flag set — not every capture tool honours it, check on the receiving side"),
            )
        } else {
            ComponentStatus::new(ComponentState::Warn, t("увімкнеться під час показу підказок", "turns on when the hints window is shown"))
        };
        self.session.set_status(StatusKey::Protection, s);
    }

    /// The main window and the region picker share one flag; the overlay keeps its own (overlay.protect).
    pub fn apply_window_protection(&self) {
        let want = self.s().window.protect_from_capture && win::display_protection_supported();
        win::MAIN.set_capture_excluded(want);
        win::PICKER.set_capture_excluded(want);
    }

    /// Called by the UI once the overlay window exists: style, z-order, protection and saved position.
    pub fn overlay_ready(&self) {
        win::OVERLAY.make_tool_noactivate();
        let o = self.s().overlay;
        win::OVERLAY.set_topmost(o.pinned);
        win::OVERLAY.set_rect(self.overlay_bounds(), o.pinned, false);
        self.apply_protection();
    }

    pub fn picker_ready(&self) {
        win::PICKER.make_tool_noactivate();
        self.apply_window_protection();
    }

    fn overlay_bounds(&self) -> Rect {
        let o = self.s().overlay;
        o.bounds
            .map(|b| Rect { x: b.x, y: b.y, w: b.width.max(300), h: b.height.max(120) })
            .filter(|r| win::bounds_visible(*r))
            .unwrap_or_else(|| {
                let wa = win::primary_work_area();
                Rect { x: wa.x + wa.w - 460 - 24, y: wa.y + 24, w: 460, h: 220 }
            })
    }

    pub fn save_overlay_bounds(&self, r: Rect) {
        self.settings.update(|s| s.overlay.bounds = Some(Bounds { x: r.x, y: r.y, width: r.w, height: r.h }));
    }

    fn set_overlay_status(&self) {
        let s = if win::OVERLAY.is_visible() {
            ComponentStatus::new(ComponentState::Ok, if self.s().overlay.expanded { t("розгорнутий", "expanded") } else { t("компактний", "compact") })
        } else {
            ComponentStatus::new(ComponentState::Off, t("приховано", "hidden"))
        };
        self.session.set_status(StatusKey::Overlay, s);
    }

    pub fn show_overlay(&self) {
        // Shown inactive so the foreground window (IDE, Zoom) keeps keyboard focus.
        if let Some(r) = win::OVERLAY.rect() {
            if !win::bounds_visible(r) {
                win::OVERLAY.set_rect(self.overlay_bounds(), self.s().overlay.pinned, false);
            }
        }
        win::OVERLAY.show_no_activate();
        let s = ComponentStatus::new(ComponentState::Ok, if self.s().overlay.expanded { t("розгорнутий", "expanded") } else { t("компактний", "compact") });
        self.session.set_status(StatusKey::Overlay, s);
    }

    pub fn hide_overlay(&self) {
        win::OVERLAY.hide();
        self.session.set_status(StatusKey::Overlay, ComponentStatus::new(ComponentState::Off, t("приховано", "hidden")));
    }

    pub fn toggle_overlay(&self) {
        if win::OVERLAY.is_visible() {
            self.hide_overlay();
        } else {
            self.show_overlay();
        }
    }

    pub fn set_expanded(&self, v: bool, trigger: QuestionTrigger) {
        self.settings.update(|s| s.overlay.expanded = v);
        let snap = self.session.snapshot();
        if v && !snap.answers.contains_key(&AnswerKind::Deep) && snap.active_question.is_some() {
            self.session.answer_now(AnswerKind::Deep, trigger);
        }
        self.set_overlay_status();
        self.emit();
    }

    fn after_start(&self, id: String, mode: SessionMode) -> String {
        self.asr_idle.fetch_add(1, Ordering::SeqCst);
        self.asr.reset_session();
        let asr = self.asr.clone();
        self.rt.spawn(async move { asr.ensure().await });
        self.warm_local_model();
        let s = self.s();
        self.capture.start(
            CaptureOptions {
                session_id: id.clone(),
                mic_device_id: s.audio.mic_device_id.clone(),
                capture_mic: s.audio.capture_mic && s.consent.mic,
                // In a mock interview the only other voice is a video playing somewhere; it must not count as the interviewer.
                capture_system: s.audio.capture_system && s.consent.system && mode != SessionMode::Mock,
            },
            self.sink(),
        );
        if !matches!(mode, SessionMode::Meeting | SessionMode::Mock) {
            self.show_overlay();
        }
        id
    }

    pub fn start_session(&self, mode: SessionMode) -> String {
        let id = self.session.start(mode);
        self.after_start(id, mode)
    }

    pub fn resume_session(&self) -> Option<String> {
        let r = self.db.last_resumable()?;
        let id = self.session.continue_previous(self.db.session_segments(&r.id), r.mode);
        Some(self.after_start(id, r.mode))
    }

    pub fn ask_typed(&self, text: &str) {
        if !self.session.active() {
            self.start_session(SessionMode::Practice);
        }
        self.session.ask_typed(text);
    }

    /// A cold local model costs 20–60 s on its first request, so I load it while the user is still settling in.
    fn warm_local_model(&self) {
        let Some(t) = self.registry.resolve(ModelRole::QuickAnswer, &self.s()) else { return };
        if !t.is_local || t.cfg.llama.as_ref().is_some_and(|l| !l.auto_start) {
            return;
        }
        let core = self.arc();
        self.rt.spawn(async move {
            if let Err(e) = core.llama.ensure(&t.cfg, &core.net).await {
                core.session.notice(Some(format!("llama.cpp: {e}")));
                return;
            }
            let req = ChatRequest { model: t.model.clone(), messages: vec![ChatMessage { role: Role::User, content: "OK".into(), images: vec![] }], max_tokens: 1, temperature: 0.0 };
            let _ = run_stream(t.adapter.as_ref(), &req, &CancellationToken::new(), |_| {}, RunOptions::new(120_000).retries(0)).await;
        });
    }

    // whisper-server holds ~300 MB; I keep it briefly for a quick restart, then give the memory back.
    fn schedule_asr_idle_stop(&self) {
        let gen = self.asr_idle.fetch_add(1, Ordering::SeqCst) + 1;
        let core = self.arc();
        self.rt.spawn(async move {
            tokio::time::sleep(Duration::from_secs(180)).await;
            if core.asr_idle.load(Ordering::SeqCst) == gen && !core.session.active() {
                core.asr.stop();
            }
        });
    }

    pub fn stop_session(&self) {
        let id = self.session.session_id().filter(|_| self.session.persisting());
        self.session.stop();
        self.capture.stop();
        self.schedule_asr_idle_stop();
        if let Some(id) = id {
            self.auto_debrief(&id);
        }
    }

    pub fn toggle_listen(&self) {
        if !self.session.active() {
            self.start_session(SessionMode::Live);
        } else if self.session.listening() {
            self.session.pause();
            self.capture.pause(self.sink().as_ref());
        } else {
            self.session.resume();
            self.capture.resume(self.sink());
        }
    }

    pub fn panic(&self) {
        self.session.panic();
        self.capture.stop();
        self.asr.stop();
        self.hide_overlay();
        self.cancel_region();
        // Opt-in: emergency stop leaves nothing running — the whole app quits after capture is down.
        if self.s().window.panic_quits {
            self.shutdown();
        }
    }

    pub fn on_shortcut(&self, a: ShortcutAction) {
        match a {
            ShortcutAction::ToggleOverlay => self.toggle_overlay(),
            ShortcutAction::ToggleListen => self.toggle_listen(),
            ShortcutAction::AnswerNow => self.session.answer_now(AnswerKind::Say, QuestionTrigger::Hotkey),
            ShortcutAction::SnapRegion => self.begin_region(),
            ShortcutAction::ScrollUp | ShortcutAction::ScrollDown => {
                *self.ui.overlay_scroll.lock().unwrap() += if a == ShortcutAction::ScrollUp { -140.0 } else { 140.0 };
                self.emit();
            }
            ShortcutAction::Expand => self.set_expanded(!self.s().overlay.expanded, QuestionTrigger::Hotkey),
            ShortcutAction::Panic => self.panic(),
        }
    }

    pub fn begin_region(&self) {
        if win::PICKER.is_visible() {
            return;
        }
        let s = self.s();
        if !s.consent.screen {
            self.session.notice(Some(t("Знімки екрана вимкнено — дозвольте їх у «Налаштування → Приватність»", "Screenshots are off — allow them in “Settings → Privacy”").into()));
            return;
        }
        let Some((monitor, _)) = win::monitor_under_cursor() else { return };
        let overlay_was_visible = win::OVERLAY.is_visible();
        let protected_overlay = s.overlay.protect && win::display_protection_supported();
        if overlay_was_visible && !protected_overlay {
            win::OVERLAY.hide();
            std::thread::sleep(Duration::from_millis(150));
        }
        match crate::screen::capture(monitor) {
            Ok(shot) => {
                {
                    let mut p = self.ui.picker.lock().unwrap();
                    p.shot = Some(Arc::new(shot));
                    p.generation += 1;
                    p.has_vision = self.registry.resolve(ModelRole::Vision, &s).is_some();
                    p.ocr = self.ocr_installed();
                    p.overlay_was_visible = overlay_was_visible;
                    p.protected_overlay = protected_overlay;
                }
                self.apply_window_protection();
                win::PICKER.set_rect(monitor, true, true);
                win::PICKER.foreground();
                self.emit();
                if let Some(ctx) = self.ui.ctx.get() {
                    ctx.request_repaint_of(crate::ui::picker_id());
                }
            }
            Err(e) => {
                if overlay_was_visible && !protected_overlay {
                    win::OVERLAY.show_no_activate();
                }
                self.session.notice(Some(e));
            }
        }
    }

    fn close_picker(&self) -> Option<Arc<Shot>> {
        win::PICKER.hide();
        let mut p = self.ui.picker.lock().unwrap();
        if p.overlay_was_visible && !p.protected_overlay {
            win::OVERLAY.show_no_activate();
        }
        p.overlay_was_visible = false;
        p.shot.take()
    }

    pub fn cancel_region(&self) {
        if win::PICKER.is_visible() || self.ui.picker.lock().unwrap().shot.is_some() {
            self.close_picker();
        }
    }

    /// `sel` and `blurs` are in screenshot pixels.
    pub fn submit_region(&self, sel: Rect, blurs: Vec<Rect>, task: ScreenTask, with_code: bool, code_lang: String) {
        let Some(shot) = self.close_picker() else { return };
        if !self.s().consent.screen {
            self.session.notice(Some(t("Знімки екрана вимкнено — дозвольте їх у «Налаштування → Приватність»", "Screenshots are off — allow them in “Settings → Privacy”").into()));
            return;
        }
        let core = self.arc();
        std::thread::spawn(move || {
            let (png, rgba) = match crate::screen::crop_png(&shot, sel, &blurs) {
                Ok(v) => v,
                Err(e) => return core.session.notice(Some(e)),
            };
            let s = core.s();
            let target = core.registry.resolve(ModelRole::Vision, &s);
            let known_bad = target.as_ref().is_none_or(|t| core.registry.vision_ok.lock().unwrap().get(&format!("{}/{}", t.cfg.id, t.model)) == Some(&false));
            if !known_bad {
                let image = ChatImage { mime: "image/png".into(), base64: base64::engine::general_purpose::STANDARD.encode(png) };
                core.session.screen(ScreenInput { task, with_code, code_lang, ocr_text: None, image: Some(image) });
                return;
            }
            core.session.notice(Some(t("Модель без vision — використано локальний OCR (якість залежить від чіткості тексту)", "No vision model — local OCR used (quality depends on text clarity)").into()));
            match crate::screen::recognize(rgba.as_raw(), rgba.width(), rgba.height()) {
                Ok(text) => {
                    let text = if text.trim().is_empty() { t("(OCR не знайшов тексту)", "(OCR found no text)").to_string() } else { text };
                    core.session.screen(ScreenInput { task, with_code, code_lang, ocr_text: Some(text), image: None });
                }
                Err(e) => core.session.notice(Some(tr!("Знімок не надіслано: {}", "Screenshot not sent: {}", e))),
            }
        });
    }

    pub fn outgoing_preview(&self) -> OutgoingPreview {
        let st = self.s();
        let mut recipients: Vec<Recipient> = Vec::new();
        for r in [ModelRole::QuickAnswer, ModelRole::DeepAnswer, ModelRole::Vision] {
            if let Some(t) = self.registry.resolve(r, &st) {
                if !recipients.iter().any(|x| x.base_url == t.cfg.base_url && x.label == t.cfg.label) {
                    recipients.push(Recipient { label: t.cfg.label.clone(), base_url: t.cfg.base_url.clone(), local: t.is_local });
                }
            }
        }
        OutgoingPreview {
            recipients,
            items: vec![
                t("Аудіо не покидає комп'ютер — розпізнається локально (whisper.cpp)", "Audio never leaves the computer — recognized locally (whisper.cpp)").into(),
                t(
                    "Провайдеру відповідей іде: текст поточного питання, останні хвилини транскрипту, доречні фрагменти CV/JD і нотатки профілю",
                    "The answer provider receives: the current question, the last minutes of transcript, relevant CV/JD fragments and profile notes",
                )
                .into(),
                t("Підсумок сесії отримує весь транскрипт: автоматично лише локальна модель, хмарна — за вашою кнопкою в «Історії»", "A session debrief gets the whole transcript: automatically only a local model, a cloud one on your click in “History”").into(),
                t("Знімки екрана — лише після вашого виділення й підтвердження", "Screenshots — only after you select and confirm").into(),
                if st.history && st.consent.history {
                    t("Історія зберігається лише на цьому комп'ютері (SQLite)", "History is stored on this computer only (SQLite)")
                } else {
                    t("Історія не зберігається", "History is not saved")
                }
                .into(),
            ],
        }
    }

    // ---- providers ----

    pub fn upsert_provider(&self, mut cfg: ProviderConfig, key: Option<&str>) -> Result<(ProviderConfig, Option<String>), String> {
        let check = check_endpoint(&cfg.base_url);
        if !check.ok {
            return Err(check.error.unwrap_or_default());
        }
        if cfg.id.is_empty() {
            cfg.id = new_id()[..8].to_string();
        }
        cfg.protocol = preset(&cfg.preset).protocol;
        let saved = self.registry.upsert(cfg, key)?;
        self.llama.stop(&saved.id);
        self.update_llm_status();
        Ok((saved, check.warning))
    }

    pub fn remove_provider(&self, id: &str) {
        self.llama.stop(id);
        self.registry.remove(id);
        self.update_llm_status();
        self.emit();
    }

    pub async fn test_provider(&self, id: &str, model: Option<String>) -> ProviderTestReport {
        if let Some(cfg) = self.registry.get(id) {
            if let Err(e) = self.llama.ensure(&cfg, &self.net).await {
                return ProviderTestReport { models: Err(crate::providers::errors::app_error(ErrorCode::Network, Some(&e), None)), text: None, vision: None };
            }
        }
        let r = self.registry.test(id, model.as_deref()).await;
        self.update_llm_status();
        r
    }

    pub fn llama_state(&self, id: &str) -> LlamaState {
        self.llama.state(id)
    }

    pub async fn llama_start(&self, id: &str) -> Result<(), String> {
        let cfg = self.registry.get(id).ok_or_else(|| t("Провайдера не знайдено", "Provider not found").to_string())?;
        self.llama.ensure(&cfg, &self.net).await
    }

    // ---- profiles & knowledge ----

    pub fn upsert_profile(&self, mut p: Profile) -> Profile {
        if p.id.is_empty() {
            p.id = new_id()[..8].to_string();
        }
        self.db.upsert_profile(&p);
        self.session.reload_knowledge();
        p
    }

    pub fn remove_profile(&self, id: &str) -> Result<(), String> {
        if self.db.list_profiles().len() <= 1 {
            return Err(t("Не можна видалити останній профіль", "You cannot delete the last profile").into());
        }
        self.db.delete_profile(id);
        if self.s().active_profile_id == id {
            let first = self.db.list_profiles()[0].id.clone();
            self.update_settings(|s| s.active_profile_id = first);
        }
        self.session.reload_knowledge();
        Ok(())
    }

    pub fn import_doc(&self, profile_id: &str, kind: DocKind, path: &Path) -> Result<(), String> {
        let text = crate::context::importers::extract_text(path)?;
        if text.trim().is_empty() {
            return Err(t("У документі не знайдено тексту (можливо, це скан — потрібен OCR)", "No text found in the document (it may be a scan — OCR needed)").into());
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.add_text(profile_id, kind, &name, &text);
        Ok(())
    }

    pub fn add_text(&self, profile_id: &str, kind: DocKind, name: &str, text: &str) {
        let pieces = crate::context::retrieval::chunk_text(text, 700);
        self.db.add_document(&new_id(), profile_id, name, kind, &pieces);
        self.session.reload_knowledge();
    }

    pub fn remove_doc(&self, id: &str) {
        self.db.delete_doc(id);
        self.session.reload_knowledge();
    }

    // ---- history ----

    pub fn history_detail(&self, id: &str) -> HistoryDetail {
        self.db.session_detail(id)
    }

    pub fn export_session(&self, id: &str, json_fmt: bool, path: &Path) -> Result<(), String> {
        let content = crate::export::export_session(&self.db.session_detail(id), json_fmt)?;
        std::fs::write(path, content).map_err(|e| e.to_string())
    }

    // ---- ASR ----

    pub async fn asr_install(&self, backend: AsrBackend, model: String) -> Result<bool, String> {
        let asr = self.asr.clone();
        let progress = move |d: Download| asr.set_download(Some(d), None);
        let r: Result<(), String> = async {
            if self.assets.server_path(backend).is_none() {
                self.assets.download_binary(&self.net, backend, &progress).await?;
            }
            if !self.assets.has_model(&model) {
                self.assets.download_model(&self.net, &model, &progress).await?;
            }
            Ok(())
        }
        .await;
        match r {
            Ok(()) => {
                self.asr.set_download(None, None);
                self.update_settings(|s| {
                    s.asr.backend = backend;
                    s.asr.model = model.clone();
                });
                Ok(self.asr.ensure().await)
            }
            Err(e) => {
                self.asr.set_download(None, Some(e.clone()));
                Err(e)
            }
        }
    }

    // ---- diagnostics ----

    pub async fn diagnostics(&self) -> Vec<DiagCheck> {
        let mut checks = Vec::new();
        let mut add = |id: &str, label: &str, ok: Option<bool>, detail: String| checks.push(DiagCheck { id: id.into(), label: label.into(), ok, detail });
        let secrets_ok = self.secrets.available();
        add(
            "secrets",
            t("Сховище ключів (DPAPI)", "Key storage (DPAPI)"),
            Some(secrets_ok),
            if secrets_ok { t("Ключі шифруються Windows під вашим обліковим записом", "Keys are encrypted by Windows under your account") } else { t("Шифрування недоступне — ключі не зберігаються", "Encryption unavailable — keys are not saved") }.into(),
        );
        let st = self.asr.status();
        if !st.binary || !st.model_present {
            add("asr", t("Розпізнавання мовлення", "Speech recognition"), Some(false), t("Не встановлено — відкрийте «Мовлення і звук» → «Встановити»", "Not installed — open “Speech & audio” → “Install”").into());
        } else {
            let ok = self.asr.ensure().await;
            let st = self.asr.status();
            let detail = if ok {
                let lat = st.last_latency_ms.map(|m| tr!(", остання затримка {} мс", ", last latency {} ms", m)).unwrap_or_default();
                tr!("whisper {} запущено локально{}", "whisper {} running locally{}", st.model, lat)
            } else {
                st.error.unwrap_or_else(|| t("не запускається", "does not start").into())
            };
            add("asr", t("Розпізнавання мовлення", "Speech recognition"), Some(ok), detail);
        }
        let s = self.s();
        for (role, label) in [
            (ModelRole::QuickAnswer, t("Швидка відповідь", "Quick answer")),
            (ModelRole::DeepAnswer, t("Детальна відповідь", "Deep answer")),
            (ModelRole::Vision, t("Зображення", "Vision")),
        ] {
            let id = format!("role:{role:?}");
            let Some(tg) = self.registry.resolve(role, &s) else {
                add(&id, label, if role == ModelRole::QuickAnswer { Some(false) } else { None }, t("Модель не вибрано", "No model selected").into());
                continue;
            };
            if let Err(e) = self.llama.ensure(&tg.cfg, &self.net).await {
                add(&id, label, Some(false), format!("{}: {e}", tg.cfg.label));
                continue;
            }
            let t0 = std::time::Instant::now();
            match self.registry.models(&tg.cfg.id, true).await {
                Ok(_) => add(
                    &id,
                    label,
                    Some(true),
                    tr!("{} · {} — з'єднання {} мс{}", "{} · {} — connection {} ms{}", tg.cfg.label, tg.model, t0.elapsed().as_millis(), if tg.is_local { t(" · локально", " · local") } else { "" }),
                ),
                Err(e) => add(&id, label, Some(false), format!("{}: {}", tg.cfg.label, e.line())),
            }
        }
        let conflicts = self.shortcut_conflicts.lock().unwrap().clone();
        add(
            "shortcuts",
            t("Гарячі клавіші", "Hotkeys"),
            Some(conflicts.is_empty()),
            if conflicts.is_empty() { t("Усі поєднання зареєстровано, конфліктів немає", "All shortcuts registered, no conflicts").into() } else { tr!("Конфлікти: {}", "Conflicts: {}", conflicts.join("; ")) },
        );
        let supported = win::display_protection_supported();
        add(
            "protection",
            t("Захист вікна підказок від захоплення", "Hints window capture protection"),
            if supported { None } else { Some(false) },
            if supported {
                t(
                    "Системний прапорець WDA_EXCLUDEFROMCAPTURE доступний. Результат залежить від способу захоплення — перевірте в одержувача: почніть демонстрацію екрана й попросіть співрозмовника сказати, чи видно вікно підказок.",
                    "The system flag WDA_EXCLUDEFROMCAPTURE is available. The result depends on the capture method — check on the receiving side: start sharing your screen and ask whether the hints window is visible.",
                )
            } else {
                t("Потрібна Windows 10 2004 або новіша", "Requires Windows 10 2004 or newer")
            }
            .into(),
        );
        if s.offline {
            let blocked = self.net.get("https://example.com/").is_err();
            add(
                "offline",
                t("Офлайн-режим", "Offline mode"),
                Some(blocked),
                if blocked { t("Зовнішні адреси заблоковано на рівні застосунку", "External addresses are blocked by the app") } else { t("Зовнішній запит НЕ заблоковано", "External request was NOT blocked") }.into(),
            );
        } else {
            add("offline", t("Офлайн-режим", "Offline mode"), None, t("Вимкнено: дозволено запити до вибраних провайдерів", "Off: requests to the selected providers are allowed").into());
        }
        let langs = crate::screen::ocr_languages();
        *self.ocr.lock().unwrap() = Some(!langs.is_empty());
        add(
            "ocr",
            t("Локальний OCR (Windows)", "Local OCR (Windows)"),
            if langs.is_empty() { None } else { Some(true) },
            if langs.is_empty() {
                t("Немає мовних пакетів з розпізнаванням тексту — потрібен лише для моделей без vision", "No language packs with text recognition — needed only for models without vision").into()
            } else {
                tr!("Доступно: {}", "Available: {}", langs.join(", "))
            },
        );
        let qc = self.db.quick_check();
        add("db", t("Локальна база", "Local database"), Some(qc == "ok"), if qc == "ok" { t("Цілісність у порядку", "Integrity OK").into() } else { tr!("Проблема: {}", "Problem: {}", qc) });
        checks
    }

    pub fn shutdown(&self) -> ! {
        self.session.stop();
        self.capture.stop();
        self.asr.stop();
        self.llama.stop_all();
        std::process::exit(0);
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    use super::*;

    /// Answers every POST with the same OpenAI-style stream, and records the request bodies.
    fn fake_llm(reply: &'static str) -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let s2 = seen.clone();
        std::thread::spawn(move || {
            for sock in listener.incoming().flatten() {
                let mut sock = sock;
                let mut req = Vec::new();
                let mut buf = [0u8; 65536];
                loop {
                    let n = sock.read(&mut buf).unwrap_or(0);
                    req.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&req);
                    let done = text.find("\r\n\r\n").is_some_and(|h| {
                        let len = text[..h].lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                        req.len() >= h + 4 + len
                    });
                    if n == 0 || done {
                        break;
                    }
                }
                s2.lock().unwrap().push(String::from_utf8_lossy(&req).into_owned());
                let body = format!("data: {}\n\ndata: [DONE]\n\n", json!({ "choices": [{ "delta": { "content": reply } }] }));
                let _ = sock.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
            }
        });
        (port, seen)
    }

    fn core() -> Arc<Core> {
        // The same switch the screenshot run uses: no unpacking, no scan of this machine, no network.
        unsafe { std::env::set_var("COPILOT_SNAPSHOT", "1") };
        let dir = std::env::temp_dir().join(format!("copilot-core-{}", new_id()));
        Core::new(dir).unwrap()
    }

    fn wait(slot: &GenSlot) -> Gen {
        let t0 = Instant::now();
        loop {
            let g = slot.lock().unwrap().clone();
            if !g.busy {
                return g;
            }
            assert!(t0.elapsed() < Duration::from_secs(20), "generation hung");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn seg(session_id: &str, source: Source, start_ms: i64, end_ms: i64, text: &str) -> TranscriptSegment {
        TranscriptSegment { session_id: session_id.into(), segment_id: new_id(), source, start_ms, end_ms, text: text.into(), is_final: true, lang: None }
    }

    #[test]
    fn found_models_connect_in_one_step_without_duplicates() {
        let c = core();
        let server = Found::Server { runtime: "Ollama", base_url: "http://127.0.0.1:11434/v1".into(), models: vec!["qwen3:8b".into(), "gemma3:4b".into()] };
        c.rt.block_on(c.use_found(&server, Some("gemma3:4b".into()))).unwrap();
        let s = c.s();
        let quick = s.role(ModelRole::QuickAnswer).unwrap();
        assert_eq!(quick.model, "gemma3:4b");
        assert_eq!(s.role(ModelRole::DeepAnswer).unwrap().provider_id, quick.provider_id);
        assert_eq!(c.registry.get(&quick.provider_id).unwrap().label, "Ollama");
        assert!(c.registry.resolve(ModelRole::QuickAnswer, &s).unwrap().is_local);
        // The same server again reuses the provider and only switches the model.
        c.rt.block_on(c.use_found(&server, Some("qwen3:8b".into()))).unwrap();
        assert_eq!(c.registry.list().len(), 1);
        assert_eq!(c.s().role(ModelRole::QuickAnswer).unwrap().model, "qwen3:8b");

        // A GGUF file becomes a llama-server the app runs itself, on a port of its own.
        std::fs::create_dir_all(c.data_dir.join("llama")).unwrap();
        std::fs::write(c.data_dir.join("llama/llama-server.exe"), b"").unwrap();
        let gguf = Found::Gguf(discover::GgufModel { path: c.data_dir.join("m.gguf"), name: "m".into(), size: 1, mmproj: None, origin: "x" });
        c.rt.block_on(c.use_found(&gguf, None)).unwrap();
        let cfg = c.registry.get(&c.s().role(ModelRole::QuickAnswer).unwrap().provider_id).unwrap();
        let launch = cfg.llama.expect("llama launch");
        assert_eq!(launch.gpu_layers, 0, "VRAM fitting is left to llama-server");
        assert!(launch.server_path.ends_with("llama-server.exe"));
        c.rt.block_on(c.use_found(&gguf, None)).unwrap();
        assert_eq!(c.registry.list().len(), 2, "the same file is not connected twice");
    }

    #[test]
    fn mock_interview_flow_and_debrief() {
        let (port, seen) = fake_llm("Tell me about a hard bug you fixed.");
        let c = core();
        c.update_settings(|s| s.consent.history = true);
        let server = Found::Server { runtime: "Test", base_url: format!("http://127.0.0.1:{port}/v1"), models: vec!["m".into()] };
        c.rt.block_on(c.use_found(&server, None)).unwrap();

        // The app asks.
        c.mock_next();
        let snap = c.session.snapshot();
        assert_eq!(snap.mode, Some(SessionMode::Mock));
        assert!(wait(&c.mock.question).error.is_none());
        let t0 = Instant::now();
        while c.session.snapshot().active_question.is_none() {
            assert!(t0.elapsed() < Duration::from_secs(5), "the question never became active");
            std::thread::sleep(Duration::from_millis(10));
        }
        let snap = c.session.snapshot();
        assert_eq!(snap.active_question.as_ref().unwrap().text, "Tell me about a hard bug you fixed.");
        assert!(snap.answers.is_empty(), "a mock question gets no generated answer");

        // No answer yet: a notice instead of a request.
        let before = seen.lock().unwrap().len();
        c.mock_feedback("");
        assert_eq!(seen.lock().unwrap().len(), before);
        assert!(c.session.snapshot().notice.is_some());

        // The user answers aloud; the review sees the speech.
        let id = snap.session_id.clone().unwrap();
        c.session.on_segment(seg(&id, Source::User, 5000, 9000, "I traced a race condition in the payment queue and fixed it with an idempotency key"));
        assert!(c.session.speech_since_question().contains("idempotency"));
        c.mock_feedback("");
        assert!(wait(&c.mock.feedback).error.is_none());
        assert!(seen.lock().unwrap().last().unwrap().contains("idempotency key"), "the review request carries the answer");

        // The debrief is written from the stored transcript and kept with the session.
        c.stop_session();
        let slot = c.debrief(&id);
        assert!(wait(&slot).error.is_none());
        assert_eq!(crate::db::s(c.db.session_detail(&id).session.as_ref().unwrap(), "summary"), "Tell me about a hard bug you fixed.");
        let d = c.db.session_detail(&id);
        assert!(d.answers.iter().any(|a| crate::db::s(a, "kind") == "feedback"), "the review is saved with the session");
        assert!(crate::export::export_session(&d, false).unwrap().contains("## "));
        assert_eq!(c.db.list_sessions("hard bug", 5).len(), 1, "history search reaches the debrief");
    }

    #[test]
    fn cloud_requests_respect_consent_and_offline_mode() {
        let c = core();
        let (cfg, _) = c.upsert_provider(ProviderConfig { id: String::new(), label: "Cloud".into(), preset: "openai".into(), protocol: ProviderProtocol::Openai, base_url: "https://api.openai.com/v1".into(), has_key: false, allow_fallback: false, timeout_ms: 1000, llama: None }, None).unwrap();
        c.update_settings(|s| {
            s.roles.insert(ModelRole::QuickAnswer, Some(RoleTarget { provider_id: cfg.id.clone(), model: "gpt".into() }));
        });
        let task = || Task { system: String::new(), user: String::new(), max_tokens: 1, temperature: 0.0 };
        let e = c.rt.block_on(c.run_task(ModelRole::QuickAnswer, task(), None, |_| {})).unwrap_err();
        assert!(e.contains("Privacy") || e.contains("Приватність"), "{e}");
        c.update_settings(|s| {
            s.consent.cloud = true;
            s.offline = true;
        });
        let e = c.rt.block_on(c.run_task(ModelRole::QuickAnswer, task(), None, |_| {})).unwrap_err();
        assert!(e.to_lowercase().contains("offline") || e.contains("Офлайн"), "{e}");
    }

    #[test]
    fn diagnostics_report_holds_no_keys() {
        let c = core();
        let (cfg, _) = c.upsert_provider(ProviderConfig { id: String::new(), label: "My secret employer proxy".into(), preset: "custom".into(), protocol: ProviderProtocol::Openai, base_url: "http://127.0.0.1:1/v1".into(), has_key: false, allow_fallback: false, timeout_ms: 1000, llama: None }, Some("sk-supersecret-1234567890")).unwrap();
        assert!(cfg.has_key);
        let path = c.data_dir.join("report.json");
        c.export_diagnostics(&path).unwrap();
        let report = std::fs::read_to_string(&path).unwrap();
        assert!(!report.contains("supersecret"));
        assert!(!report.contains("employer"), "provider names stay out of the report");
        assert!(report.contains(env!("CARGO_PKG_VERSION")));
    }
}
