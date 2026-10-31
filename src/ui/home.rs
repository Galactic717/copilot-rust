use eframe::egui::{self, RichText};

use super::widgets::*;
use super::{Ctx, Page, Tab};
use crate::i18n::{fmt_date, t};
use crate::tr;
use crate::types::*;

pub struct HomeState {
    mode: SessionMode,
}

impl Default for HomeState {
    fn default() -> Self {
        Self { mode: SessionMode::Practice }
    }
}

pub fn show(ui: &mut egui::Ui, c: &mut Ctx, st: &mut HomeState) {
    let snap = c.snap.clone();
    let s = c.s.clone();
    let core = c.core.clone();
    if let (Some(n), None) = (&snap.notice, &snap.session_id) {
        banner_text(ui, Tone::Warn, n);
        ui.add_space(6.0);
    }
    let no_model = s.role(ModelRole::QuickAnswer).is_none();
    let no_asr = snap.status.asr.state == ComponentState::Warn || (snap.status.asr.state == ComponentState::Error && snap.session_id.is_none());
    let active = snap.session_id.is_some();

    card(ui, |ui| {
        ui.columns(2, |cols| {
            let ui = &mut cols[0];
            h1(ui, if active { t("Сесія триває", "Session in progress") } else { t("Готові до розмови?", "Ready to talk?") });
            ui.horizontal_wrapped(|ui| {
                for (m, label) in [
                    (SessionMode::Practice, t("Тренування", "Practice")),
                    (SessionMode::Live, t("Інтерв'ю / дзвінок", "Interview / call")),
                    (SessionMode::Meeting, t("Зустріч (лише транскрипт)", "Meeting (transcript only)")),
                ] {
                    if pill(ui, label, st.mode == m, !active) {
                        st.mode = m;
                    }
                }
            });
            note(
                ui,
                match st.mode {
                    SessionMode::Practice => t(
                        "Питання можна читати вголос самому або вводити текстом — підказки й на ваш голос.",
                        "Read questions aloud yourself or type them — hints respond to your voice too.",
                    ),
                    SessionMode::Live => t(
                        "Підказки лише на мовлення співрозмовника (системний звук). Ваш голос — у транскрипт і контекст.",
                        "Hints only for the other side's speech (system audio). Your voice goes to the transcript and context.",
                    ),
                    SessionMode::Meeting => t("Без підказок: транскрипт зустрічі й питання зберігаються локально.", "No hints: the meeting transcript and questions are stored locally."),
                },
            );
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if active {
                    if btn(ui, t("Відкрити сесію", "Open session")) {
                        c.nav = Some((Page::Live, None));
                    }
                    if btn(ui, t("Завершити", "Stop")) {
                        core.stop_session();
                    }
                } else {
                    if primary(ui, t("Почати сесію", "Start session"), !(no_asr && no_model)) {
                        core.start_session(st.mode);
                    }
                    if let Some(r) = core.db.last_resumable() {
                        let tip = tr!("{} · {} реплік", "{} · {} lines", r.title, r.segments);
                        if button(ui, t("Продовжити минулу сесію", "Resume previous session"), Kind::Normal, false, true).on_hover_text(tip).clicked() {
                            core.resume_session();
                            c.nav = Some((Page::Live, None));
                        }
                    }
                }
                let profiles = core.db.list_profiles();
                let mut cur = s.active_profile_id.clone();
                let opts: Vec<(String, String)> = profiles.iter().map(|p| (p.id.clone(), p.name.clone())).collect();
                ui.add_enabled_ui(!active, |ui| {
                    if combo(ui, "home-profile", &mut cur, &opts, 200.0) {
                        core.update_settings(|s| s.active_profile_id = cur);
                        core.session.reload_knowledge();
                    }
                });
            });
            if no_model || no_asr {
                banner(ui, Tone::Warn, |ui| {
                    if no_asr {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(small(t("Розпізнавання мовлення не встановлено.", "Speech recognition is not installed.")));
                            if ui.link(small(t("Встановити", "Install"))).clicked() {
                                c.nav = Some((Page::Settings, Some(Tab::Audio)));
                            }
                            ui.label(small(t("— або ставте питання текстом.", "— or type your questions.")));
                        });
                    }
                    if no_model {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(small(t("Не вибрано модель для відповідей.", "No answer model selected.")));
                            if ui.link(small(t("Підключити сервіс", "Connect a service"))).clicked() {
                                c.nav = Some((Page::Settings, Some(Tab::Providers)));
                            }
                        });
                    }
                });
            }
            let ui = &mut cols[1];
            card_fill(ui, theme_surface2(), |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                status_row(ui, t("Мікрофон", "Microphone"), &snap.status.mic);
                status_row(ui, t("Звук співрозмовника", "Remote audio"), &snap.status.system);
                status_row(ui, t("Розпізнавання", "Speech recognition"), &snap.status.asr);
                status_row(ui, t("Модель відповідей", "Answer model"), &snap.status.llm);
                status_row(ui, t("Зображення", "Vision"), &snap.status.vision);
                status_row(ui, t("Оверлей", "Overlay"), &snap.status.overlay);
                status_row(ui, t("Захист екрана", "Capture protection"), &snap.status.protection);
                status_row(ui, t("Мережа", "Network"), &snap.status.network);
            });
        });
    });
    ui.add_space(10.0);

    ui.columns(2, |cols| {
        let preview = core.outgoing_preview();
        card(&mut cols[0], |ui| {
            h2(ui, t("Що йде провайдеру", "What is sent to the provider"));
            if preview.recipients.is_empty() {
                note(ui, t("Провайдера не вибрано — нікуди.", "No provider selected — nowhere."));
            }
            for r in &preview.recipients {
                ui.horizontal(|ui| {
                    chip(ui, if r.local { t("на цьому ПК", "this PC") } else { t("хмара", "cloud") }, !r.local);
                    ui.label(RichText::new(&r.label).strong());
                    let host = reqwest::Url::parse(&r.base_url).ok().and_then(|u| u.host_str().map(String::from)).unwrap_or_default();
                    ui.label(faint(host).monospace());
                });
            }
            for i in &preview.items {
                ui.horizontal_top(|ui| {
                    ui.label(muted("•"));
                    note(ui, i);
                });
            }
        });
        card(&mut cols[1], |ui| {
            h2(ui, t("Гарячі клавіші", "Hotkeys"));
            let k = &s.shortcuts;
            egui::Grid::new("home-keys").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                for (label, accel) in [
                    (t("Показати/сховати підказки", "Show/hide hints"), &k.toggle_overlay),
                    (t("Відповісти зараз", "Answer now"), &k.answer_now),
                    (t("Пауза / продовжити", "Pause / resume"), &k.toggle_listen),
                    (t("Знімок області екрана", "Screen region snapshot"), &k.snap_region),
                    (t("Розгорнути / детально", "Expand / deep dive"), &k.expand),
                    (t("Аварійна зупинка", "Emergency stop"), &k.panic),
                ] {
                    ui.label(small(label));
                    kbd(ui, &pretty_accel(accel));
                    ui.end_row();
                }
            });
        });
    });
    ui.add_space(10.0);

    section(
        ui,
        t("Останні сесії", "Recent sessions"),
        |ui| {
            if ghost_sm(ui, t("Уся історія →", "All history →")) {
                c.nav = Some((Page::History, None));
            }
        },
        |ui| {
            let sessions = core.db.list_sessions("", 5);
            if sessions.is_empty() {
                let tail = if s.history && s.consent.history { "" } else { t(" Збереження історії вимкнено.", " History saving is off.") };
                note(ui, &format!("{}{tail}", t("Поки немає.", "None yet.")));
            }
            for x in sessions {
                ui.horizontal(|ui| {
                    ui.add_sized([110.0, 18.0], egui::Label::new(faint(fmt_date(x.started_at))));
                    ui.add(egui::Label::new(small(&x.title)).truncate());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if x.status == "interrupted" {
                            chip(ui, t("перервана", "interrupted"), false);
                        }
                        ui.label(muted(tr!("{} пит.", "{} q.", x.questions)).size(12.5));
                    });
                });
            }
        },
    );
}

pub fn theme_surface2() -> egui::Color32 {
    super::theme::p().surface2
}
