use std::collections::HashMap;
use std::sync::Arc;

use eframe::egui::{self, RichText};

use super::theme::p;
use super::widgets::*;
use super::Ctx;
use crate::core::Core;
use crate::i18n::t;
use crate::llama::LlamaState;
use crate::net::is_loopback_url;
use crate::providers::presets::{label as preset_label, preset, PRESETS};
use crate::providers::registry::ProviderTestReport;
use crate::tr;
use crate::types::*;

pub fn role_label(r: ModelRole) -> &'static str {
    match r {
        ModelRole::QuickAnswer => t("Швидка відповідь (Say it, тези)", "Quick answer (Say it, points)"),
        ModelRole::DeepAnswer => t("Детальна відповідь", "Deep answer"),
        ModelRole::Vision => t("Знімки екрана (vision)", "Screenshots (vision)"),
        ModelRole::Classification => t("Класифікація", "Classification"),
        ModelRole::PostSessionSummary => t("Підсумок зустрічі", "Meeting summary"),
    }
}

pub enum FormResult {
    None,
    Saved(ProviderConfig),
    Cancelled,
}

pub struct ProviderForm {
    initial: Option<ProviderConfig>,
    preset: String,
    label: String,
    base_url: String,
    key: String,
    allow_fallback: bool,
    timeout_ms: u64,
    llama: LlamaLaunch,
    err: Option<String>,
    warn: Option<String>,
    browse: Option<Promise<Option<(bool, String)>>>,
}

impl ProviderForm {
    pub fn new(initial: Option<ProviderConfig>) -> Self {
        let pr = preset(initial.as_ref().map(|c| c.preset.as_str()).unwrap_or("llamacpp"));
        Self {
            preset: pr.id.into(),
            label: initial.as_ref().map(|c| c.label.clone()).unwrap_or_else(|| preset_label(pr).into()),
            base_url: initial.as_ref().map(|c| c.base_url.clone()).unwrap_or_else(|| pr.base_url.into()),
            key: String::new(),
            allow_fallback: initial.as_ref().is_some_and(|c| c.allow_fallback),
            timeout_ms: initial.as_ref().map(|c| c.timeout_ms).unwrap_or_else(|| pr.default_timeout_ms()),
            llama: initial.as_ref().and_then(|c| c.llama.clone()).unwrap_or(LlamaLaunch { ctx_size: 8192, gpu_layers: 99, auto_start: true, ..Default::default() }),
            initial,
            err: None,
            warn: None,
            browse: None,
        }
    }

    fn pick(&mut self, id: &str) {
        let pr = preset(id);
        self.preset = pr.id.into();
        self.label = preset_label(pr).into();
        self.base_url = pr.base_url.into();
        self.timeout_ms = pr.default_timeout_ms();
    }

