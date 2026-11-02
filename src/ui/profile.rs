use eframe::egui;

use super::widgets::*;
use super::Ctx;
use crate::i18n::t;
use crate::tr;
use crate::types::*;

pub fn preset_label(p: ProfilePreset) -> &'static str {
    match p {
        ProfilePreset::Hr => t("HR / поведінкове", "HR / behavioral"),
        ProfilePreset::Backend => "Backend",
        ProfilePreset::Frontend => "Frontend",
        ProfilePreset::Devops => "DevOps / SRE",
        ProfilePreset::SystemDesign => "System Design",
        ProfilePreset::Coding => t("Coding-інтерв'ю", "Coding interview"),
        ProfilePreset::Meeting => t("Робоча зустріч", "Work meeting"),
    }
}

pub fn style_label(s: AnswerStyle) -> &'static str {
    match s {
        AnswerStyle::Short => t("Максимально коротко", "As short as possible"),
        AnswerStyle::Natural => t("Природно", "Natural"),
        AnswerStyle::Academic => t("Академічно", "Academic"),
        AnswerStyle::EnglishB2 => t("Англійською B2", "English B2"),
    }
}

pub fn kind_label(k: DocKind) -> &'static str {
    match k {
        DocKind::Cv => t("Резюме", "CV"),
        DocKind::Jd => t("Вакансія", "Job description"),
        DocKind::Notes => t("Нотатки", "Notes"),
    }
}

#[derive(Default)]
pub struct ProfileState {
    draft: Option<Profile>,
    err: Option<String>,
    paste: Option<(DocKind, String)>,
    saved_at: Option<std::time::Instant>,
    import: Option<Promise<Option<Result<(), String>>>>,
}

/// File dialog + local extraction off the UI thread.
pub fn start_import(core: &std::sync::Arc<crate::core::Core>, profile_id: String, kind: DocKind) -> Promise<Option<Result<(), String>>> {
    let c2 = core.clone();
    Promise::thread(core, move || {
        let path = rfd::FileDialog::new().set_title(t("Виберіть документ", "Choose a document")).add_filter(t("Документи", "Documents"), &["pdf", "docx", "txt", "md"]).pick_file()?;
        Some(c2.import_doc(&profile_id, kind, &path))
    })
}

