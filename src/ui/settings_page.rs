use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use eframe::egui::{self, RichText};

use super::providers::{self, ProvidersState};
use super::theme::p;
use super::widgets::*;
use super::{Ctx, Tab};
use crate::core::Core;
use crate::i18n::{fmt_date, t};
use crate::tr;
use crate::types::*;

#[derive(Default)]
pub struct SettingsState {
    pub providers: ProvidersState,
    pub audio: AudioUi,
    recording: Option<ShortcutAction>,
    diag: Option<Promise<Vec<DiagCheck>>>,
    report: Option<Vec<DiagCheck>>,
    privacy_msg: Option<String>,
}

pub fn show(ui: &mut egui::Ui, c: &mut Ctx, st: &mut SettingsState, tab: &mut Tab) {
    h1(ui, t("Налаштування", "Settings"));
    ui.horizontal_wrapped(|ui| {
        for (id, label) in [
            (Tab::Providers, t("Моделі та ключі", "Models & keys")),
            (Tab::Audio, t("Мовлення і звук", "Speech & audio")),
            (Tab::Overlay, t("Підказки", "Hints")),
            (Tab::Shortcuts, t("Гарячі клавіші", "Hotkeys")),
            (Tab::Privacy, t("Приватність", "Privacy")),
            (Tab::Usage, t("Витрати", "Spending")),
            (Tab::Diagnostics, t("Діагностика", "Diagnostics")),
        ] {
            if pill(ui, label, *tab == id, true) {
                *tab = id;
            }
        }
    });
    ui.add_space(8.0);
    match tab {
        Tab::Providers => providers::show(ui, c, &mut st.providers),
        Tab::Audio => audio_tab(ui, c, &mut st.audio),
        Tab::Overlay => overlay_tab(ui, c),
        Tab::Shortcuts => shortcuts_tab(ui, c, st),
        Tab::Privacy => privacy_tab(ui, c, st),
        Tab::Usage => usage_tab(ui, c),
        Tab::Diagnostics => diagnostics_tab(ui, c, st),
    }
}

pub fn ui_lang_options() -> Vec<(Lang, String)> {
    vec![(Lang::Uk, "Українська".into()), (Lang::En, "English".into())]
}

// A fixed language is ~1.5× faster on CPU and never turns speech into a translation.
pub fn asr_lang_options() -> Vec<(LangPref, String)> {
    vec![(LangPref::Uk, "Українська".into()), (LangPref::En, "English".into()), (LangPref::Auto, t("Змішана — автовизначення (повільніше)", "Mixed — auto-detect (slower)").into())]
}