    pub fn show(&mut self, ui: &mut egui::Ui, core: &Arc<Core>, cancellable: bool) -> FormResult {
        let mut result = FormResult::None;
        if let Some(Some((is_model, path))) = self.browse.as_ref().and_then(|p| p.take()) {
            if is_model {
                self.llama.model_path = path;
            } else {
                self.llama.server_path = path;
            }
            self.browse = None;
        }
        let editing = self.initial.is_some();
        ui.horizontal_wrapped(|ui| {
            for pr in PRESETS {
                let on = self.preset == pr.id;
                let c = p();
                let txt = format!("{}\n{}", preset_label(pr), if pr.local { t("локально, без ключа", "local, no key") } else if pr.needs_key { t("потрібен ключ", "needs a key") } else { t("ключ за потреби", "key optional") });
                let b = egui::Button::new(RichText::new(txt).size(12.5).color(if on { c.accent } else { c.text }))
                    .fill(if on { c.accent.gamma_multiply(0.15) } else { c.surface2 })
                    .stroke(egui::Stroke::new(1.0, if on { c.accent } else { c.border }))
                    .corner_radius(8)
                    .min_size(egui::vec2(150.0, 46.0));
                if ui.add_enabled(!editing, b).clicked() {
                    self.pick(pr.id);
                }
            }
        });
        let info = preset(&self.preset);
        note(ui, info.note());
        ui.columns(2, |cols| {
            field(&mut cols[0], t("Назва", "Name"), |ui| {
                text_input(ui, &mut self.label, "", ui.available_width());
            });
            field(&mut cols[1], t("Адреса API", "API address"), |ui| {
                ui.add(egui::TextEdit::singleline(&mut self.base_url).font(egui::TextStyle::Monospace).desired_width(ui.available_width()).margin(egui::Margin::symmetric(8, 6)));
            });
            let has_key = self.initial.as_ref().is_some_and(|c| c.has_key);
            let key_label = if has_key {
                t("API-ключ (збережено — введіть новий, щоб замінити)", "API key (saved — enter a new one to replace)")
            } else if info.needs_key {
                t("API-ключ", "API key")
            } else {
                t("API-ключ (не обов'язковий)", "API key (optional)")
            };
            cols[0].vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.horizontal(|ui| {
                    ui.label(muted(key_label).size(12.5));
                    if let Some(url) = info.key_url {
                        if ui.link(small(t("де взяти", "where to get"))).clicked() {
                            crate::win::open_url(url);
                        }
                    }
                });
                ui.add(
                    egui::TextEdit::singleline(&mut self.key)
                        .password(true)
                        .hint_text(if has_key { "••••••••" } else { "" })
                        .font(egui::TextStyle::Monospace)
                        .desired_width(ui.available_width())
                        .margin(egui::Margin::symmetric(8, 6)),
                );
            });
            field(&mut cols[1], t("Чекати перший токен, мс", "Wait for the first token, ms"), |ui| {
                ui.add(egui::DragValue::new(&mut self.timeout_ms).range(2000..=180_000).speed(500));
            });
        });
        if self.preset == "llamacpp" {
            self.llama_section(ui, core);
        }
        toggle(
            ui,
            &mut self.allow_fallback,
            t("Дозволити як запасний", "Allow as a fallback"),
            Some(t(
                "Якщо основний провайдер недоступний (429/5xx/тайм-аут), запит можна надіслати сюди. Без цієї позначки дані сюди не потраплять.",
                "If the main provider fails (429/5xx/timeout), the request may go here. Without this switch no data is sent here.",
            )),
        );
        note(ui, t("Ключ шифрується Windows (DPAPI) і не потрапляє в налаштування, журнали та експорт.", "The key is encrypted by Windows (DPAPI) and never stored in settings, logs or exports."));
        error_text(ui, &self.err);
        if let Some(w) = &self.warn {
            banner_text(ui, Tone::Warn, w);
        }
        ui.horizontal(|ui| {
            let can = !self.base_url.trim().is_empty() && (!info.needs_key || !self.key.trim().is_empty() || self.initial.as_ref().is_some_and(|c| c.has_key));
            if primary(ui, t("Зберегти", "Save"), can) {
                let cfg = ProviderConfig {
                    id: self.initial.as_ref().map(|c| c.id.clone()).unwrap_or_default(),
                    label: self.label.trim().to_string(),
                    preset: self.preset.clone(),
                    protocol: info.protocol,
                    base_url: self.base_url.trim().to_string(),
                    has_key: false,
                    allow_fallback: self.allow_fallback,
                    timeout_ms: self.timeout_ms,
                    llama: (self.preset == "llamacpp" && !self.llama.server_path.trim().is_empty()).then(|| self.llama.clone()),
                };
                match core.upsert_provider(cfg, Some(self.key.trim()).filter(|k| !k.is_empty())) {
                    Ok((saved, warning)) => {
                        self.key.clear();
                        self.warn = warning;
                        self.initial = Some(saved.clone());
                        result = FormResult::Saved(saved);
                    }
                    Err(e) => self.err = Some(e),
                }
            }
            if cancellable && ghost_sm(ui, t("Скасувати", "Cancel")) {
                result = FormResult::Cancelled;
            }
        });
        result
    }

    fn llama_section(&mut self, ui: &mut egui::Ui, core: &Arc<Core>) {
        card_fill(ui, p().surface2, |ui| {
            h3(ui, t("Запуск llama-server із застосунку (необов'язково)", "Start llama-server from the app (optional)"));
            note(
                ui,
                t(
                    "Вкажіть llama-server.exe (з релізу llama.cpp) і модель .gguf — застосунок сам запустить сервер на порту з адреси вище, дочекається завантаження моделі й зупинить його при виході. Якщо сервер уже запущено вами — залиште порожнім.",
                    "Point to llama-server.exe (from a llama.cpp release) and a .gguf model — the app starts the server on the port from the address above, waits for the model to load and stops it on exit. If you run the server yourself, leave this empty.",
                ),
            );
            for (is_model, label, filter) in [(false, "llama-server.exe", "exe"), (true, t("Модель GGUF", "GGUF model"), "gguf")] {
                field(ui, label, |ui| {
                    ui.horizontal(|ui| {
                        let v = if is_model { &mut self.llama.model_path } else { &mut self.llama.server_path };
                        ui.add(egui::TextEdit::singleline(v).font(egui::TextStyle::Monospace).desired_width(ui.available_width() - 90.0).margin(egui::Margin::symmetric(8, 6)));
                        if btn_sm(ui, t("Огляд…", "Browse…")) && self.browse.is_none() {
                            self.browse = Some(Promise::thread(core, move || {
                                rfd::FileDialog::new().add_filter(filter, &[filter]).pick_file().map(|p| (is_model, p.display().to_string()))
                            }));
                        }
                    });
                });
            }
            ui.horizontal(|ui| {
                field(ui, t("Контекст, токенів", "Context, tokens"), |ui| ui.add(egui::DragValue::new(&mut self.llama.ctx_size).range(0..=262_144).speed(256)));
                field(ui, t("Шарів на GPU (-ngl)", "GPU layers (-ngl)"), |ui| ui.add(egui::DragValue::new(&mut self.llama.gpu_layers).range(0..=999)));
            });
            toggle(ui, &mut self.llama.auto_start, t("Запускати автоматично на початку сесії", "Start automatically when a session starts"), None);
            if let Some(cfg) = &self.initial {
                llama_controls(ui, core, &cfg.id);
            }
        });
    }
}

