//! `COPILOT_SNAPSHOT=<dir>`: renders every screen with the windows hidden and saves PNGs, then exits.
//! Used to check layouts and translations without putting a window on screen, and to refresh README screenshots.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eframe::egui;

use super::{overlay_id, Page, Tab};
use crate::core::Core;
use crate::types::*;

#[derive(Clone, Copy)]
pub enum Step {
    Lang(Lang),
    Onboarding(usize),
    Page(Page, Option<Tab>),
    Overlay,
}

pub struct Snapshot {
    dir: PathBuf,
    steps: Vec<(Step, String)>,
    i: usize,
    shots: u8,
    pending: Option<Arc<AtomicBool>>,
}

impl Snapshot {
    pub fn from_env() -> Option<Self> {
        let dir = PathBuf::from(std::env::var_os("COPILOT_SNAPSHOT")?);
        let _ = std::fs::create_dir_all(&dir);
        let mut steps = Vec::new();
        // COPILOT_SNAPSHOT_LANGS=de,ja checks the layouts of other languages.
        let langs = std::env::var("COPILOT_SNAPSHOT_LANGS").unwrap_or_else(|_| "uk,en".into());
        for tag in langs.split(',') {
            let Some(lang) = Lang::parse(tag.trim()) else { continue };
            steps.push((Step::Lang(lang), String::new()));
            for n in 0..7 {
                steps.push((Step::Onboarding(n), format!("{tag}-onboarding-{n}")));
            }
            for (p, tab, name) in [
                (Page::Home, None, "home"),
                (Page::Live, None, "live"),
                (Page::Live, None, "mock"),
                (Page::History, None, "history"),
                (Page::Profile, None, "profile"),
                (Page::Settings, Some(Tab::Providers), "settings-providers"),
                (Page::Settings, Some(Tab::Audio), "settings-audio"),
                (Page::Settings, Some(Tab::Overlay), "settings-overlay"),
                (Page::Settings, Some(Tab::Shortcuts), "settings-shortcuts"),
                (Page::Settings, Some(Tab::Privacy), "settings-privacy"),
                (Page::Settings, Some(Tab::Usage), "settings-usage"),
                (Page::Settings, Some(Tab::Diagnostics), "settings-diagnostics"),
            ] {
                steps.push((Step::Page(p, tab), format!("{tag}-{name}")));
            }
            steps.push((Step::Overlay, format!("{tag}-overlay")));
        }
        Some(Self { dir, steps, i: 0, shots: 0, pending: None })
    }

    /// Current step (already applied); None when finished.
    pub fn step(&self) -> Option<Step> {
        self.steps.get(self.i).map(|s| s.0)
    }

    pub fn tick(&mut self, ctx: &egui::Context, core: &Arc<Core>) {
        let Some((step, name)) = self.steps.get(self.i).cloned() else { core.shutdown() };
        match step {
            Step::Lang(l) => {
                core.update_settings(|s| s.ui_lang = l);
                self.i += 1;
                ctx.request_repaint();
                return;
            }
            Step::Onboarding(_) => core.update_settings(|s| s.onboarded = false),
            Step::Page(..) | Step::Overlay => {
                core.update_settings(|s| s.onboarded = true);
                demo(core);
                core.session.with_snap(|s| s.mode = Some(if name.ends_with("mock") { SessionMode::Mock } else { SessionMode::Practice }));
            }
        }
        if let Some(done) = &self.pending {
            if !done.load(Ordering::SeqCst) {
                ctx.request_repaint();
                ctx.request_repaint_of(overlay_id());
                return;
            }
            self.pending = None;
            self.shots += 1;
            if self.shots == 2 {
                self.shots = 0;
                self.i += 1;
                ctx.request_repaint();
                return;
            }
        }
        // The first shot lets layout settle; only the second is saved.
        let done = Arc::new(AtomicBool::new(false));
        let (d2, path, save) = (done.clone(), self.dir.join(format!("{name}.png")), self.shots == 1);
        let cb = egui::ScreenshotCallback::new(move |img: Arc<egui::ColorImage>| {
            if save {
                let [w, h] = img.size;
                let raw: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
                if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, raw) {
                    let _ = buf.save(&path);
                }
            }
            d2.store(true, Ordering::SeqCst);
        });
        let target = if matches!(step, Step::Overlay) { overlay_id() } else { egui::ViewportId::ROOT };
        ctx.send_viewport_cmd_to(target, egui::ViewportCommand::Screenshot(cb));
        ctx.request_repaint();
        ctx.request_repaint_of(overlay_id());
        self.pending = Some(done);
    }
}

