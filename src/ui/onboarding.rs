use eframe::egui::{self, RichText};

use super::profile::{answer_lang_options, kind_label, preset_label, start_import};
use super::providers::{start_test, test_result, FormResult, ModelPicker, ProviderForm};
use super::settings_page::{asr_installer, asr_lang_options, device_settings, ui_lang_options, AudioUi};
use super::theme::p;
use super::widgets::*;
use super::Ctx;
use crate::i18n::t;
use crate::net::is_loopback_url;
use crate::providers::registry::ProviderTestReport;
use crate::tr;
use crate::types::*;

pub struct OnboardingState {
    step: usize,
    started: std::time::Instant,
    audio: AudioUi,
    form: Option<ProviderForm>,
    adding: bool,
    picker: ModelPicker,
    test: Option<Promise<ProviderTestReport>>,
    report: Option<ProviderTestReport>,
    import: Option<Promise<Option<Result<(), String>>>>,
    import_err: Option<String>,
    typed: String,
    test_until: Option<std::time::Instant>,
    agree: bool,
}

impl Default for OnboardingState {
    fn default() -> Self {
        Self {
            step: 0,
            started: std::time::Instant::now(),
            audio: AudioUi::default(),
            form: None,
            adding: false,
            picker: ModelPicker::default(),
            test: None,
            report: None,
            import: None,
            import_err: None,
            typed: String::new(),
            test_until: None,
            agree: false,
        }
    }
}

impl OnboardingState {
    pub fn set_step(&mut self, n: usize) {
        self.step = n;
    }
}

fn steps() -> [&'static str; 7] {
    [
        t("Мова", "Language"),
        t("Звук", "Audio"),
        t("Розпізнавання", "Recognition"),
        t("Модель", "Model"),
        t("Профіль", "Profile"),
        t("Перевірка", "Self-test"),
        t("Згода", "Consent"),
    ]
}

pub fn show(ui: &mut egui::Ui, c: &mut Ctx, st: &mut OnboardingState) {
    let core = c.core.clone();
    let n = steps().len();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.add_space(28.0);
        ui.vertical_centered(|ui| {
            ui.set_max_width(880.0);
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    super::brand(ui);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(faint(tr!("Крок {} з {}", "Step {} of {}", st.step + 1, n))));
                });
                ui.horizontal_wrapped(|ui| {
                    for (i, s) in steps().iter().enumerate() {
                        let c2 = p();
                        let (fill, fg) = if i == st.step {
                            (c2.accent, c2.accent_ink)
                        } else if i < st.step {
                            (c2.ok.gamma_multiply(0.18), c2.ok)
                        } else {
                            (c2.surface3, c2.muted)
                        };
                        egui::Frame::new().fill(fill).corner_radius(10).inner_margin(egui::Margin::symmetric(10, 3)).show(ui, |ui| ui.label(RichText::new(*s).size(12.5).color(fg)));
                    }
                });
                ui.add_space(8.0);
                ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                    ui.set_min_height(380.0);
                    match st.step {
                        0 => step_lang(ui, c),
                        1 => step_audio(ui, c, st),
                        2 => step_asr(ui, c, st),
                        3 => step_model(ui, c, st),
                        4 => step_profile(ui, c, st),
                        5 => step_test(ui, c, st),
                        _ => step_consent(ui, c, st),
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if st.step > 0 && ghost_sm(ui, t("Назад", "Back")) {
                        leave_step(&core, st);
                        st.step -= 1;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if st.step < n - 1 {
                            if primary(ui, t("Далі", "Next"), true) {
                                // Capture consent comes only from an explicit «Далі» on the audio step, for exactly the enabled sources.
                                if st.step == 1 {
                                    let a = core.s().audio;
                                    core.update_settings(|s| {
                                        s.consent.mic = a.capture_mic;
                                        s.consent.system = a.capture_system;
                                    });
                                }
                                leave_step(&core, st);
                                st.step += 1;
                            }
                            if st.step > 0 && ghost_sm(ui, t("Пропустити", "Skip")) {
                                leave_step(&core, st);
                                st.step += 1;
                            }
                        }
                    });
                });
            });
        });
    });
}

fn leave_step(core: &std::sync::Arc<crate::core::Core>, st: &mut OnboardingState) {
    if st.step == 5 {
        core.stop_session();
        st.test_until = None;
    }
}