/// Status line + start/stop for a saved llama.cpp provider.
pub fn llama_controls(ui: &mut egui::Ui, core: &Arc<Core>, id: &str) {
    let state = core.llama_state(id);
    ui.horizontal_wrapped(|ui| {
        let (col, text) = match &state {
            LlamaState::Stopped => (p().faint, t("не запущено застосунком", "not started by the app").to_string()),
            LlamaState::Starting => (p().info, t("завантажує модель…", "loading the model…").to_string()),
            LlamaState::Running => (p().ok, t("працює", "running").to_string()),
            LlamaState::Failed(e) => (p().err, e.clone()),
        };
        dot(ui, col);
        ui.add(egui::Label::new(small(text)).wrap());
        match state {
            LlamaState::Running | LlamaState::Starting => {
                if btn_sm(ui, t("Зупинити", "Stop")) {
                    core.llama.stop(id);
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            }
            _ => {
                if btn_sm(ui, t("Запустити", "Start")) {
                    let (c2, id2) = (core.clone(), id.to_string());
                    core.rt.spawn(async move {
                        if let Err(e) = c2.llama_start(&id2).await {
                            c2.session.notice(Some(format!("llama.cpp: {e}")));
                        }
                        c2.emit();
                    });
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(300));
                }
            }
        }
    });
}

#[derive(Default)]
pub struct ModelPicker {
    provider_id: String,
    models: Option<Result<Vec<ModelInfo>, String>>,
    loading: Option<Promise<Result<Vec<ModelInfo>, AppError>>>,
    manual: bool,
}

