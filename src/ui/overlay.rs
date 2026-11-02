use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, CornerRadius, CursorIcon, Pos2, Rect, RichText, Sense, Stroke, Vec2, ViewportCommand};

use super::md::{code_blocks, markdown};
use super::theme::{p, semibold};
use super::widgets::{dot, pretty_accel};
use crate::core::Core;
use crate::i18n::t;
use crate::types::*;
use crate::win;

const COMPACT_MIN: f32 = 120.0;
const COMPACT_MAX: f32 = 440.0;
const HEADER: f32 = 32.0;

#[derive(Default, PartialEq, Clone, Copy)]
enum OvTab {
    #[default]
    Answer,
    Code,
    Questions,
}

#[derive(Default)]
pub struct OverlayUi {
    tab: OvTab,
    last_rect: Option<win::Rect>,
    changed_at: Option<Instant>,
    saved: Option<win::Rect>,
    scroll: f32,
}

fn state_label(s: ConvState, has_q: bool) -> &'static str {
    match s {
        ConvState::Listening | ConvState::PartialTranscript => t("Слухаю…", "Listening…"),
        ConvState::QuestionCandidate => t("Можливе питання", "Possible question"),
        ConvState::QuestionConfirmed => t("Питання", "Question"),
        ConvState::Generating => t("Генерую…", "Generating…"),
        ConvState::Paused => t("Пауза — захоплення зупинено", "Paused — capture stopped"),
        ConvState::Stopped | ConvState::Idle => t("Сесію не запущено", "No active session"),
        _ if has_q => t("Питання", "Question"),
        _ => t("Слухаю…", "Listening…"),
    }
}

fn short_prov(a: &AnswerEvent) -> String {
    if a.model.is_empty() {
        return String::new();
    }
    let mut parts = vec![a.model.clone()];
    if let Some(ms) = a.first_token_ms {
        parts.push(format!("{:.1} {}", ms as f64 / 1000.0, t("с", "s")));
    }
    if a.status == AnswerStatus::Done {
        parts.push(match a.usage_cost_usd {
            None => t("Ціна невідома", "Price unknown").into(),
            Some(0.0) => t("локально", "local").into(),
            Some(c) => format!("${c:.4}"),
        });
    }
    parts.join(" · ")
}

fn icon_btn(ui: &mut egui::Ui, glyph: &str, tip: &str) -> bool {
    ui.add(egui::Button::new(RichText::new(glyph).size(15.0).color(p().muted)).frame_when_inactive(false).min_size(Vec2::new(26.0, 24.0)).corner_radius(5))
        .on_hover_text(tip)
        .clicked()
}

fn link(ui: &mut egui::Ui, text: &str, color: Color32) -> bool {
    ui.add(egui::Label::new(RichText::new(text).color(color).underline()).sense(Sense::click())).on_hover_cursor(CursorIcon::PointingHand).clicked()
}

