mod history;
mod home;
mod live;
mod md;
mod onboarding;
mod overlay;
mod picker;
mod profile;
mod providers;
mod settings_page;
mod snapshot;
mod theme;
mod widgets;

use std::str::FromStr;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, RichText};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::core::Core;
use crate::i18n::t;
use crate::types::*;
use crate::win;
use widgets::*;

pub fn overlay_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("copilot-overlay")
}

pub fn picker_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of("copilot-picker")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Home,
    Live,
    History,
    Profile,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Providers,
    Audio,
    Overlay,
    Shortcuts,
    Privacy,
    Usage,
    Diagnostics,
}

/// Navigation request raised by a page.
pub type Nav = Option<(Page, Option<Tab>)>;

/// What a page needs each frame.
pub struct Ctx<'a> {
    pub core: &'a Arc<Core>,
    pub s: Settings,
    pub snap: AppSnapshot,
    pub nav: Nav,
}

enum AppEvent {
    Hotkey(u32),
    Menu(String),
    TrayClick,
}

struct TrayItems {
    open: MenuItem,
    overlay: MenuItem,
    listen: MenuItem,
    stop: MenuItem,
    panic: MenuItem,
    quit: MenuItem,
}

pub fn run(core: Arc<Core>) -> eframe::Result<()> {
    let icon = egui::IconData { rgba: crate::icon::app_icon(64), width: 64, height: 64 };
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_visible(std::env::var_os("COPILOT_SNAPSHOT").is_none() && !std::env::args().any(|a| a == win::TRAY_ARG))
            .with_title(win::TITLE_MAIN)
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([880.0, 600.0])
            .with_icon(Arc::new(icon)),
        ..Default::default()
    };
    eframe::run_native(win::TITLE_MAIN, opts, Box::new(move |cc| Ok(Box::new(App::new(cc, core)))))
}