impl ModelPicker {
    fn load(&mut self, core: &Arc<Core>, provider_id: &str, refresh: bool) {
        self.provider_id = provider_id.into();
        let (c2, id) = (core.clone(), provider_id.to_string());
        self.loading = Some(Promise::spawn(core, async move {
            if let Some(cfg) = c2.registry.get(&id) {
                let _ = c2.llama.ensure(&cfg, &c2.net).await;
            }
            c2.registry.models(&id, refresh).await
        }));
    }

    /// Returns true when `value` changed.
    pub fn show(&mut self, ui: &mut egui::Ui, core: &Arc<Core>, provider_id: &str, value: &mut String) -> bool {
        if self.provider_id != provider_id {
            self.models = None;
            self.load(core, provider_id, false);
        }
        if let Some(r) = self.loading.as_ref().and_then(|p| p.take()) {
            self.loading = None;
            self.models = Some(r.map_err(|e| e.line()));
        }
        let mut changed = false;
        ui.horizontal(|ui| {
            let list: Vec<ModelInfo> = match &self.models {
                Some(Ok(m)) => m.clone(),
                _ => Vec::new(),
            };
            let w = ui.available_width() - 120.0;
            if self.manual || matches!(&self.models, Some(Ok(m)) if m.is_empty()) || matches!(self.models, Some(Err(_))) {
                let r = ui.add(egui::TextEdit::singleline(value).hint_text(t("ID моделі", "Model ID")).font(egui::TextStyle::Monospace).desired_width(w).margin(egui::Margin::symmetric(8, 6)));
                changed |= r.changed();
            } else {
                let sel = if list.iter().any(|m| &m.id == value) {
                    value.clone()
                } else if self.loading.is_some() {
                    t("завантаження…", "loading…").into()
                } else {
                    t("— виберіть модель —", "— pick a model —").into()
                };
                egui::ComboBox::from_id_salt(("model", provider_id, ui.scope_id())).selected_text(sel).width(w).show_ui(ui, |ui| {
                    for m in &list {
                        let mut label = match &m.label {
                            Some(l) if l != &m.id => format!("{l} ({})", m.id),
                            _ => m.id.clone(),
                        };
                        if m.vision == Some(true) {
                            label += " · vision";
                        }
                        if ui.selectable_label(&m.id == value, label).clicked() && &m.id != value {
                            *value = m.id.clone();
                            changed = true;
                        }
                    }
                });
            }
            if button(ui, "↻", Kind::Normal, true, true).on_hover_text(t("Оновити список з API", "Refresh the list from the API")).clicked() {
                self.load(core, provider_id, true);
            }
            if ghost_sm(ui, if self.manual { t("список", "list") } else { t("вручну", "manual") }) {
                self.manual = !self.manual;
            }
        });
        if let Some(Err(e)) = &self.models {
            ui.label(RichText::new(e).size(12.5).color(p().err));
        }
        changed
    }
}

pub fn test_result(ui: &mut egui::Ui, r: &ProviderTestReport) {
    ui.spacing_mut().item_spacing.y = 3.0;
    ui.horizontal_wrapped(|ui| {
        dot(ui, if r.models.is_ok() { p().ok } else { p().err });
        ui.label(small(match &r.models {
            Ok((ms, n)) => tr!("З'єднання: ок, {} моделей, {} мс", "Connection: ok, {} models, {} ms", n, ms),
            Err(e) => tr!("З'єднання: {}", "Connection: {}", e.line()),
        }));
    });
    if let Some(tx) = &r.text {
        ui.horizontal_wrapped(|ui| {
            dot(ui, if tx.ok { p().ok } else { p().err });
            ui.label(small(if tx.ok {
                tr!("Текст: ок, перший токен {} мс («{}»)", "Text: ok, first token {} ms (\"{}\")", tx.ms.unwrap_or(0), tx.sample)
            } else {
                tr!("Текст: {}", "Text: {}", tx.error.clone().unwrap_or_default())
            }));
        });
    }
    if let Some(v) = &r.vision {
        ui.horizontal_wrapped(|ui| {
            dot(ui, if v.ok { p().ok } else { p().warn });
            ui.label(small(if v.ok {
                t("Зображення: ок, модель бачить картинки", "Image: ok, the model sees pictures").to_string()
            } else {
                tr!("Зображення: ні — {}. Для знімків буде локальний OCR.", "Image: no — {}. Screenshots will use local OCR.", v.error.clone().unwrap_or_default())
            }));
        });
    }
}

