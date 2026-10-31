use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, CornerRadius, Margin, RichText, Sense, Stroke, Ui, Vec2};

use super::theme::{p, semibold};
use crate::types::{ComponentState, ComponentStatus};

pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(p().surface)
        .stroke(Stroke::new(1.0, p().border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), add).inner
        })
        .inner
}

pub fn card_fill<R>(ui: &mut Ui, fill: Color32, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(fill)
        .stroke(Stroke::new(1.0, p().border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), add).inner
        })
        .inner
}

/// Titled card: heading on the left, optional controls on the right.
pub fn section<R>(ui: &mut Ui, title: &str, aside: impl FnOnce(&mut Ui), add: impl FnOnce(&mut Ui) -> R) -> R {
    card(ui, |ui| {
        ui.horizontal(|ui| {
            h2(ui, title);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), aside);
        });
        ui.add_space(4.0);
        add(ui)
    })
}

pub fn h1(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(semibold(24.0)));
}

pub fn h2(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(semibold(17.0)));
}

pub fn h3(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).font(semibold(15.0)));
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).color(p().muted)
}

pub fn faint(text: impl Into<String>) -> RichText {
    RichText::new(text).color(p().faint).size(12.5)
}

pub fn small(text: impl Into<String>) -> RichText {
    RichText::new(text).size(12.5)
}

pub fn mono(text: impl Into<String>) -> RichText {
    RichText::new(text).monospace()
}

/// Wrapped paragraph in the muted color.
pub fn note(ui: &mut Ui, text: &str) {
    ui.add(egui::Label::new(muted(text).size(12.5)).wrap());
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Normal,
    Primary,
    Danger,
    Ghost,
}

pub fn button(ui: &mut Ui, text: &str, kind: Kind, small: bool, enabled: bool) -> egui::Response {
    let c = p();
    let (fill, fg, stroke) = match kind {
        Kind::Normal => (c.surface3, c.text, Stroke::new(1.0, c.border)),
        Kind::Primary => (c.accent, c.accent_ink, Stroke::NONE),
        Kind::Danger => (c.err.gamma_multiply(0.18), c.err, Stroke::new(1.0, c.err.gamma_multiply(0.5))),
        Kind::Ghost => (Color32::TRANSPARENT, c.muted, Stroke::NONE),
    };
    let mut rt = RichText::new(text).color(fg);
    if small {
        rt = rt.size(12.5);
    }
    if kind == Kind::Primary {
        rt = rt.font(semibold(if small { 12.5 } else { 14.0 }));
    }
    let b = egui::Button::new(rt).fill(fill).stroke(stroke).corner_radius(CornerRadius::same(6)).min_size(Vec2::new(0.0, if small { 26.0 } else { 32.0 }));
    ui.add_enabled(enabled, b)
}

pub fn btn(ui: &mut Ui, text: &str) -> bool {
    button(ui, text, Kind::Normal, false, true).clicked()
}

pub fn btn_sm(ui: &mut Ui, text: &str) -> bool {
    button(ui, text, Kind::Normal, true, true).clicked()
}

pub fn primary(ui: &mut Ui, text: &str, enabled: bool) -> bool {
    button(ui, text, Kind::Primary, false, enabled).clicked()
}

pub fn danger_sm(ui: &mut Ui, text: &str) -> bool {
    button(ui, text, Kind::Danger, true, true).clicked()
}

pub fn ghost_sm(ui: &mut Ui, text: &str) -> bool {
    button(ui, text, Kind::Ghost, true, true).clicked()
}

pub fn state_color(s: ComponentState) -> Color32 {
    let c = p();
    match s {
        ComponentState::Ok => c.ok,
        ComponentState::Warn => c.warn,
        ComponentState::Error => c.err,
        ComponentState::Busy => c.info,
        ComponentState::Off => c.faint,
    }
}

pub fn dot(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.0, color);
}

pub fn status_row(ui: &mut Ui, name: &str, st: &ComponentStatus) {
    ui.horizontal(|ui| {
        dot(ui, state_color(st.state));
        ui.allocate_ui_with_layout(Vec2::new(150.0, 18.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_min_width(150.0);
            ui.add(egui::Label::new(RichText::new(name)).truncate());
        });
        ui.add(egui::Label::new(muted(&st.label).size(12.5)).truncate()).on_hover_text(&st.label);
    });
}

pub fn meter(ui: &mut Ui, value: f32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 8.0), Sense::hover());
    let c = p();
    ui.painter().rect_filled(rect, CornerRadius::same(4), c.surface3);
    let pct = (value.max(0.0).sqrt() * 2.2).min(1.0);
    let mut fill = rect;
    fill.set_width(rect.width() * pct);
    ui.painter().rect_filled(fill, CornerRadius::same(4), if pct > 0.85 { c.warn } else { c.ok });
}

pub fn progress(ui: &mut Ui, frac: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 8.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(4), p().surface3);
    let mut fill = rect;
    fill.set_width(rect.width() * frac.clamp(0.02, 1.0));
    ui.painter().rect_filled(fill, CornerRadius::same(4), p().info);
}

pub fn chip(ui: &mut Ui, text: &str, accent: bool) {
    let c = p();
    let (fill, fg) = if accent { (c.accent.gamma_multiply(0.2), c.accent) } else { (c.surface3, c.muted) };
    egui::Frame::new().fill(fill).corner_radius(CornerRadius::same(9)).inner_margin(Margin::symmetric(8, 2)).show(ui, |ui| {
        ui.label(RichText::new(text).size(12.0).color(fg));
    });
}

