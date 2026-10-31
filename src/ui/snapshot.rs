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
        for (lang, tag) in [(Lang::Uk, "uk"), (Lang::En, "en")] {
            steps.push((Step::Lang(lang), String::new()));
            for n in 0..7 {
                steps.push((Step::Onboarding(n), format!("{tag}-onboarding-{n}")));
            }
            for (p, tab, name) in [
                (Page::Home, None, "home"),
                (Page::Live, None, "live"),
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
