use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, CornerRadius, Pos2, Rect, RichText, Sense, Stroke, Vec2};

use super::theme::p;
use super::widgets::*;
use crate::core::Core;
use crate::i18n::t;
use crate::types::ScreenTask;
use crate::win;

const LANGS: [&str; 9] = ["Python", "TypeScript", "JavaScript", "Java", "Go", "C#", "C++", "Kotlin", "SQL"];

pub struct PickerUi {
    generation: u64,
    tex: Option<egui::TextureHandle>,
    img_size: Vec2,
    sel: Option<Rect>,
    drag: Option<Pos2>,
    cur: Option<Rect>,
    blur_mode: bool,
    blurs: Vec<Rect>,
    with_code: bool,
    code_lang: String,
}

impl Default for PickerUi {
    fn default() -> Self {
        Self { generation: 0, tex: None, img_size: Vec2::ZERO, sel: None, drag: None, cur: None, blur_mode: false, blurs: Vec::new(), with_code: false, code_lang: "Python".into() }
    }
}

pub fn show(ui: &mut egui::Ui, core: &Arc<Core>, state: &Arc<Mutex<PickerUi>>) {
    let mut st = state.lock().unwrap();
    let ctx = ui.ctx().clone();
    let (gen, has_vision, ocr) = {
        let p = core.ui.picker.lock().unwrap();
        if p.generation != st.generation {
            st.generation = p.generation;
            st.sel = None;
            st.blurs.clear();
            st.blur_mode = false;
            st.tex = p.shot.as_ref().map(|s| {
                st.img_size = Vec2::new(s.width as f32, s.height as f32);
                ctx.load_texture("screen", egui::ColorImage::from_rgba_unmultiplied([s.width as usize, s.height as usize], &s.rgba), egui::TextureOptions::LINEAR)
            });
        }
        (p.generation, p.has_vision, p.ocr)
    };
    let _ = gen;
    let full = ui.max_rect();
    let painter = ui.painter().clone();
    painter.rect_filled(full, 0.0, Color32::BLACK);
    let Some(tex) = st.tex.clone() else { return };
    painter.image(tex.id(), full, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);

    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        st.tex = None;
        core.cancel_region();
        return;
    }

    // Shade everything except the selection.
    let shade = Color32::from_black_alpha(110);
    match st.sel {
        Some(s) => {
            for r in [
                Rect::from_min_max(full.min, Pos2::new(full.right(), s.top())),
                Rect::from_min_max(Pos2::new(full.left(), s.bottom()), full.max),
                Rect::from_min_max(Pos2::new(full.left(), s.top()), Pos2::new(s.left(), s.bottom())),
                Rect::from_min_max(Pos2::new(s.right(), s.top()), Pos2::new(full.right(), s.bottom())),
            ] {
                painter.rect_filled(r, 0.0, shade);
            }
            painter.rect_stroke(s, 0.0, Stroke::new(2.0, p().accent), egui::StrokeKind::Outside);
        }
        None => {
            painter.rect_filled(full, 0.0, shade);
        }
    }
    for b in &st.blurs {
        painter.rect_filled(*b, 0.0, Color32::from_gray(0x7a));
    }

    let bar_rect = st.sel.filter(|_| st.drag.is_none()).map(|s| {
        let size = Vec2::new(470.0, 120.0);
        let below = s.bottom() + size.y + 20.0 < full.bottom();
        let x = s.left().clamp(full.left() + 8.0, full.right() - size.x - 8.0);
        let y = if below { s.bottom() + 10.0 } else { (s.top() - size.y - 10.0).max(full.top() + 8.0) };
        Rect::from_min_size(Pos2::new(x, y), size)
    });

    let resp = ui.interact(full, ui.scope_id().with("picker-area"), Sense::drag());
    let over_bar = |pos: Pos2| bar_rect.is_some_and(|b| b.expand(4.0).contains(pos));
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos().filter(|p| !over_bar(*p)) {
            if st.sel.is_some() && !st.blur_mode {
                st.sel = None;
                st.blurs.clear();
            }
            st.drag = Some(pos);
            st.cur = Some(Rect::from_two_pos(pos, pos));
        }
    }
    if let (Some(start), Some(pos)) = (st.drag, resp.interact_pointer_pos()) {
        st.cur = Some(Rect::from_two_pos(start, pos));
    }
    if resp.drag_stopped() {
        if let Some(cur) = st.cur.take() {
            if cur.width() > 8.0 && cur.height() > 8.0 {
                if st.blur_mode && st.sel.is_some() {
                    st.blurs.push(cur);
                } else {
                    st.sel = Some(cur);
                }
            }
        }
        st.drag = None;
    }
    ui.ctx().set_cursor_icon(if st.blur_mode { egui::CursorIcon::Cell } else { egui::CursorIcon::Crosshair });
    if let Some(cur) = st.cur {
        if st.blur_mode && st.sel.is_some() {
            painter.rect_filled(cur, 0.0, Color32::from_gray(0x7a));
        } else {
            painter.rect_stroke(cur, 0.0, Stroke::new(2.0, p().accent), egui::StrokeKind::Outside);
        }
    }

    if st.sel.is_none() {
        let help = t("Виділіть область мишею. Esc — скасувати.", "Drag to select an area. Esc to cancel.");
        let r = Rect::from_center_size(Pos2::new(full.center().x, full.top() + 40.0), Vec2::new(420.0, 34.0));
        painter.rect_filled(r, CornerRadius::same(8), p().surface.gamma_multiply(0.95));
        painter.text(r.center(), egui::Align2::CENTER_CENTER, help, egui::FontId::proportional(14.0), p().text);
        return;
    }
    let Some(bar) = bar_rect else { return };
    let mut submit: Option<ScreenTask> = None;
    let mut bu = ui.new_child(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::top_down(egui::Align::Min)));
    egui::Frame::new().fill(p().surface).stroke(Stroke::new(1.0, p().border)).corner_radius(10).inner_margin(egui::Margin::same(10)).show(&mut bu, |ui| {
        ui.horizontal(|ui| {
            for (task, label) in [
                (ScreenTask::Explain, t("Поясни умову", "Explain the task")),
                (ScreenTask::Approach, t("Запропонуй підхід", "Suggest an approach")),
                (ScreenTask::Review, t("Розбери код", "Review the code")),
            ] {
                if button(ui, label, Kind::Primary, true, true).clicked() {
                    submit = Some(task);
                }
            }
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut st.with_code, t("з кодом", "with code"));
            if st.with_code {
                let opts: Vec<(String, String)> = LANGS.iter().map(|l| (l.to_string(), l.to_string())).collect();
                let mut cl = st.code_lang.clone();
                if combo(ui, "code-lang", &mut cl, &opts, 120.0) {
                    st.code_lang = cl;
                }
            }
            if button(ui, t("Розмити область", "Blur area"), if st.blur_mode { Kind::Primary } else { Kind::Normal }, true, true).clicked() {
                st.blur_mode = !st.blur_mode;
            }
            if ghost_sm(ui, t("Скасувати", "Cancel")) {
                st.tex = None;
                core.cancel_region();
            }
        });
        let mut hint = if has_vision {
            t("Надішлеться лише виділена область, після натискання кнопки.", "Only the selected area is sent, after you press a button.")
        } else if ocr {
            t("Модель без vision: текст розпізнається локально (OCR) і піде текстом.", "No vision model: text is recognized locally (OCR) and sent as text.")
        } else {
            t("Немає vision-моделі й OCR — додайте мовний пакет з OCR у Windows.", "No vision model and no OCR — add a Windows language pack with OCR.")
        }
        .to_string();
        if st.blur_mode {
            hint += t(" Обведіть особисті дані — їх буде зафарбовано.", " Outline personal data — it will be painted over.");
        }
        ui.add(egui::Label::new(RichText::new(hint).size(12.0).color(p().faint)).wrap());
    });

    if let (Some(task), Some(sel)) = (submit, st.sel) {
        let k = st.img_size.x / full.width();
        let to_px = |r: Rect| win::Rect {
            x: ((r.left() - full.left()) * k).round() as i32,
            y: ((r.top() - full.top()) * k).round() as i32,
            w: (r.width() * k).round() as i32,
            h: (r.height() * k).round() as i32,
        };
        let blurs = st.blurs.iter().map(|b| to_px(*b)).collect();
        st.tex = None;
        core.submit_region(to_px(sel), blurs, task, st.with_code, st.code_lang.clone());
    }
}