/// A plausible live state so the session page and overlay have something to show.
fn demo(core: &Core) {
    let en = crate::i18n::en();
    demo_setup(core, en);
    core.found.lock().unwrap().get_or_insert_with(|| {
        use crate::discover::{Found, GgufModel};
        vec![
            Found::Server { runtime: "Ollama", base_url: "http://127.0.0.1:11434/v1".into(), models: vec!["qwen3.5:4b".into(), "gemma3:12b".into()] },
            Found::Gguf(GgufModel { path: "Qwen3.5-4B-Q4_K_M.gguf".into(), name: "Qwen3.5-4B-Q4_K_M".into(), size: 2_740_000_000, mmproj: None, origin: "LM Studio" }),
        ]
    });
    core.session.with_snap(|s| {
        let q = if en { "How do database indexes speed up queries, and what do they cost?" } else { "Як індекси в базі даних прискорюють запити і яка їхня ціна?" };
        let say = if en {
            "An index is a sorted structure, usually a B-tree, that lets the database find rows in logarithmic time instead of scanning the table, at the cost of slower writes and extra storage."
        } else {
            "Індекс — це відсортована структура, зазвичай B-дерево: база знаходить рядки за логарифмічний час замість повного сканування, але платить повільнішими записами й додатковим місцем."
        };
        let points = if en {
            "• B-tree gives O(log n) lookups and range scans\n• Every INSERT/UPDATE also updates the index\n• Composite index order must match the query filters\n• Check the plan with EXPLAIN ANALYZE"
        } else {
            "• B-дерево дає пошук і діапазони за O(log n)\n• Кожен INSERT/UPDATE оновлює й індекс\n• Порядок полів складеного індексу має відповідати фільтрам\n• План перевіряйте через EXPLAIN ANALYZE"
        };
        s.session_id = Some("demo".into());
        s.mode = Some(SessionMode::Practice);
        s.listening = true;
        s.state = ConvState::AnswerReady;
        s.active_question = Some(ActiveQuestion { question_id: "q".into(), revision: 1, text: q.into(), source: Source::Remote, kind: UtteranceKind::Question, trigger: QuestionTrigger::Auto });
        let ev = |kind, text: &str| AnswerEvent {
            session_id: "demo".into(),
            question_id: "q".into(),
            revision: 1,
            answer_id: format!("{kind:?}"),
            kind,
            provider: "llamacpp".into(),
            model: "qwen2.5-14b-instruct-q5_k_m".into(),
            prompt_preset: String::new(),
            text: text.into(),
            status: AnswerStatus::Done,
            first_token_ms: Some(420),
            latency_ms: Some(2900),
            usage: Some(Usage { input_tokens: 900, output_tokens: 80, exact: true }),
            usage_cost_usd: Some(0.0),
            error: None,
        };
        s.answers.insert(AnswerKind::Say, ev(AnswerKind::Say, say));
        s.answers.insert(AnswerKind::Points, ev(AnswerKind::Points, points));
        s.segments = vec![TranscriptSegment {
            session_id: "demo".into(),
            segment_id: "s1".into(),
            source: Source::Remote,
            start_ms: 0,
            end_ms: 3000,
            text: q.into(),
            is_final: true,
            lang: None,
        }];
        s.levels = [0.08, 0.2];
    });
}