pub struct App {
    core: Arc<Core>,
    page: Page,
    tab: Tab,
    events: Receiver<AppEvent>,
    tray: Option<TrayIcon>,
    tray_items: TrayItems,
    tray_key: (bool, bool, Lang),
    hotkeys: Option<GlobalHotKeyManager>,
    registered: Vec<(HotKey, ShortcutAction)>,
    overlay_ui: Arc<Mutex<overlay::OverlayUi>>,
    picker_ui: Arc<Mutex<picker::PickerUi>>,
    overlay_ready: bool,
    picker_ready: bool,
    main_ready: bool,
    last_session: Option<String>,
    home: home::HomeState,
    live: live::LiveState,
    history: history::HistoryState,
    profile: profile::ProfileState,
    settings: settings_page::SettingsState,
    onboarding: onboarding::OnboardingState,
    snapshot: Option<snapshot::Snapshot>,
    fonts_for: Option<(Lang, LangPref, LangPref)>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, core: Arc<Core>) -> Self {
        let ctx = cc.egui_ctx.clone();
        let _ = core.ui.ctx.set(ctx.clone());
        theme::apply(&ctx, core.s().overlay.theme);

        let (tx, events) = channel::<AppEvent>();
        {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
                if e.state == HotKeyState::Pressed {
                    let _ = tx.send(AppEvent::Hotkey(e.id));
                    ctx.request_repaint();
                }
            }));
        }
        {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
                let _ = tx.send(AppEvent::Menu(e.id.0.clone()));
                ctx.request_repaint();
            }));
        }
        {
            let ctx = ctx.clone();
            TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
                if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                    let _ = tx.send(AppEvent::TrayClick);
                    ctx.request_repaint();
                }
            }));
        }

        let tray_items = TrayItems {
            open: MenuItem::with_id("open", "", true, None),
            overlay: MenuItem::with_id("overlay", "", true, None),
            listen: MenuItem::with_id("listen", "", true, None),
            stop: MenuItem::with_id("stop", "", true, None),
            panic: MenuItem::with_id("panic", "", true, None),
            quit: MenuItem::with_id("quit", "", true, None),
        };
        let menu = Menu::new();
        let _ = menu.append_items(&[
            &tray_items.open,
            &tray_items.overlay,
            &tray_items.listen,
            &tray_items.stop,
            &PredefinedMenuItem::separator(),
            &tray_items.panic,
            &tray_items.quit,
        ]);
        let tray = tray_icon::Icon::from_rgba(crate::icon::app_icon(32), 32, 32)
            .ok()
            .and_then(|icon| TrayIconBuilder::new().with_icon(icon).with_tooltip("Copilot").with_menu(Box::new(menu)).with_menu_on_left_click(false).build().ok());

        let mut app = Self {
            core: core.clone(),
            page: Page::Home,
            tab: Tab::Providers,
            events,
            tray,
            tray_items,
            tray_key: (true, true, Lang::EN),
            hotkeys: GlobalHotKeyManager::new().ok(),
            registered: Vec::new(),
            overlay_ui: Arc::new(Mutex::new(overlay::OverlayUi::default())),
            picker_ui: Arc::new(Mutex::new(picker::PickerUi::default())),
            overlay_ready: false,
            picker_ready: false,
            main_ready: false,
            last_session: None,
            home: Default::default(),
            live: Default::default(),
            history: Default::default(),
            profile: Default::default(),
            settings: Default::default(),
            onboarding: Default::default(),
            snapshot: snapshot::Snapshot::from_env(),
            fonts_for: None,
        };
        app.refresh_tray(true);
        app
    }

    /// OS hotkeys (RegisterHotKey), not a keyboard hook, so nothing can leak if the app crashes.
    fn register_hotkeys(&mut self) {
        let Some(m) = &self.hotkeys else { return };
        for (hk, _) in self.registered.drain(..) {
            let _ = m.unregister(hk);
        }
        let s = self.core.s();
        let mut conflicts = Vec::new();
        for a in ShortcutAction::ALL {
            let accel = s.shortcuts.get(a);
            if accel.is_empty() {
                continue;
            }
            match HotKey::from_str(accel) {
                Ok(hk) => match m.register(hk) {
                    Ok(()) => self.registered.push((hk, a)),
                    Err(_) => conflicts.push(crate::tr!("{} ({}) зайнято іншою програмою", "{} ({}) is taken by another app", pretty_accel(accel), settings_page::action_label(a))),
                },
                Err(_) => conflicts.push(crate::tr!("{} ({}) — некоректне поєднання", "{} ({}) — invalid shortcut", pretty_accel(accel), settings_page::action_label(a))),
            }
        }
        if !conflicts.is_empty() {
            crate::db::diag("shortcut_conflicts", serde_json::json!(conflicts));
        }
        *self.core.shortcut_conflicts.lock().unwrap() = conflicts;
    }

    fn refresh_tray(&mut self, force: bool) {
        let active = self.core.session.active();
        let listening = self.core.session.listening();
        let key = (active, listening, crate::i18n::lang());
        if !force && key == self.tray_key {
            return;
        }
        self.tray_key = key;
        let it = &self.tray_items;
        it.open.set_text(t("Відкрити Copilot", "Open Copilot"));
        it.overlay.set_text(t("Показати/сховати підказки", "Show/hide hints"));
        it.listen.set_text(if !active {
            t("Почати сесію", "Start session")
        } else if listening {
            t("Пауза", "Pause")
        } else {
            t("Продовжити", "Resume")
        });
        it.stop.set_text(t("Завершити сесію", "Stop session"));
        it.stop.set_enabled(active);
        it.panic.set_text(t("Аварійна зупинка", "Emergency stop"));
        it.quit.set_text(t("Вихід", "Quit"));
        if let Some(tray) = &self.tray {
            let tip = if !active {
                "Copilot".to_string()
            } else if listening {
                format!("Copilot — {}", t("слухає", "listening"))
            } else {
                format!("Copilot — {}", t("пауза", "paused"))
            };
            let _ = tray.set_tooltip(Some(tip));
        }
    }

    fn show_main(&self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        win::MAIN.show_focused();
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                AppEvent::Hotkey(id) => {
                    if let Some((_, a)) = self.registered.iter().find(|(hk, _)| hk.id() == id) {
                        self.core.on_shortcut(*a);
                    }
                }
                AppEvent::TrayClick => self.show_main(ctx),
                AppEvent::Menu(id) => match id.as_str() {
                    "open" => self.show_main(ctx),
                    "overlay" => self.core.toggle_overlay(),
                    "listen" => self.core.toggle_listen(),
                    "stop" => self.core.stop_session(),
                    "panic" => self.core.panic(),
                    "quit" => self.core.shutdown(),
                    _ => {}
                },
            }
        }
    }

    /// Reloads fonts when a language with another script is picked: the CJK ones are too big to always keep in memory.
    fn sync_fonts(&mut self, ctx: &egui::Context) {
        let s = self.core.s();
        let key = (s.ui_lang, s.asr.language, s.answer_lang);
        if self.fonts_for != Some(key) {
            self.fonts_for = Some(key);
            theme::install_fonts(ctx, &[s.ui_lang.code(), s.asr.language.code(), s.answer_lang.code()]);
        }
    }

    fn keep_viewports(&self, ctx: &egui::Context) {
        let (core, state) = (self.core.clone(), self.overlay_ui.clone());
        ctx.show_viewport_deferred(
            overlay_id(),
            egui::ViewportBuilder::default()
                .with_title(win::TITLE_OVERLAY)
                .with_inner_size([460.0, 220.0])
                .with_min_inner_size([300.0, 120.0])
                .with_decorations(false)
                .with_transparent(true)
                .with_resizable(true)
                .with_taskbar(false)
                .with_active(false)
                .with_visible(false)
                .with_window_level(egui::WindowLevel::AlwaysOnTop),
            move |ui, _| overlay::show(ui, &core, &state),
        );
        let (core, state) = (self.core.clone(), self.picker_ui.clone());
        ctx.show_viewport_deferred(
            picker_id(),
            egui::ViewportBuilder::default()
                .with_title(win::TITLE_PICKER)
                .with_inner_size([400.0, 300.0])
                .with_decorations(false)
                .with_resizable(false)
                .with_taskbar(false)
                .with_visible(false)
                .with_window_level(egui::WindowLevel::AlwaysOnTop),
            move |ui, _| picker::show(ui, &core, &state),
        );
    }

    fn nav(&self, ui: &mut egui::Ui, snap: &AppSnapshot, s: &Settings) -> Nav {
        let mut nav: Nav = None;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            brand(ui);
        });
        ui.add_space(14.0);
        let items = [
            (Page::Home, t("Головна", "Home")),
            (Page::Live, t("Сесія", "Session")),
            (Page::History, t("Історія", "History")),
            (Page::Profile, t("Профіль і документи", "Profile & documents")),
            (Page::Settings, t("Налаштування", "Settings")),
        ];
        for (p, label) in items {
            let on = self.page == p;
            let c = theme::p();
            let resp = ui.add_sized(
                [ui.available_width(), 34.0],
                egui::Button::new(RichText::new(label).color(if on { c.text } else { c.muted }))
                    .fill(if on { c.surface3 } else { egui::Color32::TRANSPARENT })
                    .stroke(egui::Stroke::NONE)
                    .corner_radius(6),
            );
            if p == Page::Live && snap.session_id.is_some() {
                let r = resp.rect;
                ui.painter().circle_filled(egui::pos2(r.right() - 14.0, r.center().y), 4.0, if snap.listening { c.ok } else { c.warn });
            }
            if resp.clicked() {
                nav = Some((p, None));
            }
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.label(faint(format!("v{} · {}", env!("CARGO_PKG_VERSION"), if s.offline { t("офлайн", "offline") } else { "BYOK" })));
            if snap.session_id.is_some() && button(ui, t("Аварійна зупинка", "Emergency stop"), Kind::Danger, true, true).on_hover_text(pretty_accel(&s.shortcuts.panic)).clicked() {
                self.core.panic();
            }
        });
        nav
    }
}