fn step_lang(ui: &mut egui::Ui, c: &mut Ctx) {
    let core = c.core.clone();
    let s = c.s.clone();
    h1(ui, t("Ласкаво просимо до Copilot", "Welcome to Copilot"));
    ui.add(
        egui::Label::new(muted(t(
            "Помічник слухає розмову, вчасно помічає питання й показує коротку відповідь з опорою на ваше резюме. Розпізнавання мовлення — на цьому комп'ютері, модель для відповідей ви обираєте самі.",
            "The assistant listens to the conversation, notices questions in time and shows a short answer based on your CV. Speech recognition runs on this computer; you choose the answer model.",
        )))
        .wrap(),
    );
    ui.columns(3, |cols| {
        field(&mut cols[0], t("Мова інтерфейсу", "Interface language"), |ui| {
            let mut v = s.ui_lang;
            if combo(ui, "onb-ui", &mut v, &ui_lang_options(), ui.available_width()) {
                let speech = if v == Lang::En { LangPref::En } else { LangPref::Uk };
                core.update_settings(|s| {
                    s.ui_lang = v;
                    s.asr.language = speech;
                });
            }
        });
        field(&mut cols[1], t("Мова відповідей", "Answer language"), |ui| {
            let mut v = s.answer_lang;
            if combo(ui, "onb-ans", &mut v, &answer_lang_options(), ui.available_width()) {
                core.update_settings(|s| s.answer_lang = v);
            }
        });
        field(&mut cols[2], t("Мова розмови", "Conversation language"), |ui| {
            let mut v = s.asr.language;
            if combo(ui, "onb-asr", &mut v, &asr_lang_options(), ui.available_width()) {
                core.update_settings(|s| s.asr.language = v);
            }
        });
    });
    ui.label(faint(t("Займе близько 3 хвилин. Будь-який крок можна пропустити й налаштувати пізніше.", "Takes about 3 minutes. Any step can be skipped and set up later.")));
}

fn step_audio(ui: &mut egui::Ui, c: &mut Ctx, st: &mut OnboardingState) {
    h2(ui, t("Звук: ви й співрозмовник — окремо", "Audio: you and the other side — separately"));
    note(
        ui,
        t(
            "Мікрофон — це ви. Системний звук — голос співрозмовника із Zoom, Meet, Teams або браузера. Підказки будуються за мовленням співрозмовника.",
            "The microphone is you. System audio is the other side's voice from Zoom, Meet, Teams or a browser. Hints are built from the other side's speech.",
        ),
    );
    device_settings(ui, c.core, &mut st.audio);
}

fn step_asr(ui: &mut egui::Ui, c: &mut Ctx, st: &mut OnboardingState) {
    h2(ui, t("Розпізнавання мовлення", "Speech recognition"));
    note(
        ui,
        t(
            "Працює локально (whisper.cpp): аудіо не покидає комп'ютер. Потрібно один раз завантажити рушій і модель — розмір і перевірка контрольної суми показані нижче.",
            "Runs locally (whisper.cpp): audio never leaves the computer. Download the engine and model once — size and checksum verification are shown below.",
        ),
    );
    asr_installer(ui, c.core, &mut st.audio, true);
    ui.label(faint(t("Без розпізнавання можна вводити питання текстом.", "Without recognition you can type questions.")));
}