#[derive(Default)]
pub struct RoleUi {
    picker: ModelPicker,
    test: Option<Promise<ProviderTestReport>>,
    report: Option<ProviderTestReport>,
}

pub fn start_test(core: &Arc<Core>, provider_id: &str, model: &str) -> Promise<ProviderTestReport> {
    let (c2, id, m) = (core.clone(), provider_id.to_string(), model.to_string());
    Promise::spawn(core, async move { c2.test_provider(&id, Some(m)).await })
}

fn role_row(ui: &mut egui::Ui, c: &Ctx, role: ModelRole, providers: &[ProviderConfig], st: &mut RoleUi) {
    let core = c.core;
    let cur = c.s.role(role).cloned().or_else(|| c.s.roles.get(&role).cloned().flatten());
    if let Some(r) = st.test.as_ref().and_then(|p| p.take()) {
        st.test = None;
        st.report = Some(r);
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new(role_label(role)).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if let Some(cur) = cur.as_ref().filter(|c| !c.model.is_empty()) {
                let busy = st.test.is_some();
                if button(ui, if busy { t("Перевіряю…", "Checking…") } else { t("Перевірити з'єднання", "Test connection") }, Kind::Normal, true, !busy).clicked() {
                    st.report = None;
                    st.test = Some(start_test(core, &cur.provider_id, &cur.model));
                }
            }
        });
    });
    ui.horizontal(|ui| {
        let mut pid = cur.as_ref().map(|c| c.provider_id.clone()).unwrap_or_default();
        let mut opts: Vec<(String, String)> = vec![(String::new(), t("— не використовувати —", "— not used —").into())];
        opts.extend(providers.iter().map(|p| (p.id.clone(), p.label.clone())));
        if combo(ui, ("role-prov", role), &mut pid, &opts, 220.0) {
            core.update_settings(|s| {
                s.roles.insert(role, (!pid.is_empty()).then(|| RoleTarget { provider_id: pid.clone(), model: String::new() }));
            });
        }
        if !pid.is_empty() {
            let mut model = cur.as_ref().map(|c| c.model.clone()).unwrap_or_default();
            if st.picker.show(ui, core, &pid, &mut model) {
                core.update_settings(|s| {
                    s.roles.insert(role, Some(RoleTarget { provider_id: pid.clone(), model: model.clone() }));
                });
            }
        }
    });
    if let Some(r) = &st.report {
        test_result(ui, r);
    }
}

#[derive(Default)]
pub struct ProvidersState {
    editing: Option<ProviderForm>,
    roles: HashMap<ModelRole, RoleUi>,
    keys: HashMap<String, String>,
    key_err: Option<String>,
}