pub fn show(ui: &mut egui::Ui, core: &Arc<Core>, state: &Arc<Mutex<OverlayUi>>) {
    let mut st = state.lock().unwrap();
    let s = core.s();
    let snap = core.session.snapshot();
    let ctx = ui.ctx().clone();
    let fs = s.overlay.font_scale.clamp(0.8, 1.8);
    let size = 14.0 * fs;
    let alpha = (s.overlay.opacity.clamp(0.15, 1.0) * 255.0) as u8;
    let c = p();
    let tint = |col: Color32, a: u8| Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), a);

    let full = ui.max_rect();
    let painter = ui.painter().clone();
    painter.rect(full.shrink(0.5), CornerRadius::same(10), tint(c.surface, alpha), Stroke::new(1.0, c.border), egui::StrokeKind::Inside);
    let head = Rect::from_min_size(full.min, Vec2::new(full.width(), HEADER));
    painter.rect_filled(head.shrink(1.0), CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 }, tint(c.surface2, alpha));
    painter.hline(full.x_range(), head.bottom(), Stroke::new(1.0, c.border));

    // Dragging the header moves the window; it never takes focus.
    let drag = ui.interact(head, ui.scope_id().with("ov-drag"), Sense::click_and_drag());
    if drag.drag_started() {
        ctx.send_viewport_cmd(ViewportCommand::StartDrag);
    }
    resize_edges(ui, full);

    let expanded = s.overlay.expanded;
    let q = snap.active_question.clone();
    let mut head_ui = ui.new_child(egui::UiBuilder::new().max_rect(head.shrink2(Vec2::new(10.0, 4.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    {
        let ui = &mut head_ui;
        let col = if snap.listening {
            if snap.state == ConvState::Generating {
                c.info
            } else {
                c.ok
            }
        } else if snap.session_id.is_some() {
            c.warn
        } else {
            c.faint
        };
        dot(ui, col);
        ui.label(RichText::new(state_label(snap.state, q.is_some())).size(12.0).color(c.muted));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if icon_btn(ui, "×", t("Сховати", "Hide")) {
                core.hide_overlay();
            }
            if icon_btn(ui, if expanded { "▭" } else { "⤢" }, if expanded { t("Згорнути", "Collapse") } else { t("Розгорнути", "Expand") }) {
                core.set_expanded(!expanded, QuestionTrigger::Manual);
                if !expanded {
                    resize_height(&ctx, 560.0);
                }
            }
            if icon_btn(ui, "⟳", t("Наступне питання", "Next question")) {
                core.session.next_question();
            }
            if icon_btn(ui, "↵", t("Відповісти зараз", "Answer now")) {
                core.session.answer_now(AnswerKind::Say, QuestionTrigger::Manual);
            }
        });
    }

    let answers = &snap.answers;
    let say = answers.get(&AnswerKind::Say).or(answers.get(&AnswerKind::Screen));
    let deep = answers.get(&AnswerKind::Deep);
    let mut codes = code_blocks(deep.map(|d| d.text.as_str()).unwrap_or(""));
    codes.extend(code_blocks(answers.get(&AnswerKind::Screen).map(|d| d.text.as_str()).unwrap_or("")));

    let footer_h = 22.0;
    let tabs_h = if expanded { 28.0 } else { 0.0 };
    let body = Rect::from_min_max(Pos2::new(full.left() + 12.0, head.bottom() + 6.0 + tabs_h), Pos2::new(full.right() - 8.0, full.bottom() - footer_h - 4.0));
    if expanded {
        let tabs = Rect::from_min_size(Pos2::new(full.left() + 10.0, head.bottom() + 4.0), Vec2::new(full.width() - 20.0, 24.0));
        let mut tu = ui.new_child(egui::UiBuilder::new().max_rect(tabs).layout(egui::Layout::left_to_right(egui::Align::Center)));
        for (tab, label) in [
            (OvTab::Answer, t("Відповідь", "Answer").to_string()),
            (OvTab::Code, if codes.is_empty() { t("Код", "Code").to_string() } else { format!("{} ({})", t("Код", "Code"), codes.len()) }),
            (OvTab::Questions, t("Останні питання", "Recent questions").to_string()),
        ] {
            let on = st.tab == tab;
            if tu.add(egui::Button::new(RichText::new(label).size(12.5).color(if on { c.accent } else { c.muted })).frame_when_inactive(on).fill(c.surface3)).clicked() {
                st.tab = tab;
            }
        }
    }

    let pending = std::mem::take(&mut *core.ui.overlay_scroll.lock().unwrap());
    let mut body_ui = ui.new_child(egui::UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::Min)));
    let mut area = egui::ScrollArea::vertical().id_salt("ov-body").auto_shrink([false, true]);
    if pending != 0.0 {
        area = area.vertical_scroll_offset((st.scroll + pending).max(0.0));
    }
    let tab = if expanded { st.tab } else { OvTab::Answer };
    let out = area.show(&mut body_ui, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.set_width(ui.available_width() - 6.0);
        if let Some(b) = &snap.budget_blocked {
            alert(ui, &format!("{b}. {}", t("Платні запити зупинено.", "Paid requests are paused.")), size);
            if link(ui, t("Продовжити понад ліміт", "Continue over the limit"), c.accent) {
                core.session.approve_budget();
            }
        }
        if let (Some(cand), None) = (snap.candidates.last(), &q) {
            ui.label(RichText::new(format!("{}: {}", t("Можливе питання", "Possible question"), cand.text)).size(size));
            ui.horizontal(|ui| {
                if link(ui, t("Це питання", "It is a question"), c.accent) {
                    core.session.confirm_candidate(&cand.question_id);
                }
                if link(ui, t("Ні", "No"), c.faint) {
                    core.session.dismiss_candidate(&cand.question_id);
                }
            });
        }
        match tab {
            OvTab::Answer => {
                let live = snap.partial[1].clone().or(snap.partial[0].clone());
                if let Some(q) = &q {
                    let mut text = format!("{}{}", if q.source == Source::Remote { "❝ " } else { "" }, q.text);
                    if q.revision > 1 {
                        text += t(" · уточнено", " · refined");
                    }
                    ui.add(egui::Label::new(RichText::new(text).size(size * 0.93).color(c.muted)).wrap());
                    if let Some(tr) = &snap.translation {
                        ui.add(egui::Label::new(RichText::new(tr).size(size * 0.93).color(c.faint)).wrap());
                    }
                } else if let Some(l) = &live {
                    ui.add(egui::Label::new(RichText::new(l).size(size * 0.93).color(c.faint)).wrap());
                }
                match say {
                    Some(a) if !a.text.is_empty() => {
                        if a.kind == AnswerKind::Screen {
                            markdown(ui, &a.text, size);
                        } else {
                            let mut txt = a.text.clone();
                            if a.status == AnswerStatus::Streaming {
                                txt.push('▍');
                            }
                            ui.add(egui::Label::new(RichText::new(txt).font(semibold(size * 1.08))).wrap());
                        }
                    }
                    _ if q.is_some() && (say.is_some_and(|a| a.status == AnswerStatus::Streaming) || snap.state == ConvState::Generating) => {
                        ui.label(RichText::new("…").size(size).color(c.faint));
                    }
                    _ => {}
                }
                let pts: Vec<String> = answers
                    .get(&AnswerKind::Points)
                    .map(|a| a.text.lines().map(|l| l.trim().trim_start_matches(['•', '-', '*']).trim().to_string()).filter(|l| !l.is_empty()).take(if expanded { 5 } else { 3 }).collect())
                    .unwrap_or_default();
                for pt in pts {
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(RichText::new("•").size(size).color(c.accent));
                        ui.add(egui::Label::new(RichText::new(pt).size(size)).wrap());
                    });
                }
                if expanded {
                    if let Some(d) = deep.filter(|d| !d.text.is_empty()) {
                        egui::Frame::new().fill(tint(c.surface2, alpha.saturating_add(20))).corner_radius(6).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            markdown(ui, &d.text, size * 0.95);
                        });
                    } else if q.is_some() && deep.is_none() && link(ui, &format!("{} →", t("Детальніше", "Deep dive")), c.accent) {
                        core.session.answer_now(AnswerKind::Deep, QuestionTrigger::Manual);
                    }
                }
                if let Some(e) = [AnswerKind::Say, AnswerKind::Points, AnswerKind::Deep, AnswerKind::Screen]
                    .iter()
                    .filter_map(|k| answers.get(k))
                    .find(|a| matches!(a.status, AnswerStatus::Error | AnswerStatus::Blocked))
                    .and_then(|a| a.error.as_ref())
                {
                    alert(ui, &e.line(), size);
                }
                if let Some(n) = &snap.notice {
                    ui.add(egui::Label::new(RichText::new(n).size(size * 0.86).color(c.info)).wrap());
                }
                if q.is_none() && live.is_none() && snap.candidates.is_empty() && snap.session_id.is_some() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(t("Чекаю питання.", "Waiting for a question.")).size(size * 0.9).color(c.faint));
                        ui.label(RichText::new(pretty_accel(&s.shortcuts.answer_now)).monospace().size(size * 0.8).color(c.muted));
                        ui.label(RichText::new(t("— відповісти одразу.", "— answer right away.")).size(size * 0.9).color(c.faint));
                    });
                }
                if snap.session_id.is_none() {
                    ui.label(RichText::new(t("Сесію не запущено", "No active session")).size(size * 0.9).color(c.faint));
                }
            }
            OvTab::Code => {
                if codes.is_empty() {
                    ui.add(egui::Label::new(RichText::new(t("Код з'явиться після «Детальніше» або знімка екрана з режимом «з кодом».", "Code appears after “Deep dive” or a screenshot with “with code”.")).color(c.faint)).wrap());
                }
                for code in &codes {
                    if link(ui, t("Копіювати", "Copy"), c.accent) {
                        ctx.copy_text(code.clone());
                    }
                    super::md::code_block(ui, code, size);
                }
            }
            OvTab::Questions => {
                if snap.recent_questions.is_empty() {
                    ui.label(RichText::new(t("Поки порожньо", "Empty so far")).color(c.faint));
                }
                for (_, text) in &snap.recent_questions {
                    ui.add(egui::Label::new(RichText::new(format!("· {text}")).size(size * 0.95)).wrap());
                }
            }
        }
    });
    st.scroll = out.state.clamped_offset().y;

    let foot = Rect::from_min_max(Pos2::new(full.left() + 12.0, full.bottom() - footer_h - 2.0), Pos2::new(full.right() - 12.0, full.bottom() - 2.0));
    let mut fu = ui.new_child(egui::UiBuilder::new().max_rect(foot).layout(egui::Layout::left_to_right(egui::Align::Center)));
    fu.add(egui::Label::new(RichText::new(say.or(answers.get(&AnswerKind::Points)).map(short_prov).unwrap_or_default()).size(11.5).color(c.faint)).truncate());
    if snap.cost.unknown_priced > 0 {
        fu.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.label(RichText::new(t("Ціна невідома", "Price unknown")).size(11.5).color(c.faint)));
    }

    // Compact mode follows the content so an answer is never silently cut off; expanded keeps the user's size.
    if !expanded && !drag.dragged() {
        let want = (HEADER + 6.0 + out.content_size.y + footer_h + 12.0).clamp(COMPACT_MIN, COMPACT_MAX);
        if (want - full.height()).abs() > 6.0 {
            resize_height(&ctx, want);
        }
    }
    track_bounds(core, &mut st, &ctx);
}