/// A configured app: a connected model and a finished session with its debrief. Screenshot runs use their own data folder.
fn demo_setup(core: &Core, en: bool) {
    if core.registry.list().is_empty() {
        let cfg = ProviderConfig { id: String::new(), label: "Ollama".into(), preset: "custom".into(), protocol: ProviderProtocol::Openai, base_url: "http://127.0.0.1:11434/v1".into(), has_key: false, allow_fallback: false, timeout_ms: 30_000, llama: None };
        if let Ok((saved, _)) = core.upsert_provider(cfg, None) {
            core.registry.text_ok.lock().unwrap().insert(format!("{}/qwen3.5:4b", saved.id), true);
            core.update_settings(|s| {
                let target = RoleTarget { provider_id: saved.id.clone(), model: "qwen3.5:4b".into() };
                s.roles.insert(ModelRole::QuickAnswer, Some(target.clone()));
                s.roles.insert(ModelRole::DeepAnswer, Some(target));
                s.consent.history = true;
                s.consent.mic = true;
                s.consent.system = true;
            });
        }
        let id = new_id();
        let p = core.profile();
        core.db.start_session(&id, &p.id, "live", &format!("{} · Senior Backend", crate::i18n::fmt_datetime(now_ms() - 3_600_000)));
        let lines: [(Source, &str); 3] = if en {
            [(Source::Remote, "How do database indexes speed up queries?"), (Source::User, "An index is a sorted structure, usually a B-tree, so the database avoids scanning the table."), (Source::Remote, "And what do they cost?")]
        } else {
            [(Source::Remote, "Як індекси прискорюють запити в базі даних?"), (Source::User, "Індекс — це відсортована структура, зазвичай B-дерево, тому база не сканує всю таблицю."), (Source::Remote, "А яка їхня ціна?")]
        };
        for (i, (source, text)) in lines.iter().enumerate() {
            core.db.add_segment(&TranscriptSegment { session_id: id.clone(), segment_id: new_id(), source: *source, start_ms: i as i64 * 5000, end_ms: i as i64 * 5000 + 4000, text: (*text).into(), is_final: true, lang: None });
        }
        core.db.end_session(&id);
        core.db.set_summary(&id, if en {
            "## How it went\nA focused technical conversation about databases; the core answers were clear.\n\n## Gaps to close\n- Give numbers when talking about write overhead\n\n## Prepare for next time\n- Composite index ordering\n- Reading EXPLAIN ANALYZE\n- When not to index"
        } else {
            "## Як пройшло\nПредметна технічна розмова про бази даних, основні відповіді були чіткі.\n\n## Що підтягнути\n- Наводити цифри, коли йдеться про ціну запису\n\n## До наступного разу\n- Порядок полів у складеному індексі\n- Читання EXPLAIN ANALYZE\n- Коли індекс не потрібен"
        });
    }
    core.session.with_snap(|s| {
        s.status.mic = ComponentStatus::new(ComponentState::Ok, if en { "Headset microphone" } else { "Мікрофон гарнітури" });
        s.status.system = ComponentStatus::new(ComponentState::Ok, "WASAPI loopback");
        s.status.asr = ComponentStatus::new(ComponentState::Ok, format!("whisper base · {}", if en { "local" } else { "локально" }));
        s.status.overlay = ComponentStatus::new(ComponentState::Ok, if en { "compact" } else { "компактний" });
    });
    *core.mock.feedback.lock().unwrap() = crate::core::Gen {
        text: if en {
            "## Score\n7/10: correct and clear, but with no example from your own work.\n\n## What was missing\n- The cost on writes\n- A real case from your projects".into()
        } else {
            "## Оцінка\n7/10: правильно й чітко, але без прикладу з власної роботи.\n\n## Чого бракувало\n- Ціни для записів\n- Реального випадку з ваших проєктів".into()
        },
        ..Default::default()
    };
}