/// Clickable pill (mode selector, tabs).
pub fn pill(ui: &mut Ui, text: &str, on: bool, enabled: bool) -> bool {
    let c = p();
    let (fill, fg, stroke) = if on { (c.accent.gamma_multiply(0.2), c.accent, Stroke::new(1.0, c.accent)) } else { (c.surface2, c.text, Stroke::new(1.0, c.border)) };
    ui.add_enabled(enabled, egui::Button::new(RichText::new(text).color(fg).size(13.0)).fill(fill).stroke(stroke).corner_radius(CornerRadius::same(14))).clicked()
}

#[derive(Clone, Copy, PartialEq)]
pub enum Tone {
    Info,
    Warn,
    Error,
    Ok,
}

pub fn banner<R>(ui: &mut Ui, tone: Tone, add: impl FnOnce(&mut Ui) -> R) -> R {
    let c = p();
    let col = match tone {
        Tone::Info => c.info,
        Tone::Warn => c.warn,
        Tone::Error => c.err,
        Tone::Ok => c.ok,
    };
    egui::Frame::new()
        .fill(col.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, col.gamma_multiply(0.45)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.with_layout(egui::Layout::top_down(egui::Align::Min), add).inner
        })
        .inner
}

pub fn banner_text(ui: &mut Ui, tone: Tone, text: &str) {
    banner(ui, tone, |ui| {
        ui.add(egui::Label::new(RichText::new(text).size(13.0)).wrap());
    });
}

pub fn error_text(ui: &mut Ui, text: &Option<String>) {
    if let Some(t) = text {
        banner_text(ui, Tone::Error, t);
    }
}

pub fn kbd(ui: &mut Ui, text: &str) {
    egui::Frame::new()
        .fill(p().surface3)
        .stroke(Stroke::new(1.0, p().border))
        .corner_radius(CornerRadius::same(4))
        .inner_margin(Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).monospace().size(12.0));
        });
}

pub fn pretty_accel(a: &str) -> String {
    a.replace("Control", "Ctrl").replace('+', " + ")
}

/// iOS-style switch with label and optional hint. Returns true when toggled.
pub fn toggle(ui: &mut Ui, on: &mut bool, label: &str, hint: Option<&str>) -> bool {
    let mut changed = false;
    ui.horizontal_top(|ui| {
        let size = Vec2::new(36.0, 20.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        if resp.clicked() {
            *on = !*on;
            changed = true;
        }
        let t = ui.ctx().animate_bool(resp.id, *on);
        let c = p();
        let bg = if *on { c.accent } else { c.surface3 };
        ui.painter().rect(rect, CornerRadius::same(10), bg, Stroke::new(1.0, if *on { c.accent } else { c.border }), egui::StrokeKind::Inside);
        let x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, t);
        ui.painter().circle_filled(egui::pos2(x, rect.center().y), 7.0, if *on { c.accent_ink } else { c.muted });
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            if ui.add(egui::Label::new(label).wrap().sense(Sense::click())).clicked() {
                *on = !*on;
                changed = true;
            }
            if let Some(h) = hint {
                ui.add(egui::Label::new(muted(h).size(12.5)).wrap());
            }
        });
    });
    changed
}

/// Labelled form field.
pub fn field<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.label(muted(label).size(12.5));
        add(ui)
    })
    .inner
}

pub fn combo<T: PartialEq + Clone>(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, cur: &mut T, options: &[(T, String)], width: f32) -> bool {
    let selected = options.iter().find(|(v, _)| v == cur).map(|(_, l)| l.clone()).unwrap_or_default();
    let mut changed = false;
    egui::ComboBox::from_id_salt(id).selected_text(selected).width(width).show_ui(ui, |ui| {
        for (v, l) in options {
            if ui.selectable_label(v == cur, l).clicked() && v != cur {
                *cur = v.clone();
                changed = true;
            }
        }
    });
    changed
}

pub fn text_input(ui: &mut Ui, value: &mut String, hint: &str, width: f32) -> egui::Response {
    ui.add(egui::TextEdit::singleline(value).hint_text(hint.to_string()).desired_width(width).margin(Margin::symmetric(8, 6)))
}

/// Result slot for work done off the UI thread; the UI polls it each frame.
pub struct Promise<T>(Arc<Mutex<Option<T>>>);

impl<T> Clone for Promise<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: Send + 'static> Promise<T> {
    pub fn spawn(core: &crate::core::Core, fut: impl std::future::Future<Output = T> + Send + 'static) -> Self {
        let slot = Arc::new(Mutex::new(None));
        let s2 = slot.clone();
        let ctx = core.ui.ctx.get().cloned();
        core.rt.spawn(async move {
            let v = fut.await;
            *s2.lock().unwrap() = Some(v);
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
        Self(slot)
    }

    pub fn thread(core: &crate::core::Core, f: impl FnOnce() -> T + Send + 'static) -> Self {
        let slot = Arc::new(Mutex::new(None));
        let s2 = slot.clone();
        let ctx = core.ui.ctx.get().cloned();
        std::thread::spawn(move || {
            let v = f();
            *s2.lock().unwrap() = Some(v);
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
        Self(slot)
    }

    pub fn take(&self) -> Option<T> {
        self.0.lock().unwrap().take()
    }
}
