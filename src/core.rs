//! The app core: owns storage, providers, session, ASR, capture and the window side effects. The UI calls into it;
//! hotkeys and the tray reach it through the UI thread.

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
use crate::budget::BudgetManager;
use crate::context::prompt::ScreenInput;
use crate::db::{Db, HistoryDetail};
use crate::i18n::t;
use crate::llama::{LlamaManager, LlamaState};
use crate::net::{check_endpoint, Net};
use crate::providers::presets::preset;
use crate::providers::registry::{ProviderRegistry, ProviderTestReport, ResolvedTarget};
use crate::providers::runner::{run_stream, RunOptions};
use crate::screen::Shot;
use crate::secrets::SecretStore;
use crate::session::manager::{SessionDeps, SessionManager};
use crate::settings::SettingsStore;
use crate::tr;
use crate::types::*;
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
        Ok(core)
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
                capture_system: s.audio.capture_system && s.consent.system,
            },
            self.sink(),
        );
        if mode != SessionMode::Meeting {
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
        if !t.is_local {
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
        self.session.stop();
        self.capture.stop();
        self.schedule_asr_idle_stop();
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
        if cfg.preset != "llamacpp" {
            cfg.llama = None;
        }
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