pub fn brand(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::hover());
    let tex_id = egui::Id::unique("brand-icon");
    let tex = ui.ctx().data_mut(|d| d.get_temp::<egui::TextureHandle>(tex_id));
    let tex = tex.unwrap_or_else(|| {
        let img = egui::ColorImage::from_rgba_unmultiplied([64, 64], &crate::icon::app_icon(64));
        let h = ui.ctx().load_texture("brand", img, egui::TextureOptions::LINEAR);
        ui.ctx().data_mut(|d| d.insert_temp(tex_id, h.clone()));
        h
    });
    ui.painter().image(tex.id(), rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
    ui.label(RichText::new("Copilot").font(theme::semibold(17.0)));
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(sn) = self.snapshot.as_mut() {
            match sn.step() {
                Some(snapshot::Step::Onboarding(n)) => self.onboarding.set_step(n),
                Some(snapshot::Step::Page(p, tab)) => {
                    self.page = p;
                    if let Some(t) = tab {
                        self.tab = t;
                    }
                }
                _ => {}
            }
            sn.tick(ctx, &self.core);
        }
        self.handle_events(ctx);
        self.sync_fonts(ctx);
        if self.core.ui.shortcuts_dirty.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.register_hotkeys();
        }
        self.refresh_tray(false);
        if !self.overlay_ready && win::OVERLAY.get().is_some() {
            self.overlay_ready = true;
            self.core.overlay_ready();
        }
        if !self.picker_ready && win::PICKER.get().is_some() {
            self.picker_ready = true;
            self.core.picker_ready();
        }
        if !self.main_ready && win::MAIN.get().is_some() {
            self.main_ready = true;
            self.core.apply_window_protection();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let s = self.core.s();
        theme::apply(&ctx, s.overlay.theme);
        self.keep_viewports(&ctx);

        // Close/minimize go to the tray unless the user is quitting; the session (and its capture) keeps running.
        let (close, minimized) = ctx.input(|i| (i.viewport().close_requested(), i.viewport().minimized == Some(true)));
        if close {
            if s.window.minimize_to_tray || self.core.session.active() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                self.core.shutdown();
            }
        }
        if minimized && s.window.minimize_to_tray {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }

        let snap = self.core.session.snapshot();
        // A session started from the tray/hotkey brings the live page forward, like the old renderer did.
        if snap.session_id.is_some() && snap.session_id != self.last_session && self.snapshot.is_none() {
            self.page = Page::Live;
        }
        self.last_session = snap.session_id.clone();

        let mut c = Ctx { core: &self.core, s: s.clone(), snap, nav: None };
        let bg = theme::p().bg;
        if !s.onboarded {
            egui::CentralPanel::no_frame().show(ui, |ui| {
                ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
                onboarding::show(ui, &mut c, &mut self.onboarding);
            });
            return;
        }
        let side = egui::Frame::new().fill(theme::p().surface).inner_margin(egui::Margin::same(14)).stroke(egui::Stroke::new(1.0, theme::p().border));
        let nav = egui::Panel::left("nav").exact_size(220.0).resizable(false).frame(side).show(ui, |ui| self.nav(ui, &c.snap, &s)).inner;
        egui::CentralPanel::no_frame().show(ui, |ui| {
            ui.painter().rect_filled(ui.max_rect(), 0.0, bg);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                egui::Frame::new().inner_margin(egui::Margin::same(22)).show(ui, |ui| match self.page {
                    Page::Home => home::show(ui, &mut c, &mut self.home),
                    Page::Live => live::show(ui, &mut c, &mut self.live),
                    Page::History => history::show(ui, &mut c, &mut self.history),
                    Page::Profile => profile::show(ui, &mut c, &mut self.profile),
                    Page::Settings => settings_page::show(ui, &mut c, &mut self.settings, &mut self.tab),
                });
            });
        });
        if let Some((p, tab)) = nav.or(c.nav) {
            self.page = p;
            if let Some(tab) = tab {
                self.tab = tab;
            }
        }
    }
}