fn overlay_tab(ui: &mut egui::Ui, c: &mut Ctx) {
    let core = c.core.clone();
    let s = c.s.clone();
    let o = s.overlay.clone();
    section(
        ui,
        t("Вікно підказок", "Hints window"),
        |ui| {
            if btn_sm(ui, t("Показати / сховати", "Show / hide")) {
                core.toggle_overlay();
            }
        },
        |ui| {
            ui.columns(2, |cols| {
                field(&mut cols[0], &tr!("Непрозорість фону: {}%", "Background opacity: {}%", (o.opacity * 100.0).round()), |ui| {
                    let mut v = o.opacity;
                    ui.spacing_mut().slider_width = ui.available_width() - 8.0;
                    if ui.add(egui::Slider::new(&mut v, 0.15..=1.0).step_by(0.02).show_value(false)).changed() {
                        core.update_settings(|s| s.overlay.opacity = v);
                    }
                    ui.label(faint(t("Фон вікна підказок просвічує, текст лишається чітким. 100% — непрозорий.", "The hints background shows through, text stays crisp. 100% is opaque.")));
                });
                field(&mut cols[1], &tr!("Розмір шрифту: {}%", "Font size: {}%", (o.font_scale * 100.0).round()), |ui| {
                    let mut v = o.font_scale;
                    ui.spacing_mut().slider_width = ui.available_width() - 8.0;
                    if ui.add(egui::Slider::new(&mut v, 0.8..=1.8).step_by(0.05).show_value(false)).changed() {
                        core.update_settings(|s| s.overlay.font_scale = v);
                    }
                });
                field(&mut cols[0], t("Тема", "Theme"), |ui| {
                    let mut v = o.theme;
                    if combo(ui, "theme", &mut v, &[(Theme::Dark, t("Темна", "Dark").into()), (Theme::Light, t("Світла", "Light").into())], ui.available_width()) {
                        core.update_settings(|s| s.overlay.theme = v);
                    }
                });
                field(&mut cols[1], t("Мова інтерфейсу", "Interface language"), |ui| {
                    let mut v = s.ui_lang;
                    if combo(ui, "ui-lang", &mut v, &ui_lang_options(), ui.available_width()) {
                        core.update_settings(|s| s.ui_lang = v);
                    }
                });
            });
            let mut pinned = o.pinned;
            if toggle(ui, &mut pinned, t("Поверх усіх вікон", "Always on top"), None) {
                core.update_settings(|s| s.overlay.pinned = pinned);
            }
            let mut auto = s.auto_answer;
            if toggle(
                ui,
                &mut auto,
                t("Відповідати автоматично на розпізнане питання", "Answer recognized questions automatically"),
                Some(t(
                    "Вимкнено — відповідь за кнопкою/гарячою клавішею: дешевше й передбачуваніше. Сумнівні репліки завжди показуються як «Можливе питання» без генерації.",
                    "Off — answers on a button/hotkey: cheaper and more predictable. Doubtful lines always show as “Possible question” without generation.",
                )),
            ) {
                core.update_settings(|s| s.auto_answer = auto);
            }
            note(
                ui,
                t(
                    "Вікно перетягується за верхню смугу й змінює розмір за краї. Положення запам'ятовується. Вікно ніколи не забирає фокус клавіатури.",
                    "Drag the window by its top bar and resize it by the edges. The position is remembered. The window never takes keyboard focus.",
                ),
            );
        },
    );
    ui.add_space(10.0);
    section(ui, t("Захист від демонстрації екрана", "Screen-sharing protection"), |_| {}, |ui| {
        let mut protect = o.protect;
        let hint = tr!("Стан: {}", "Status: {}", c.snap.status.protection.label);
        if toggle(ui, &mut protect, t("Приховувати вікно підказок від захоплення екрана", "Hide the hints window from screen capture"), Some(&hint)) {
            core.update_settings(|s| s.overlay.protect = protect);
        }
        banner_text(
            ui,
            Tone::Info,
            t(
                "Це системний прапорець Windows (SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE). Він НЕ гарантує невидимість: різні програми захоплюють екран по-різному, і ми не обходимо засоби контролю. Режим тренування працює повноцінно без нього.",
                "This is a Windows system flag (SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE). It does NOT guarantee invisibility: capture tools differ, and we never bypass monitoring tools. Practice mode works fully without it.",
            ),
        );
        ui.label(RichText::new(t("Як перевірити в себе (2 хвилини):", "How to check it yourself (2 minutes):")).strong());
        for (i, step) in [
            t("Відкрийте підказки (оверлей) поверх вікна.", "Open the hints (overlay) on top of a window."),
            t("Почніть демонстрацію екрана в Zoom/Meet/Teams або OBS.", "Start screen sharing in Zoom/Meet/Teams or OBS."),
            t("Подивіться на результат з другого пристрою/акаунта або у вікні попереднього перегляду OBS.", "Look at the result from a second device/account or OBS preview."),
            t("Оверлей не видно — захист працює для цього способу захоплення. Видно — не покладайтеся на нього.", "Overlay not visible — protection works for this capture method. Visible — do not rely on it."),
        ]
        .iter()
        .enumerate()
        {
            ui.label(small(format!("{}. {step}", i + 1)));
        }
        let mut wp = s.window.protect_from_capture;
        if toggle(
            ui,
            &mut wp,
            t("Приховувати від захоплення й головне вікно, і вибір області", "Also hide the main window and region picker from capture"),
            Some(t(
                "Той самий прапорець Windows. Якщо на дзвінку попросять показати екран, вікна Copilot не потраплять у потік — але ви їх бачите. Не гарантія: залежить від програми захоплення.",
                "The same Windows flag. If asked to share your screen in a call, Copilot windows stay out of the stream — but you see them. Not a guarantee: depends on the capture tool.",
            )),
        ) {
            core.update_settings(|s| s.window.protect_from_capture = wp);
        }
    });
    ui.add_space(10.0);
    section(ui, t("Вікно й трей", "Window & tray"), |_| {}, |ui| {
        let mut tray = s.window.minimize_to_tray;
        if toggle(
            ui,
            &mut tray,
            t("Згортати в трей при закритті та згортанні", "Minimize to tray on close and minimize"),
            Some(t(
                "Вікно ховається в значок біля годинника, застосунок і сесія продовжують працювати. Повний вихід — через меню значка в треї.",
                "The window hides into the icon near the clock; the app and session keep running. Quit fully from the tray icon menu.",
            )),
        ) {
            core.update_settings(|s| s.window.minimize_to_tray = tray);
        }
        let mut pq = s.window.panic_quits;
        if toggle(
            ui,
            &mut pq,
            t("Аварійна зупинка повністю закриває застосунок", "Emergency stop fully quits the app"),
            Some(t(
                "Типово аварійна зупинка лише глушить захоплення й ховає підказки, а застосунок лишається в треї. З цією опцією — застосунок повністю завершується (процесу не лишається).",
                "By default emergency stop only cuts capture and hides hints, and the app stays in the tray. With this option the app exits completely (no process left).",
            )),
        ) {
            core.update_settings(|s| s.window.panic_quits = pq);
        }
    });
}

