use eframe::egui::{self, RichText};

use super::md::markdown;
use super::theme::p;
use super::widgets::*;
use super::Ctx;
use crate::i18n::t;
use crate::tr;
use crate::types::*;

#[derive(Default)]
pub struct LiveState {
    typed: String,
    editing: Option<String>,
}

pub fn provenance(a: &AnswerEvent) -> String {
    if a.model.is_empty() {
        return String::new();
    }
    let mut s = format!("{} · {}", a.provider, a.model);
    if let Some(ms) = a.first_token_ms {
        s += &tr!(" · перший токен {:.1} с", " · first token {:.1} s", ms as f64 / 1000.0);
    }
    if a.status == AnswerStatus::Done {
        s += &match a.usage_cost_usd {
            None => format!(" · {}", t("Ціна невідома", "Price unknown")),
            Some(0.0) => t(" · локально, безкоштовно", " · local, free").to_string(),
            Some(c) => format!(" · ${c:.4}"),
        };
    }
    if a.usage.is_some_and(|u| !u.exact) {
        s += t(" · токени оцінено", " · tokens estimated");
    }
    s
}

fn err_line(ui: &mut egui::Ui, a: &AnswerEvent) {
    if let Some(e) = &a.error {
        banner_text(ui, Tone::Error, &e.line());
    }
}