fn step_model(ui: &mut egui::Ui, c: &mut Ctx, st: &mut OnboardingState) {
    let core = c.core.clone();
    h2(ui, t("Модель для відповідей", "Answer model"));
    note(
        ui,
        t(
            "Свій ключ (BYOK) для будь-якого сервісу — OpenAI, Anthropic, Gemini, OpenRouter, Groq… — або локальна модель llama.cpp без інтернету й без ключа.",
            "Your own key (BYOK) for any service — OpenAI, Anthropic, Gemini, OpenRouter, Groq… — or a local llama.cpp model with no internet and no key.",
        ),
    );
    let providers = core.registry.list();
    let quick = c.s.role(ModelRole::QuickAnswer).cloned().or_else(|| c.s.roles.get(&ModelRole::QuickAnswer).cloned().flatten());
    if providers.is_empty() || st.adding {
        let form = st.form.get_or_insert_with(|| ProviderForm::new(None));
        match form.show(ui, &core, !providers.is_empty()) {
            FormResult::Saved(cfg) => {
                core.update_settings(|s| {
                    s.roles.insert(ModelRole::QuickAnswer, Some(RoleTarget { provider_id: cfg.id.clone(), model: String::new() }));
                });
                st.form = None;
                st.adding = false;
            }
            FormResult::Cancelled => {
                st.form = None;
                st.adding = false;
            }
            FormResult::None => {}
        }
        return;
    }
    let Some(pr) = providers.iter().find(|p| Some(&p.id) == quick.as_ref().map(|q| &q.provider_id)).or(providers.first()).cloned() else { return };
    if let Some(r) = st.test.as_ref().and_then(|p| p.take()) {
        st.test = None;
        st.report = Some(r);
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new(&pr.label).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ghost_sm(ui, t("Інший сервіс", "Another service")) {
                st.adding = true;
            }
        });
    });
    let mut model = quick.as_ref().filter(|q| q.provider_id == pr.id).map(|q| q.model.clone()).unwrap_or_default();
    if st.picker.show(ui, &core, &pr.id, &mut model) {
        let t2 = RoleTarget { provider_id: pr.id.clone(), model: model.clone() };
        core.update_settings(|s| {
            for r in [ModelRole::QuickAnswer, ModelRole::DeepAnswer, ModelRole::Vision] {
                s.roles.insert(r, Some(t2.clone()));
            }
        });
        st.report = None;
    }
    let busy = st.test.is_some();
    if button(ui, if busy { t("Перевіряю текст і зображення…", "Checking text and image…") } else { t("Перевірити з'єднання", "Test connection") }, Kind::Normal, false, !model.is_empty() && !busy).clicked() {
        st.test = Some(start_test(&core, &pr.id, &model));
    }
    if let Some(r) = &st.report {
        test_result(ui, r);
        if r.vision.as_ref().is_some_and(|v| !v.ok) {
            note(ui, t("Знімки екрана розпізнаватимуться локальним OCR. Окрему vision-модель можна вибрати в налаштуваннях.", "Screenshots will be read by local OCR. A separate vision model can be picked in settings."));
        }
    }
}

fn step_profile(ui: &mut egui::Ui, c: &mut Ctx, st: &mut OnboardingState) {
    let core = c.core.clone();
    h2(ui, t("Профіль", "Profile"));
    let p = core.profile();
    ui.horizontal_wrapped(|ui| {
        for k in ProfilePreset::ALL {
            if pill(ui, preset_label(k), p.preset == k, true) {
                core.upsert_profile(Profile { preset: k, ..p.clone() });
            }
        }
    });
    if let Some(r) = st.import.as_ref().and_then(|x| x.take()) {
        st.import = None;
        st.import_err = match r {
            Some(Err(e)) => Some(e),
            _ => None,
        };
    }
    ui.horizontal(|ui| {
        if btn(ui, t("Завантажити резюме (PDF/DOCX/TXT)", "Upload CV (PDF/DOCX/TXT)")) && st.import.is_none() {
            st.import = Some(start_import(&core, p.id.clone(), DocKind::Cv));
        }
        if button(ui, kind_label(DocKind::Jd), Kind::Ghost, false, true).clicked() && st.import.is_none() {
            st.import = Some(start_import(&core, p.id.clone(), DocKind::Jd));
        }
    });
    error_text(ui, &st.import_err);
    for d in core.db.list_docs(&p.id) {
        ui.label(small(tr!("✓ {} — {} фрагм.", "✓ {} — {} chunks", d.name, d.chunks)));
    }
    note(ui, t("Відповіді про ваш досвід будуються лише на завантажених фактах. Чого немає в резюме, застосунок не вигадує.", "Answers about your experience use only uploaded facts. What is not in the CV, the app does not invent."));
}