fn action_label(a: ShortcutAction) -> &'static str {
    match a {
        ShortcutAction::ToggleOverlay => t("Показати / сховати підказки", "Show / hide hints"),
        ShortcutAction::ToggleListen => t("Пауза / продовжити (або почати сесію)", "Pause / resume (or start a session)"),
        ShortcutAction::AnswerNow => t("Відповісти зараз", "Answer now"),
        ShortcutAction::SnapRegion => t("Знімок області екрана", "Screen region snapshot"),
        ShortcutAction::ScrollUp => t("Прокрутити підказки вгору", "Scroll hints up"),
        ShortcutAction::ScrollDown => t("Прокрутити підказки вниз", "Scroll hints down"),
        ShortcutAction::Expand => t("Розгорнути / детальна відповідь", "Expand / deep answer"),
        ShortcutAction::Panic => t("Аварійна зупинка всього", "Emergency stop of everything"),
    }
}

fn key_name(k: egui::Key) -> Option<String> {
    use egui::Key::*;
    Some(match k {
        Space => "Space".into(),
        ArrowUp => "Up".into(),
        ArrowDown => "Down".into(),
        ArrowLeft => "Left".into(),
        ArrowRight => "Right".into(),
        Enter => "Enter".into(),
        Backspace => "Backspace".into(),
        Escape => return None,
        k => {
            let n = k.name();
            if n.len() == 1 || n.starts_with('F') && n[1..].parse::<u8>().is_ok() || matches!(k, Tab | Delete | Home | End | PageUp | PageDown | Insert) {
                n.to_uppercase().replace("PAGEUP", "PageUp").replace("PAGEDOWN", "PageDown")
            } else {
                return None;
            }
        }
    })
}

fn shortcuts_tab(ui: &mut egui::Ui, c: &mut Ctx, st: &mut SettingsState) {
    let core = c.core.clone();
    section(ui, t("Глобальні гарячі клавіші", "Global hotkeys"), |_| {}, |ui| {
        note(
            ui,
            t(
                "Працюють з будь-якої програми (IDE, Zoom, OBS) і не перехоплюють решту введення. Натисніть «Змінити», потім поєднання з Ctrl/Alt/Shift.",
                "Work from any app (IDE, Zoom, OBS) and do not intercept other input. Press “Change”, then a combination with Ctrl/Alt/Shift.",
            ),
        );
        let conflicts = core.shortcut_conflicts.lock().unwrap().clone();
        if conflicts.is_empty() {
            banner_text(ui, Tone::Ok, t("Усі поєднання зареєстровано, конфліктів немає", "All shortcuts registered, no conflicts"));
        } else {
            banner_text(ui, Tone::Warn, &tr!("Конфлікти: {}", "Conflicts: {}", conflicts.join("; ")));
        }
        if let Some(a) = st.recording {
            let captured = ui.input(|i| {
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } => Some((*key, *modifiers)),
                    _ => None,
                })
            });
            if let Some((key, m)) = captured {
                if key == egui::Key::Escape {
                    st.recording = None;
                } else if let Some(name) = key_name(key) {
                    let mut parts = Vec::new();
                    if m.ctrl {
                        parts.push("Control".to_string());
                    }
                    if m.alt {
                        parts.push("Alt".into());
                    }
                    if m.shift {
                        parts.push("Shift".into());
                    }
                    let fkey = name.starts_with('F') && name.len() > 1 && name[1..].parse::<u8>().is_ok();
                    if !parts.is_empty() || fkey {
                        parts.push(name);
                        let accel = parts.join("+");
                        core.update_settings(|s| s.shortcuts.set(a, accel));
                        st.recording = None;
                    }
                }
            }
        }
        for a in ShortcutAction::ALL {
            ui.horizontal(|ui| {
                ui.add_sized([340.0, 22.0], egui::Label::new(action_label(a)).truncate());
                if st.recording == Some(a) {
                    chip(ui, t("Натисніть поєднання… (Esc — скасувати)", "Press a combination… (Esc to cancel)"), true);
                } else {
                    kbd(ui, &pretty_accel(c.s.shortcuts.get(a)));
                }
                if btn_sm(ui, t("Змінити", "Change")) {
                    st.recording = Some(a);
                }
            });
        }
    });
}