pub fn show(ui: &mut egui::Ui, c: &mut Ctx, st: &mut LiveState) {
    let snap = c.snap.clone();
    let core = c.core.clone();
    let s = c.s.clone();
    let q = snap.active_question.clone();

    card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            if snap.session_id.is_some() {
                if button(ui, t("Відповісти зараз", "Answer now"), Kind::Primary, false, true).on_hover_text(pretty_accel(&s.shortcuts.answer_now)).clicked() {
                    core.session.answer_now(AnswerKind::Say, QuestionTrigger::Manual);
                }
                if button(ui, t("Детальніше", "Deep dive"), Kind::Normal, false, q.is_some()).clicked() {
                    core.session.answer_now(AnswerKind::Deep, QuestionTrigger::Manual);
                }
                if btn(ui, t("Наступне питання", "Next question")) {
                    core.session.next_question();
                }
                if btn(ui, t("Знімок екрана", "Screenshot")) {
                    core.begin_region();
                }
                ui.separator();
                ui.label(muted(t("Я", "Me")).size(12.5));
                meter(ui, snap.levels[0], 70.0);
                ui.label(muted(t("Співрозмовник", "Other side")).size(12.5));
                meter(ui, snap.levels[1], 70.0);
                ui.separator();
                if btn(ui, if snap.listening { t("Пауза", "Pause") } else { t("Продовжити", "Resume") }) {
                    core.toggle_listen();
                }
                if btn(ui, t("Завершити", "Stop")) {
                    core.stop_session();
                }
            } else {
                ui.label(muted(format!("{}.", t("Сесію не запущено", "No active session"))));
                if primary(ui, t("Тренування", "Practice"), true) {
                    core.start_session(SessionMode::Practice);
                }
                if btn(ui, t("Інтерв'ю / дзвінок", "Interview / call")) {
                    core.start_session(SessionMode::Live);
                }
            }
            if button(ui, t("Оверлей", "Overlay"), Kind::Ghost, false, true).clicked() {
                core.toggle_overlay();
            }
        });
    });
    ui.add_space(10.0);

    let height = (ui.clip_rect().height() - 150.0).max(460.0);
    ui.columns(2, |cols| {
        // Transcript
        card(&mut cols[0], |ui| {
            ui.set_min_height(height);
            ui.horizontal(|ui| {
                h2(ui, t("Транскрипт", "Transcript"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(faint(tr!("локально · останні {} хв ідуть у контекст", "local · last {} min go to context", s.context_window_min)));
                });
            });
            egui::ScrollArea::vertical().id_salt("tr").auto_shrink([false, false]).stick_to_bottom(true).max_height(height - 90.0).show(ui, |ui| {
                let who = |src: Source| if src == Source::Remote { t("Співрозмовник", "Other side") } else { t("Я", "Me") };
                for seg in &snap.segments {
                    line(ui, who(seg.source), &seg.text, seg.source, false);
                }
                for src in [Source::Remote, Source::User] {
                    if let Some(p) = &snap.partial[src.idx()] {
                        line(ui, who(src), p, src, true);
                    }
                }
                if snap.segments.is_empty() && snap.partial.iter().all(Option::is_none) {
                    let msg = match (&snap.session_id, snap.listening) {
                        (Some(_), true) => t("Слухаю… Говоріть або увімкніть звук співрозмовника.", "Listening… Speak or turn on the other side's audio."),
                        (Some(_), false) => t("Пауза.", "Paused."),
                        (None, _) => t("Тут з'явиться розпізнане мовлення.", "Recognized speech will appear here."),
                    };
                    ui.label(faint(msg));
                }
            });
            ui.horizontal(|ui| {
                let w = ui.available_width() - 110.0;
                let r = text_input(ui, &mut st.typed, t("Поставити питання текстом", "Type a question"), w);
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if (primary(ui, t("Надіслати", "Send"), !st.typed.trim().is_empty()) || enter) && !st.typed.trim().is_empty() {
                    core.ask_typed(&st.typed);
                    st.typed.clear();
                }
            });
        });

        // Answer
        card(&mut cols[1], |ui| {
            ui.set_min_height(height);
            if let Some(b) = &snap.budget_blocked {
                banner(ui, Tone::Error, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(small(tr!("{}. Нові платні запити зупинено до вашого рішення.", "{}. New paid requests are paused until you decide.", b)));
                        if btn_sm(ui, t("Продовжити понад ліміт", "Continue over the limit")) {
                            core.session.approve_budget();
                        }
                    });
                });
            }
            if let Some(n) = &snap.notice {
                banner_text(ui, Tone::Info, n);
            }
            for cand in &snap.candidates {
                banner(ui, Tone::Warn, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(muted(format!("{}:", t("Можливе питання", "Possible question"))));
                        ui.label(&cand.text);
                        if btn_sm(ui, t("Це питання", "It is a question")) {
                            core.session.confirm_candidate(&cand.question_id);
                        }
                        if ghost_sm(ui, t("Ні", "No")) {
                            core.session.dismiss_candidate(&cand.question_id);
                        }
                    });
                });
            }
            match &q {
                Some(q) => {
                    ui.horizontal(|ui| {
                        let rev = if q.revision > 1 { tr!(" · ревізія {}", " · revision {}", q.revision) } else { String::new() };
                        let src = if q.source == Source::Remote { t("співрозмовник", "other side") } else { t("ви", "you") };
                        ui.label(muted(format!("{}{rev} · {src} · {}", t("Питання", "Question"), kind_label(q.kind))).size(12.5));
                        if st.editing.is_none() {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if button(ui, t("Виправити", "Fix"), Kind::Ghost, true, true).on_hover_text(t("Якщо розпізнано неправильно", "If it was misrecognized")).clicked() {
                                    st.editing = Some(q.text.clone());
                                }
                            });
                        }
                    });
                    if let Some(ed) = st.editing.as_mut() {
                        let mut submit = false;
                        let mut cancel = false;
                        ui.horizontal(|ui| {
                            let r = text_input(ui, ed, "", ui.available_width() - 260.0);
                            submit = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            submit |= btn_sm(ui, t("Відповісти на виправлене", "Answer the corrected one"));
                            cancel = ghost_sm(ui, t("Скасувати", "Cancel"));
                        });
                        if submit {
                            core.session.edit_question(ed);
                        }
                        if submit || cancel {
                            st.editing = None;
                        }
                    } else {
                        ui.add(egui::Label::new(RichText::new(&q.text).font(super::theme::semibold(18.0))).wrap());
                    }
                    ui.add_space(6.0);
                    let a = &snap.answers;
                    if let Some(main) = a.get(&AnswerKind::Screen).or(a.get(&AnswerKind::Say)) {
                        egui::Frame::new().fill(p().surface2).corner_radius(8).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            if main.kind == AnswerKind::Screen {
                                markdown(ui, if main.text.is_empty() { "…" } else { &main.text }, 14.0);
                            } else {
                                ui.add(egui::Label::new(RichText::new(if main.text.is_empty() { "…" } else { &main.text }).size(16.0)).wrap().selectable(true));
                            }
                            ui.label(faint(provenance(main)));
                            err_line(ui, main);
                        });
                    }
                    if let Some(pts) = a.get(&AnswerKind::Points) {
                        h3(ui, t("Тези", "Talking points"));
                        markdown(ui, if pts.text.is_empty() { "…" } else { &pts.text }, 14.0);
                        err_line(ui, pts);
                    }
                    if let Some(deep) = a.get(&AnswerKind::Deep) {
                        h3(ui, t("Детальніше", "Deep dive"));
                        markdown(ui, if deep.text.is_empty() { "…" } else { &deep.text }, 14.0);
                        ui.label(faint(provenance(deep)));
                        err_line(ui, deep);
                    }
                    if snap.state == ConvState::QuestionConfirmed && !a.contains_key(&AnswerKind::Say) {
                        banner(ui, Tone::Info, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(small(t("Питання розпізнано. Автовідповідь вимкнено —", "Question recognized. Auto-answer is off —")));
                                if primary(ui, t("Відповісти", "Answer"), true) {
                                    core.session.answer_now(AnswerKind::Say, QuestionTrigger::Manual);
                                }
                                ui.label(small(t("або", "or")));
                                kbd(ui, &pretty_accel(&s.shortcuts.answer_now));
                            });
                        });
                    }
                }
                None => {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("❝").size(34.0).color(p().faint));
                        ui.label(muted(if snap.session_id.is_some() {
                            t("Щойно пролунає питання — тут з'явиться відповідь.", "As soon as a question is asked, the answer appears here.")
                        } else {
                            t("Почніть сесію або поставте питання текстом ліворуч.", "Start a session or type a question on the left.")
                        }));
                    });
                }
            }
            ui.add_space(8.0);
            let cost = snap.cost;
            let usd = cost.usd.map(|u| format!("${u:.4}")).unwrap_or("—".into());
            let mut line = tr!("Витрати сесії: {} · запитів {}", "Session spend: {} · requests {}", usd, cost.requests);
            if cost.unknown_priced > 0 {
                line += &tr!(" · {} без ціни (ліміт $ для них не контролюється)", " · {} without price ($ limit not enforced for them)", cost.unknown_priced);
            }
            ui.label(faint(line));
        });
    });

    if !snap.recent_questions.is_empty() {
        ui.add_space(10.0);
        card(ui, |ui| {
            h3(ui, t("Останні питання", "Recent questions"));
            for (_, text) in &snap.recent_questions {
                ui.label(small(format!("· {text}")));
            }
        });
    }
}

fn kind_label(k: UtteranceKind) -> &'static str {
    match k {
        UtteranceKind::Question => t("питання", "question"),
        UtteranceKind::FollowUp => t("уточнення", "follow-up"),
        UtteranceKind::CodingTask => t("задача з коду", "coding task"),
        UtteranceKind::ScreenReference => t("про екран", "about the screen"),
        UtteranceKind::Statement => t("твердження", "statement"),
        UtteranceKind::SmallTalk => t("small talk", "small talk"),
        UtteranceKind::Unknown => t("невідомо", "unknown"),
    }
}

fn line(ui: &mut egui::Ui, who: &str, text: &str, src: Source, partial: bool) {
    ui.horizontal_top(|ui| {
        let col = if src == Source::Remote { p().accent } else { p().info };
        ui.add_sized([96.0, 18.0], egui::Label::new(RichText::new(who).size(12.0).color(col)));
        let mut rt = RichText::new(text);
        if partial {
            rt = rt.italics().color(p().muted);
        }
        ui.add(egui::Label::new(rt).wrap().selectable(true));
    });
}