pub fn show(ui: &mut egui::Ui, c: &mut Ctx, st: &mut ProvidersState) {
    let core = c.core.clone();
    let providers = core.registry.list();

    let add = std::cell::Cell::new(false);
    section(
        ui,
        t("Підключені сервіси", "Connected services"),
        |ui| {
            if btn(ui, t("+ Додати сервіс", "+ Add service")) {
                add.set(true);
            }
        },
        |ui| {
            if let Some(form) = st.editing.as_mut() {
                let r = card_fill(ui, p().surface2, |ui| form.show(ui, &core, true));
                match r {
                    FormResult::Saved(_) | FormResult::Cancelled => st.editing = None,
                    FormResult::None => {}
                }
            }
            for pr in &providers {
                ui.horizontal(|ui| {
                    let local = is_loopback_url(&pr.base_url);
                    chip(ui, if local { t("локально", "local") } else { t("хмара", "cloud") }, !local);
                    ui.label(RichText::new(&pr.label).strong());
                    ui.add(egui::Label::new(faint(&pr.base_url).monospace()).truncate());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if danger_sm(ui, t("Видалити", "Delete")) {
                            core.remove_provider(&pr.id);
                        }
                        if btn_sm(ui, t("Змінити", "Edit")) {
                            st.editing = Some(ProviderForm::new(Some(pr.clone())));
                        }
                        if pr.allow_fallback {
                            chip(ui, t("запасний", "fallback"), false);
                        }
                        if pr.has_key {
                            chip(ui, t("ключ у DPAPI", "key in DPAPI"), false);
                        }
                    });
                });
                if pr.preset == "llamacpp" && pr.llama.is_some() {
                    ui.indent(("llama", &pr.id), |ui| llama_controls(ui, &core, &pr.id));
                }
            }
            if providers.is_empty() && st.editing.is_none() {
                note(ui, t("Поки немає. Для роботи без інтернету виберіть llama.cpp.", "None yet. To work without internet pick llama.cpp."));
            }
        },
    );
    if add.get() {
        st.editing = Some(ProviderForm::new(None));
    }
    ui.add_space(10.0);

    // One place for every key, whichever service it belongs to.
    let keyed: Vec<&ProviderConfig> = providers.iter().filter(|p| preset(&p.preset).needs_key || p.has_key || p.preset == "custom").collect();
    if !keyed.is_empty() {
        section(ui, t("Ключі API", "API keys"), |_| {}, |ui| {
            note(ui, t("Ключі всіх сервісів в одному місці: додайте, замініть або видаліть. Зберігаються лише зашифрованими (DPAPI).", "Keys for every service in one place: add, replace or delete. Stored encrypted only (DPAPI)."));
            error_text(ui, &st.key_err);
            for pr in keyed {
                ui.horizontal(|ui| {
                    dot(ui, if pr.has_key { p().ok } else if preset(&pr.preset).needs_key { p().warn } else { p().faint });
                    ui.add_sized([180.0, 20.0], egui::Label::new(RichText::new(&pr.label).strong()).truncate());
                    let v = st.keys.entry(pr.id.clone()).or_default();
                    ui.add(
                        egui::TextEdit::singleline(v)
                            .password(true)
                            .hint_text(if pr.has_key { t("збережено — введіть новий для заміни", "saved — type a new one to replace") } else { t("вставте ключ", "paste the key") })
                            .desired_width(ui.available_width() - 210.0)
                            .margin(egui::Margin::symmetric(8, 5)),
                    );
                    let val = v.trim().to_string();
                    if button(ui, t("Зберегти", "Save"), Kind::Primary, true, !val.is_empty()).clicked() {
                        st.key_err = core.upsert_provider(pr.clone(), Some(&val)).err();
                        st.keys.remove(&pr.id);
                        core.update_llm_status();
                    }
                    if pr.has_key && danger_sm(ui, t("Видалити", "Delete")) {
                        core.registry.remove_key(&pr.id);
                        core.update_llm_status();
                    }
                    if let Some(url) = preset(&pr.preset).key_url {
                        if ui.link(small(t("де взяти", "where to get"))).clicked() {
                            crate::win::open_url(url);
                        }
                    }
                });
            }
        });
        ui.add_space(10.0);
    }

    if !providers.is_empty() {
        section(ui, t("Яка модель за що відповідає", "Which model does what"), |_| {}, |ui| {
            note(ui, t("Список моделей береться з API сервісу. Нову модель можна вписати вручну.", "The model list comes from the service API. A new model can be typed manually."));
            for role in [ModelRole::QuickAnswer, ModelRole::DeepAnswer, ModelRole::Vision] {
                let rs = st.roles.entry(role).or_default();
                role_row(ui, c, role, &providers, rs);
                ui.add_space(6.0);
            }
        });
    }
}