fn privacy_tab(ui: &mut egui::Ui, c: &mut Ctx, st: &mut SettingsState) {
    let core = c.core.clone();
    let s = c.s.clone();
    section(ui, t("Дозволи", "Permissions"), |_| {}, |ui| {
        let cs = s.consent.clone();
        type Row = (fn(&mut Consent) -> &mut bool, bool, &'static str, Option<&'static str>);
        let rows: [Row; 5] = [
            (|c| &mut c.mic, cs.mic, t("Захоплення мікрофона", "Microphone capture"), None),
            (|c| &mut c.system, cs.system, t("Захоплення системного звуку (голос співрозмовника)", "System audio capture (the other side's voice)"), None),
            (|c| &mut c.screen, cs.screen, t("Знімки екрана за моєю командою", "Screenshots on my command"), Some(t("Ніколи не робляться автоматично й не надсилаються без підтвердження.", "Never taken automatically and never sent without confirmation."))),
            (
                |c| &mut c.cloud,
                cs.cloud,
                t("Надсилання тексту хмарним провайдерам", "Sending text to cloud providers"),
                Some(t("Потрібно для OpenAI / Anthropic / Gemini / OpenRouter та інших хмарних сервісів. Для llama.cpp не потрібно.", "Needed for OpenAI / Anthropic / Gemini / OpenRouter and other cloud services. Not needed for llama.cpp.")),
            ),
            (|c| &mut c.history, cs.history, t("Зберігати історію сесій на цьому комп'ютері", "Keep session history on this computer"), None),
        ];
        for (get, cur, label, hint) in rows {
            let mut v = cur;
            if toggle(ui, &mut v, label, hint) {
                core.update_settings(|s| {
                    *get(&mut s.consent) = v;
                    s.consent.accepted_at = Some(now_ms());
                });
            }
        }
        note(ui, t("Телеметрії немає: застосунок нічого не надсилає своїм розробникам.", "No telemetry: the app sends nothing to its developers."));
    });
    ui.add_space(10.0);
    section(ui, t("Офлайн-режим", "Offline mode"), |_| {}, |ui| {
        let mut off = s.offline;
        if toggle(
            ui,
            &mut off,
            t("Працювати без інтернету", "Work without internet"),
            Some(t("Блокує всі зовнішні адреси на рівні застосунку. Працюють лише локальні моделі (llama.cpp) і локальне розпізнавання.", "Blocks all external addresses in the app. Only local models (llama.cpp) and local recognition work.")),
        ) {
            core.update_settings(|s| s.offline = off);
        }
    });
    ui.add_space(10.0);
    section(ui, t("Дані", "Data"), |_| {}, |ui| {
        note(
            ui,
            &tr!(
                "Історія, транскрипти й індекси документів лежать у SQLite у {}. Ключі — в окремому файлі, зашифрованому Windows (DPAPI). Сире аудіо не зберігається.",
                "History, transcripts and document indexes live in SQLite in {}. Keys are in a separate file encrypted by Windows (DPAPI). Raw audio is never stored.",
                core.data_dir.display()
            ),
        );
        note(
            ui,
            t(
                "Важливо: те, що застосунок не шле дані своїм серверам, не означає, що дані не йдуть провайдеру моделі. Хмарний провайдер отримує текст питання й вибраний контекст за своєю політикою даних.",
                "Important: the app not sending data to its own servers does not mean nothing goes to the model provider. A cloud provider receives the question text and the selected context under its own data policy.",
            ),
        );
        ui.horizontal(|ui| {
            if button(ui, t("Видалити всю історію", "Delete all history"), Kind::Danger, false, true).clicked() {
                core.db.delete_all_history();
                st.privacy_msg = Some(t("Історію та журнал діагностики видалено.", "History and diagnostics log deleted.").into());
            }
            if let Some(m) = &st.privacy_msg {
                ui.label(small(m));
            }
        });
        note(
            ui,
            t(
                "Відповідальність за допустимість запису й AI-підказок у конкретному інтерв'ю/зустрічі — на користувачеві. Повідомляйте учасників про запис.",
                "Whether recording and AI hints are allowed in a given interview/meeting is the user's responsibility. Tell participants about the recording.",
            ),
        );
    });
}