fn alert(ui: &mut egui::Ui, text: &str, size: f32) {
    egui::Frame::new().fill(p().err.gamma_multiply(0.15)).corner_radius(6).inner_margin(egui::Margin::symmetric(8, 5)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(text).size(size * 0.9).color(p().err)).wrap());
    });
}

fn resize_height(ctx: &egui::Context, points: f32) {
    if let Some(r) = win::OVERLAY.rect() {
        let h = (points * ctx.pixels_per_point()).round() as i32;
        if (h - r.h).abs() > 2 {
            win::OVERLAY.set_rect(win::Rect { h, ..r }, false, false);
        }
    }
}

/// Saved position/size after the user stops moving or resizing for 400 ms.
fn track_bounds(core: &Core, st: &mut OverlayUi, ctx: &egui::Context) {
    let Some(r) = win::OVERLAY.rect() else { return };
    if !win::OVERLAY.is_visible() {
        return;
    }
    if st.last_rect != Some(r) {
        st.last_rect = Some(r);
        st.changed_at = Some(Instant::now());
        ctx.request_repaint_after(Duration::from_millis(450));
    } else if st.changed_at.is_some_and(|t| t.elapsed() >= Duration::from_millis(400)) && st.saved != Some(r) {
        st.saved = Some(r);
        st.changed_at = None;
        if st.saved.is_some() {
            core.save_overlay_bounds(r);
        }
    }
}