fn step_test(ui: &mut egui::Ui, c: &mut Ctx, st: &mut OnboardingState) {
    let core = c.core.clone();
    h2(ui, t("Перевірка за 30 секунд", "30-second check"));
    note(
        ui,
        t(
            "Натисніть «Почати» й поставте питання вголос, наприклад: «Що таке ідемпотентність у REST?» — або введіть його. З'явиться вікно підказок.",
            "Press “Start” and ask a question aloud, e.g. “What is idempotency in REST?” — or type it. The hints window appears.",
        ),
    );
    let left = st.test_until.map(|u| u.saturating_duration_since(std::time::Instant::now()).as_secs());
    if st.test_until.is_some() && left == Some(0) {
        core.stop_session();
        st.test_until = None;
    }
    if st.test_until.is_some() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
    }
    ui.horizontal(|ui| {
        let running = st.test_until.is_some();
        let label = if running { tr!("Триває перевірка… {} с", "Checking… {} s", left.unwrap_or(0)) } else { t("Почати", "Start").to_string() };
        if primary(ui, &label, !running) {
            core.start_session(SessionMode::Practice);
            st.test_until = Some(std::time::Instant::now() + std::time::Duration::from_secs(30));
        }
        let r = text_input(ui, &mut st.typed, t("Поставити питання текстом", "Type a question"), ui.available_width());
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !st.typed.trim().is_empty() {
            core.ask_typed(&st.typed);
            st.typed.clear();
        }
    });
    let snap = &c.snap;
    card_fill(ui, p().surface2, |ui| {
        let heard = snap.partial[0].clone().or_else(|| snap.segments.last().map(|s| s.text.clone())).unwrap_or("—".into());
        ui.label(muted(tr!("Розпізнано: {}", "Recognized: {}", heard)).size(12.5));
        if let Some(q) = &snap.active_question {
            ui.label(RichText::new(&q.text).strong());
        }
        if let Some(a) = snap.answers.get(&AnswerKind::Say) {
            ui.add(egui::Label::new(RichText::new(if a.text.is_empty() { "…" } else { &a.text }).size(17.0)).wrap());
            if let Some(ms) = a.first_token_ms {
                ui.label(faint(tr!("{} · {} · перший токен {:.1} с", "{} · {} · first token {:.1} s", a.provider, a.model, ms as f64 / 1000.0)));
            }
            if let Some(e) = &a.error {
                banner_text(ui, Tone::Error, &e.line());
            }
        }
    });
}

fn step_consent(ui: &mut egui::Ui, c: &mut Ctx, st: &mut OnboardingState) {
    let core = c.core.clone();
    let cs = c.s.consent.clone();
    h2(ui, t("Останнє: згода й чесність", "Last: consent and honesty"));
    banner_text(
        ui,
        Tone::Info,
        t(
            "Записувати звук і використовувати AI-підказки можна лише там, де це дозволено правилами й учасники згодні. Copilot не обходить прокторинг і засоби контролю та не обіцяє невидимості.",
            "Recording audio and using AI hints is only OK where rules allow it and participants agree. Copilot does not bypass proctoring or monitoring tools and does not promise invisibility.",
        ),
    );
    let mut h = cs.history;
    if toggle(ui, &mut h, t("Зберігати історію сесій на цьому комп'ютері", "Keep session history on this computer"), Some(t("Можна видалити будь-коли. Без історії немає відновлення після збою.", "Can be deleted any time. Without history there is no crash recovery."))) {
        core.update_settings(|s| s.consent.history = h);
    }
    let mut sc = cs.screen;
    if toggle(ui, &mut sc, t("Дозволити знімки екрана за моєю командою", "Allow screenshots on my command"), None) {
        core.update_settings(|s| s.consent.screen = sc);
    }
    let mut cl = cs.cloud;
    if toggle(
        ui,
        &mut cl,
        t("Дозволити надсилання в хмарну модель", "Allow sending to a cloud model"),
        Some(t(
            "Потрібно, лише якщо модель для відповідей — не на цьому комп'ютері. Іде питання, недавній транскрипт і доречні фрагменти документів.",
            "Needed only if the answer model is not on this computer. The question, recent transcript and relevant document fragments are sent.",
        )),
    ) {
        core.update_settings(|s| s.consent.cloud = cl);
    }
    if !st.agree {
        st.agree = cs.participants;
    }
    ui.checkbox(&mut st.agree, t("Я розумію й повідомлятиму учасників про запис і AI-обробку", "I understand and will tell participants about recording and AI processing"));
    ui.horizontal(|ui| {
        ui.label(faint(tr!("Налаштування зайняло {} хв.", "Setup took {} min.", (st.started.elapsed().as_secs() / 60).max(1))));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if primary(ui, t("Готово", "Done"), st.agree) {
                // Auto-answer is on by default only for a local model: for a paid one every false "question" costs money.
                let s = core.s();
                let local = s.role(ModelRole::QuickAnswer).and_then(|q| core.registry.get(&q.provider_id)).is_some_and(|p| is_loopback_url(&p.base_url));
                core.update_settings(|s| {
                    s.onboarded = true;
                    s.auto_answer = local;
                    s.consent.participants = true;
                    s.consent.accepted_at = Some(now_ms());
                });
            }
        });
    });
}