fn usage_tab(ui: &mut egui::Ui, c: &mut Ctx) {
    let core = c.core.clone();
    let s = c.s.clone();
    let b = s.budget.clone();
    section(ui, t("Ліміти", "Limits"), |_| {}, |ui| {
        ui.columns(2, |cols| {
            for (i, (label, cur, step)) in [
                (t("Максимум $ за сесію (порожньо — без ліміту)", "Max $ per session (empty — no limit)"), b.per_session_usd, 0.1),
                (t("Максимум $ на день", "Max $ per day"), b.per_day_usd, 0.5),
            ]
            .into_iter()
            .enumerate()
            {
                field(&mut cols[i], label, |ui| {
                    ui.horizontal(|ui| {
                        let mut on = cur.is_some();
                        let mut v = cur.unwrap_or(1.0);
                        let ch1 = ui.checkbox(&mut on, "").changed();
                        let ch2 = ui.add_enabled(on, egui::DragValue::new(&mut v).range(0.0..=10_000.0).speed(step).prefix("$")).changed();
                        if ch1 || ch2 {
                            let nv = on.then_some(v);
                            core.update_settings(|s| if i == 0 { s.budget.per_session_usd = nv } else { s.budget.per_day_usd = nv });
                        }
                    });
                });
            }
            field(&mut cols[0], t("Запитів за хвилину", "Requests per minute"), |ui| {
                let mut v = b.requests_per_minute;
                if ui.add(egui::DragValue::new(&mut v).range(1..=600)).changed() {
                    core.update_settings(|s| s.budget.requests_per_minute = v);
                }
            });
            field(&mut cols[1], t("Одночасних запитів", "Concurrent requests"), |ui| {
                let mut v = b.max_concurrent;
                if ui.add(egui::DragValue::new(&mut v).range(1..=6)).changed() {
                    core.update_settings(|s| s.budget.max_concurrent = v);
                }
            });
        });
        note(
            ui,
            t(
                "При досягненні ліміту платні запити зупиняються до вашого підтвердження. Провайдер сам не перемикається. Локальні моделі лімітом $ не обмежуються.",
                "When a limit is reached, paid requests stop until you confirm. The provider is not switched automatically. Local models are not limited by $.",
            ),
        );
    });
    ui.add_space(10.0);
    section(ui, t("Ціни моделей", "Model prices"), |_| {}, |ui| {
        note(
            ui,
            t(
                "Ціни змінюються — впишіть актуальні з сайту провайдера ($ за 1 млн токенів). Без ціни витрати показуються як «Ціна невідома», а не $0.",
                "Prices change — enter current ones from the provider's site ($ per 1M tokens). Without a price spending shows as “Price unknown”, not $0.",
            ),
        );
        let mut seen = std::collections::BTreeSet::new();
        for t2 in s.roles.values().flatten().filter(|t| !t.model.is_empty()) {
            let key = format!("{}/{}", t2.provider_id, t2.model);
            if !seen.insert(key.clone()) {
                continue;
            }
            let cur = s.pricing.get(&key).copied().unwrap_or_default();
            ui.horizontal(|ui| {
                ui.add_sized([280.0, 20.0], egui::Label::new(mono(&t2.model)).truncate());
                let (mut i, mut o) = (cur.input_per_m_tok, cur.output_per_m_tok);
                let a = ui.add(egui::DragValue::new(&mut i).range(0.0..=1000.0).speed(0.01).prefix(t("вхід $", "in $"))).changed();
                let b2 = ui.add(egui::DragValue::new(&mut o).range(0.0..=1000.0).speed(0.01).prefix(t("вихід $", "out $"))).changed();
                if a || b2 {
                    core.update_settings(|s| {
                        s.pricing.insert(key.clone(), ModelPrice { input_per_m_tok: i, output_per_m_tok: o });
                    });
                }
            });
        }
        if seen.is_empty() {
            note(ui, t("Спершу виберіть моделі на вкладці «Моделі та ключі».", "First pick models on the “Models & keys” tab."));
        }
    });
    ui.add_space(10.0);
    section(ui, t("Поточна сесія", "Current session"), |_| {}, |ui| {
        let cost = c.snap.cost;
        let mut l = tr!("Запитів: {} · витрати: {}", "Requests: {} · spent: {}", cost.requests, cost.usd.map(|u| format!("${u:.4}")).unwrap_or("—".into()));
        if cost.unknown_priced > 0 {
            l += &tr!(" · без ціни: {}", " · without price: {}", cost.unknown_priced);
        }
        ui.label(small(l));
    });
}