fn resize_edges(ui: &mut egui::Ui, full: Rect) {
    let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else { return };
    let m = 6.0;
    let (l, r, top, b) = (pos.x - full.left() < m, full.right() - pos.x < m, pos.y - full.top() < 3.0, full.bottom() - pos.y < m);
    use egui::ResizeDirection as D;
    let dir = match (l, r, top, b) {
        (true, _, _, true) => Some((D::SouthWest, CursorIcon::ResizeSouthWest)),
        (_, true, _, true) => Some((D::SouthEast, CursorIcon::ResizeSouthEast)),
        (true, _, true, _) => Some((D::NorthWest, CursorIcon::ResizeNorthWest)),
        (_, true, true, _) => Some((D::NorthEast, CursorIcon::ResizeNorthEast)),
        (true, ..) => Some((D::West, CursorIcon::ResizeWest)),
        (_, true, ..) => Some((D::East, CursorIcon::ResizeEast)),
        (_, _, _, true) => Some((D::South, CursorIcon::ResizeSouth)),
        (_, _, true, _) => Some((D::North, CursorIcon::ResizeNorth)),
        _ => None,
    };
    if let Some((d, cursor)) = dir {
        ui.ctx().set_cursor_icon(cursor);
        if ui.input(|i| i.pointer.primary_pressed()) {
            ui.ctx().send_viewport_cmd(ViewportCommand::BeginResize(d));
        }
    }
}
