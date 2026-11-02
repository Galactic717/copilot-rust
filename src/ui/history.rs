use eframe::egui::{self, RichText};

use super::md::markdown;
use super::theme::p;
use super::widgets::*;
use super::Ctx;
use crate::db::s as col;
use crate::i18n::{fmt_date, t};

#[derive(Default)]
pub struct HistoryState {
    query: String,
    open: Option<String>,
    msg: Option<String>,
    confirm_all: bool,
    export: Option<Promise<Option<Result<(), String>>>>,
}

pub fn show(ui: &mut egui::Ui, c: &mut Ctx, st: &mut HistoryState) {
    let core = c.core.clone();
    if let Some(r) = st.export.as_ref().and_then(|p| p.take()) {
        st.export = None;
        st.msg = match r {
            Some(Ok(())) => Some(t("Експортовано", "Exported").into()),
            Some(Err(e)) => Some(e),
            None => None,
        };
    }
    ui.horizontal(|ui| {
        h1(ui, t("Історія", "History"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if st.confirm_all {
                if ghost_sm(ui, t("Скасувати", "Cancel")) {
                    st.confirm_all = false;
                }
                if danger_sm(ui, t("Так, видалити все", "Yes, delete everything")) {
                    core.db.delete_all_history();
                    st.confirm_all = false;
                    st.open = None;
                    st.msg = Some(t("Усю історію видалено з цього комп'ютера.", "All history was deleted from this computer.").into());
                }
                ui.label(small(t("Видалити всі сесії, транскрипти й журнал без можливості відновлення?", "Delete all sessions, transcripts and the log irreversibly?")));
            } else if danger_sm(ui, t("Видалити все", "Delete everything")) {
                st.confirm_all = true;
            }
        });
    });
    if let Some(m) = &st.msg {
        banner_text(ui, Tone::Ok, m);
    }
    text_input(ui, &mut st.query, t("Пошук за назвою і транскриптом", "Search by title and transcript"), ui.available_width());
    ui.add_space(6.0);
    let list = core.db.list_sessions(&st.query, 50);
    if st.open.is_none() {
        st.open = list.first().map(|x| x.id.clone());
    }
    let h = (ui.clip_rect().height() - 150.0).max(480.0);
    ui.horizontal_top(|ui| {
        let left_w = 320.0;
        ui.allocate_ui(egui::vec2(left_w, h), |ui| {
            card(ui, |ui| {
                ui.set_min_height(h - 34.0);
                egui::ScrollArea::vertical().id_salt("hist-list").auto_shrink([false, false]).show(ui, |ui| {
                    for x in &list {
                        let on = st.open.as_deref() == Some(x.id.as_str());
                        let r = egui::Frame::new().fill(if on { p().surface3 } else { egui::Color32::TRANSPARENT }).corner_radius(6).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(faint(fmt_date(x.started_at)));
                                if x.status == "interrupted" {
                                    chip(ui, t("перервана", "interrupted"), false);
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(muted(x.questions.to_string()).size(12.5)));
                            });
                            ui.add(egui::Label::new(small(&x.title)).truncate());
                        });
                        if r.response.interact(egui::Sense::click()).clicked() {
                            st.open = Some(x.id.clone());
                        }
                    }
                    if list.is_empty() {
                        note(ui, t("Нічого не знайдено", "Nothing found"));
                    }
                });
            });
        });
        ui.allocate_ui(egui::vec2(ui.available_width(), h), |ui| {
            card(ui, |ui| {
                ui.set_min_height(h - 34.0);
                let Some(id) = st.open.clone() else {
                    note(ui, t("Виберіть сесію ліворуч.", "Pick a session on the left."));
                    return;
                };
                let d = core.history_detail(&id);
                let Some(sess) = &d.session else {
                    st.open = None;
                    return;
                };
                ui.horizontal(|ui| {
                    h2(ui, &col(sess, "title"));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if danger_sm(ui, t("Видалити", "Delete")) {
                            core.db.delete_session(&id);
                            st.open = None;
                        }
                        for (json, label) in [(true, ".json"), (false, t("Експорт .md", "Export .md"))] {
                            if btn_sm(ui, label) && st.export.is_none() {
                                let (core2, id2) = (core.clone(), id.clone());
                                let ext = if json { "json" } else { "md" };
                                st.export = Some(Promise::thread(&core, move || {
                                    let path = rfd::FileDialog::new().set_file_name(format!("copilot-session-{}.{ext}", &id2[..8.min(id2.len())])).add_filter(ext.to_uppercase(), &[ext]).save_file()?;
                                    Some(core2.export_session(&id2, json, &path))
                                }));
                            }
                        }
                    });
                });
                egui::ScrollArea::vertical().id_salt("hist-detail").auto_shrink([false, false]).show(ui, |ui| {
                    debrief(ui, &core, &id, &col(sess, "summary"), !d.segments.is_empty());
                    for q in crate::export::latest_questions(&d.questions) {
                        h3(ui, &col(q, "text"));
                        for a in d.answers.iter().filter(|a| col(a, "question_id") == col(q, "id") && col(a, "status") == "done") {
                            ui.horizontal(|ui| {
                                chip(ui, crate::export::kind_label(&col(a, "kind")), false);
                                ui.label(faint(col(a, "model")));
                            });
                            markdown(ui, &col(a, "text"), 13.5);
                        }
                        ui.add_space(6.0);
                    }
                    h3(ui, t("Транскрипт", "Transcript"));
                    for seg in &d.segments {
                        let who = if col(seg, "source") == "remote" { t("Співрозмовник", "Other side") } else { t("Я", "Me") };
                        ui.horizontal_top(|ui| {
                            ui.label(RichText::new(format!("{who}:")).strong().size(13.0));
                            ui.add(egui::Label::new(small(col(seg, "text"))).wrap().selectable(true));
                        });
                    }
                });
            });
        });
    });
}

/// The debrief of a saved session: the stored one, or the one being written right now.
fn debrief(ui: &mut egui::Ui, core: &std::sync::Arc<crate::core::Core>, id: &str, stored: &str, has_transcript: bool) {
    let running = core.debriefs.lock().unwrap().get(id).map(|s| s.lock().unwrap().clone());
    let busy = running.as_ref().is_some_and(|g| g.busy);
    let text = running.as_ref().filter(|g| !g.text.is_empty()).map_or(stored, |g| g.text.as_str());
    card_fill(ui, p().surface2, |ui| {
        ui.horizontal(|ui| {
            h3(ui, t("Підсумок", "Debrief"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if busy { t("Пишу…", "Writing…") } else if text.is_empty() { t("Написати підсумок", "Write a debrief") } else { t("Переписати", "Rewrite") };
                if button(ui, label, Kind::Normal, true, !busy && has_transcript).clicked() {
                    core.debrief(id);
                }
            });
        });
        if text.is_empty() {
            note(ui, if has_transcript { t("Коротко про розмову: що було, що вирішили, що підтягнути. Транскрипт іде до вибраної моделі.", "A short account of the conversation: what happened, what was decided, what to work on. The transcript goes to the selected model.") } else { t("Немає транскрипту, з якого писати.", "There is no transcript to write from.") });
        } else {
            markdown(ui, text, 13.5);
        }
        if let Some(e) = running.as_ref().and_then(|g| g.error.as_ref()) {
            ui.label(small(e).color(p().err));
        }
    });
    ui.add_space(8.0);
}