fn diagnostics_tab(ui: &mut egui::Ui, c: &mut Ctx, st: &mut SettingsState) {
    let core = c.core.clone();
    if let Some(r) = st.diag.as_ref().and_then(|p| p.take()) {
        st.diag = None;
        st.report = Some(r);
    }
    let busy = st.diag.is_some();
    section(
        ui,
        t("Перевірка системи", "System check"),
        |ui| {
            if primary(ui, if busy { t("Перевіряю…", "Checking…") } else { t("Запустити перевірку", "Run check") }, !busy) {
                let c2 = core.clone();
                st.diag = Some(Promise::spawn(&core, async move { c2.diagnostics().await }));
            }
        },
        |ui| {
            match &st.report {
                None => note(ui, t("Перевірить ключі, розпізнавання, з'єднання з моделями, гарячі клавіші, захист вікна, офлайн-режим, OCR і базу.", "Checks keys, recognition, model connections, hotkeys, window protection, offline mode, OCR and the database.")),
                Some(checks) => {
                    for ch in checks {
                        ui.horizontal_top(|ui| {
                            dot(ui, match ch.ok {
                                Some(true) => p().ok,
                                Some(false) => p().err,
                                None => p().warn,
                            });
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 2.0;
                                ui.label(RichText::new(&ch.label).strong());
                                note(ui, &ch.detail);
                                if ch.id == "ocr" && ch.ok != Some(true) && btn_sm(ui, t("Відкрити мовні параметри Windows", "Open Windows language settings")) {
                                    crate::win::open_url("ms-settings:regionlanguage");
                                }
                            });
                        });
                    }
                }
            }
        },
    );
    ui.add_space(10.0);
    section(ui, t("Журнал (без ключів і текстів розмов)", "Log (no keys and no conversation text)"), |_| {}, |ui| {
        egui::ScrollArea::vertical().id_salt("diag-log").max_height(280.0).auto_shrink([false, true]).show(ui, |ui| {
            let log = core.db.recent_diag(100);
            if log.is_empty() {
                note(ui, t("Порожньо", "Empty"));
            }
            for (at, kind, data) in log {
                ui.horizontal_top(|ui| {
                    ui.label(faint(fmt_date(at)));
                    ui.add(egui::Label::new(mono(format!("{kind} {}", data.chars().take(200).collect::<String>())).size(12.0)).wrap());
                });
            }
        });
    });
}

// ---- Speech & audio ----

pub struct AudioUi {
    nvidia: Option<bool>,
    install: Option<Promise<Result<bool, String>>>,
    err: Option<String>,
    devices: Option<Vec<crate::audio::InputDevice>>,
    probe: [Option<Promise<Result<f32, String>>>; 2],
    probe_level: [Arc<AtomicU32>; 2],
    verdict: [Option<String>; 2],
    vocab: Option<String>,
}

impl Default for AudioUi {
    fn default() -> Self {
        Self { nvidia: None, install: None, err: None, devices: None, probe: [None, None], probe_level: [Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0))], verdict: [None, None], vocab: None }
    }
}