pub fn show(ui: &mut egui::Ui, c: &mut Ctx, st: &mut ProfileState) {
    let core = c.core.clone();
    let profiles = core.db.list_profiles();
    let active = profiles.iter().find(|p| p.id == c.s.active_profile_id).or(profiles.first()).cloned();
    let Some(active) = active else { return };
    if st.draft.as_ref().is_none_or(|d| d.id != active.id) {
        st.draft = Some(active.clone());
    }
    if let Some(r) = st.import.as_ref().and_then(|p| p.take()) {
        st.import = None;
        st.err = match r {
            Some(Err(e)) => Some(e),
            _ => None,
        };
    }
    let draft = st.draft.as_mut().unwrap();

    ui.horizontal(|ui| {
        h1(ui, t("Профіль і документи", "Profile & documents"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if btn(ui, t("+ Профіль", "+ Profile")) {
                let p = core.upsert_profile(Profile {
                    id: String::new(),
                    name: t("Новий профіль", "New profile").into(),
                    preset: ProfilePreset::Backend,
                    style: AnswerStyle::Natural,
                    notes: String::new(),
                });
                core.update_settings(|s| s.active_profile_id = p.id.clone());
            }
            let mut cur = active.id.clone();
            let opts: Vec<(String, String)> = profiles.iter().map(|p| (p.id.clone(), p.name.clone())).collect();
            if combo(ui, "profile-select", &mut cur, &opts, 220.0) {
                core.update_settings(|s| s.active_profile_id = cur);
                core.session.reload_knowledge();
            }
        });
    });
    ui.add_space(6.0);

    card(ui, |ui| {
        ui.columns(2, |cols| {
            field(&mut cols[0], t("Назва (наприклад, «Компанія — Senior Backend»)", "Name (e.g. “Company — Senior Backend”)"), |ui| {
                text_input(ui, &mut draft.name, "", ui.available_width());
            });
            field(&mut cols[1], t("Тип інтерв'ю", "Interview type"), |ui| {
                let opts: Vec<(ProfilePreset, String)> = ProfilePreset::ALL.iter().map(|p| (*p, preset_label(*p).to_string())).collect();
                combo(ui, "prof-preset", &mut draft.preset, &opts, ui.available_width());
            });
            field(&mut cols[0], t("Стиль подачі", "Delivery style"), |ui| {
                let opts: Vec<(AnswerStyle, String)> = AnswerStyle::ALL.iter().map(|p| (*p, style_label(*p).to_string())).collect();
                combo(ui, "prof-style", &mut draft.style, &opts, ui.available_width());
            });
            field(&mut cols[1], t("Мова відповідей", "Answer language"), |ui| {
                let mut v = c.s.answer_lang;
                if combo(ui, "prof-lang", &mut v, &super::settings_page::answer_lang_options(), ui.available_width()) {
                    core.update_settings(|s| s.answer_lang = v);
                }
            });
        });
        field(ui, t("Нотатки про себе й проєкти (потрапляють у контекст як факти, які ви підтверджуєте)", "Notes about you and your projects (go into context as facts you confirm)"), |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut draft.notes)
                    .desired_rows(5)
                    .desired_width(ui.available_width())
                    .hint_text(t("Наприклад: стек — Python/FastAPI, проєкт X: прискорив звіти втричі…", "E.g.: stack — Python/FastAPI, project X: made reports 3× faster…")),
            );
        });
        ui.horizontal(|ui| {
            if primary(ui, t("Зберегти", "Save"), true) {
                core.upsert_profile(draft.clone());
                st.saved_at = Some(std::time::Instant::now());
            }
            if st.saved_at.is_some_and(|t| t.elapsed().as_secs_f32() < 1.5) {
                ui.label(muted(t("Збережено", "Saved")).size(12.5));
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
            }
            if profiles.len() > 1 {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if danger_sm(ui, t("Видалити профіль", "Delete profile")) {
                        st.err = core.remove_profile(&active.id).err();
                    }
                });
            }
        });
    });
    ui.add_space(10.0);

    card(ui, |ui| {
        ui.horizontal(|ui| {
            h2(ui, t("Документи профілю", "Profile documents"));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ghost_sm(ui, t("Вставити текст", "Paste text")) {
                    st.paste = Some((DocKind::Jd, String::new()));
                }
                for k in [DocKind::Notes, DocKind::Jd, DocKind::Cv] {
                    if btn_sm(ui, &tr!("+ {} (файл)", "+ {} (file)", kind_label(k))) && st.import.is_none() {
                        st.err = None;
                        st.import = Some(start_import(&core, active.id.clone(), k));
                    }
                }
            });
        });
        note(
            ui,
            t(
                "PDF, DOCX, TXT, MD. Текст витягується й індексується на цьому комп'ютері. У відповідь потрапляють лише доречні до питання фрагменти.",
                "PDF, DOCX, TXT, MD. Text is extracted and indexed on this computer. Only fragments relevant to the question go into answers.",
            ),
        );
        if st.import.is_some() {
            ui.label(muted(t("Імпорт…", "Importing…")));
        }
        error_text(ui, &st.err);
        let mut close = false;
        if let Some((kind, text)) = st.paste.as_mut() {
            let opts: Vec<(DocKind, String)> = DocKind::ALL.iter().map(|k| (*k, kind_label(*k).to_string())).collect();
            combo(ui, "paste-kind", kind, &opts, 180.0);
            ui.add(egui::TextEdit::multiline(text).desired_rows(8).desired_width(ui.available_width()).hint_text(t("Вставте текст вакансії або резюме", "Paste the job description or CV text")));
            ui.horizontal(|ui| {
                if primary(ui, t("Додати", "Add"), !text.trim().is_empty()) {
                    core.add_text(&active.id, *kind, &tr!("{} (вставка)", "{} (pasted)", kind_label(*kind)), text);
                    close = true;
                }
                if ghost_sm(ui, t("Скасувати", "Cancel")) {
                    close = true;
                }
            });
        }
        if close {
            st.paste = None;
        }
        let docs = core.db.list_docs(&active.id);
        if docs.is_empty() {
            banner_text(ui, Tone::Info, t("Документів немає. Без резюме відповіді будуть загальними — особистий досвід не вигадуватиметься.", "No documents. Without a CV answers stay general — personal experience is never invented."));
        }
        for d in docs {
            ui.horizontal(|ui| {
                chip(ui, kind_label(d.kind), false);
                ui.label(small(&d.name));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if danger_sm(ui, t("Видалити", "Delete")) {
                        core.remove_doc(&d.id);
                    }
                    ui.label(faint(tr!("{} фрагм.", "{} chunks", d.chunks)));
                });
            });
        }
    });
}