pub fn asr_installer(ui: &mut egui::Ui, core: &Arc<Core>, st: &mut AudioUi, compact: bool) {
    let nvidia = *st.nvidia.get_or_insert_with(crate::win::has_nvidia);
    if let Some(r) = st.install.as_ref().and_then(|p| p.take()) {
        st.install = None;
        st.err = r.err();
    }
    let s = core.s();
    let asr = core.asr.status();
    let busy = st.install.is_some();
    let options = [
        (AsrBackend::Cpu, "base", t("Швидкий (процесор)", "Fast (CPU)"), "≈160 MB", t("≈1 с на фразу на CPU. Підходить усім.", "≈1 s per phrase on CPU. Works for everyone.")),
        (AsrBackend::Cpu, "small", t("Точніший (процесор)", "More accurate (CPU)"), "≈500 MB", t("≈2 с на фразу — повільніше, але краще з акцентами.", "≈2 s per phrase — slower, better with accents.")),
        (AsrBackend::Cuda, "turbo", t("Максимум (відеокарта NVIDIA)", "Maximum (NVIDIA GPU)"), "≈1.3 GB", t("≈0.1 с на фразу й найкраща точність. Перший запуск прогрівається до 30 с.", "≈0.1 s per phrase and best accuracy. First run warms up for up to 30 s.")),
    ];
    ui.columns(if compact { 2 } else { 3 }, |cols| {
        let mut i = 0;
        for (backend, model, title, size, note_text) in options {
            if compact && model == "small" {
                continue;
            }
            let ui = &mut cols[i];
            i += 1;
            let active = s.asr.backend == backend && s.asr.model == model;
            let ready = active && asr.binary && asr.model_present;
            let disabled = busy || (backend == AsrBackend::Cuda && !nvidia);
            egui::Frame::new()
                .fill(if active { p().accent.gamma_multiply(0.1) } else { p().surface2 })
                .stroke(egui::Stroke::new(1.0, if active { p().accent } else { p().border }))
                .corner_radius(8)
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(title).strong());
                    note(ui, note_text);
                    let mut sz = tr!("Завантаження {}", "Download {}", size);
                    if backend == AsrBackend::Cuda && !nvidia {
                        sz += t(" · NVIDIA не знайдено", " · NVIDIA not found");
                    }
                    ui.label(faint(sz));
                    let label = if ready { t("Встановлено", "Installed") } else if active { t("Доустановити", "Finish install") } else { t("Встановити й вибрати", "Install and select") };
                    if button(ui, label, if active { Kind::Normal } else { Kind::Primary }, true, !disabled && !ready).clicked() {
                        st.err = None;
                        let c2 = core.clone();
                        st.install = Some(Promise::spawn(core, async move { c2.asr_install(backend, model.into()).await }));
                    }
                });
        }
    });
    if let Some(d) = &asr.download {
        ui.label(small(tr!("Завантаження: {} — {} / {} МБ", "Downloading: {} — {} / {} MB", d.name, d.received / 1_048_576, if d.total > 0 { (d.total / 1_048_576).to_string() } else { "?".into() })));
        progress(ui, if d.total > 0 { d.received as f32 / d.total as f32 } else { 0.05 });
        ui.label(faint(t("Моделі перевіряються за SHA-256 після завантаження.", "Models are verified by SHA-256 after download.")));
    }
    let err = st.err.clone().or(asr.error.clone());
    error_text(ui, &err);
}

fn probe(ui: &mut egui::Ui, core: &Arc<Core>, st: &mut AudioUi, source: Source, mic: Option<String>) {
    let i = source.idx();
    if let Some(r) = st.probe[i].as_ref().and_then(|p| p.take()) {
        st.probe[i] = None;
        st.probe_level[i].store(0, Ordering::Relaxed);
        st.verdict[i] = Some(match r {
            Ok(max) if max > 0.02 => t("Сигнал є — усе чути.", "Signal present — all good.").into(),
            Ok(max) if max > 0.004 => t("Дуже тихо — перевірте гучність.", "Very quiet — check the volume.").into(),
            Ok(_) if source == Source::User => t("Тиша. Перевірте вибраний мікрофон і його гучність у Windows.", "Silence. Check the selected microphone and its volume in Windows.").into(),
            Ok(_) => t("Тиша. Увімкніть звук у дзвінку/відео під час перевірки.", "Silence. Play call/video audio during the check.").into(),
            Err(e) => tr!("Помилка: {}", "Error: {}", e),
        });
    }
    let busy = st.probe[i].is_some();
    ui.horizontal(|ui| {
        let label = if busy {
            t("Слухаю 4 с…", "Listening 4 s…")
        } else if source == Source::User {
            t("Чи чує мене?", "Does it hear me?")
        } else {
            t("Чи чує співрозмовника?", "Does it hear the other side?")
        };
        if button(ui, label, Kind::Normal, true, !busy).clicked() {
            st.verdict[i] = None;
            let lvl = st.probe_level[i].clone();
            let ctx = ui.ctx().clone();
            st.probe[i] = Some(Promise::thread(core, move || {
                crate::audio::probe_level(source, mic.as_deref(), 4000, |v| {
                    lvl.store(v.to_bits(), Ordering::Relaxed);
                    ctx.request_repaint();
                })
            }));
        }
        meter(ui, f32::from_bits(st.probe_level[i].load(Ordering::Relaxed)), ui.available_width().min(220.0));
    });
    if let Some(v) = &st.verdict[i] {
        ui.label(small(v));
    }
}

pub fn device_settings(ui: &mut egui::Ui, core: &Arc<Core>, st: &mut AudioUi) {
    let s = core.s();
    let a = s.audio.clone();
    let devices: Vec<(String, String)> = st.devices.get_or_insert_with(crate::audio::input_devices).iter().map(|d| (d.id.clone(), d.name.clone())).collect();
    let mut opts: Vec<(Option<String>, String)> = vec![(None, t("Системний типовий", "System default").into())];
    opts.extend(devices.iter().map(|d| (Some(d.0.clone()), d.1.clone())));
    ui.columns(2, |cols| {
        let ui = &mut cols[0];
        let mut on = a.capture_mic;
        if toggle(ui, &mut on, t("Мій мікрофон", "My microphone"), Some(t("Ваше мовлення — джерело «я»", "Your speech — the “me” source"))) {
            core.update_settings(|s| s.audio.capture_mic = on);
        }
        let mut cur = a.mic_device_id.clone().filter(|id| devices.iter().any(|d| &d.0 == id));
        if combo(ui, "mic-dev", &mut cur, &opts, ui.available_width()) {
            core.update_settings(|s| s.audio.mic_device_id = cur);
        }
        if ghost_sm(ui, t("Оновити список пристроїв", "Refresh device list")) {
            st.devices = None;
        }
        probe(ui, core, st, Source::User, a.mic_device_id.clone());
        let ui = &mut cols[1];
        let mut on = a.capture_system;
        if toggle(
            ui,
            &mut on,
            t("Звук співрозмовника (системний звук)", "Other side's audio (system audio)"),
            Some(t("WASAPI loopback: весь звук, який грає Windows. Захоплення окремої програми поки не підтримується.", "WASAPI loopback: all audio Windows plays. Capturing a single app is not supported yet.")),
        ) {
            core.update_settings(|s| s.audio.capture_system = on);
        }
        note(ui, t("Порада: навушники прибирають луну співрозмовника в мікрофоні.", "Tip: headphones remove the other side's echo in the microphone."));
        probe(ui, core, st, Source::Remote, None);
    });
}

fn audio_tab(ui: &mut egui::Ui, c: &mut Ctx, st: &mut AudioUi) {
    let core = c.core.clone();
    let s = c.s.clone();
    section(ui, t("Пристрої", "Devices"), |_| {}, |ui| {
        device_settings(ui, &core, st);
        note(ui, t("Сире аудіо на диск не пишеться. Пауза й «Завершити» зупиняють захоплення повністю.", "Raw audio is never written to disk. Pause and “Stop” halt capture completely."));
    });
    ui.add_space(10.0);
    section(ui, t("Розпізнавання мовлення (локально, whisper.cpp)", "Speech recognition (local, whisper.cpp)"), |_| {}, |ui| {
        asr_installer(ui, &core, st, false);
        ui.columns(2, |cols| {
            field(&mut cols[0], t("Мова мовлення", "Speech language"), |ui| {
                let mut v = s.asr.language;
                if combo(ui, "asr-lang", &mut v, &asr_lang_options(), ui.available_width()) {
                    core.update_settings(|s| s.asr.language = v);
                }
            });
            field(&mut cols[1], t("Потоків процесора", "CPU threads"), |ui| {
                let mut v = s.asr.threads;
                if ui.add(egui::DragValue::new(&mut v).range(1..=32)).changed() {
                    core.update_settings(|s| s.asr.threads = v);
                }
            });
        });
        field(ui, t("Словник термінів — допомагає правильно писати назви (через кому)", "Term dictionary — helps spell names correctly (comma-separated)"), |ui| {
            let vocab = st.vocab.get_or_insert_with(|| s.asr.vocabulary.join(", "));
            let r = ui.add(egui::TextEdit::singleline(vocab).desired_width(ui.available_width()).margin(egui::Margin::symmetric(8, 6)));
            if r.lost_focus() {
                let list: Vec<String> = vocab.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
                core.update_settings(|s| s.asr.vocabulary = list);
            }
        });
        if let Some(ms) = core.asr.status().last_latency_ms {
            note(ui, &tr!("Остання затримка розпізнавання: {} мс", "Last recognition latency: {} ms", ms));
        }
    });
}
