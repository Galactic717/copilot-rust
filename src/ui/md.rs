//! Minimal markdown → egui (no HTML path): headings, lists, fenced code, inline code, bold.

use eframe::egui::{self, text::LayoutJob, Color32, FontId, Margin, TextFormat, Ui};

use super::theme::{p, semibold};

pub fn markdown(ui: &mut Ui, text: &str, size: f32) {
    let lines: Vec<&str> = text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let mut i = 0;
    ui.spacing_mut().item_spacing.y = 4.0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim_start().starts_with("```") {
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                body.push(lines[i]);
                i += 1;
            }
            i += 1;
            code_block(ui, &body.join("\n"), size);
            continue;
        }
        if let Some(h) = heading(line) {
            ui.add_space(2.0);
            ui.add(egui::Label::new(inline(h, semibold(size + 1.0), p().text)).wrap());
            i += 1;
            continue;
        }
        if list_item(line).is_some() {
            while i < lines.len() {
                let Some(item) = list_item(lines[i]) else { break };
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(egui::RichText::new("•").size(size).color(p().accent));
                    ui.add(egui::Label::new(inline(item, FontId::proportional(size), p().text)).wrap());
                });
                i += 1;
            }
            continue;
        }
        if line.trim().is_empty() {
            i += 1;
            continue;
        }
        let mut para = Vec::new();
        while i < lines.len() && !lines[i].trim().is_empty() && heading(lines[i]).is_none() && list_item(lines[i]).is_none() && !lines[i].trim_start().starts_with("```") {
            para.push(lines[i].trim());
            i += 1;
        }
        ui.add(egui::Label::new(inline(&para.join(" "), FontId::proportional(size), p().text)).wrap());
    }
}

pub fn code_block(ui: &mut Ui, code: &str, size: f32) {
    egui::Frame::new().fill(p().surface3).corner_radius(6).inner_margin(Margin::same(8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(egui::RichText::new(code).monospace().size(size - 1.0)).wrap().selectable(true));
    });
}

fn heading(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|c| *c == '#').count();
    (1..=6).contains(&hashes).then(|| t[hashes..].strip_prefix(' ')).flatten()
}

fn list_item(line: &str) -> Option<&str> {
    let t = line.trim_start();
    for m in ["- ", "* ", "• "] {
        if let Some(r) = t.strip_prefix(m) {
            return Some(r);
        }
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let rest = &t[digits..];
        if let Some(r) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some(r);
        }
    }
    None
}

/// Inline `code` and **bold** spans.
pub fn inline(s: &str, font: FontId, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    let normal = TextFormat { font_id: font.clone(), color, ..Default::default() };
    let bold = TextFormat { font_id: semibold(font.size), color, ..Default::default() };
    let code = TextFormat { font_id: FontId::monospace(font.size - 1.0), color, background: p().surface3, ..Default::default() };
    let mut rest = s;
    while !rest.is_empty() {
        let next_code = rest.find('`');
        let next_bold = rest.find("**");
        match (next_code, next_bold) {
            (Some(c), b) if b.is_none_or(|b| c < b) => {
                if let Some(end) = rest[c + 1..].find('`') {
                    job.append(&rest[..c], 0.0, normal.clone());
                    job.append(&rest[c + 1..c + 1 + end], 0.0, code.clone());
                    rest = &rest[c + 2 + end..];
                    continue;
                }
                job.append(rest, 0.0, normal.clone());
                break;
            }
            (_, Some(b)) => {
                if let Some(end) = rest[b + 2..].find("**").filter(|e| *e > 0) {
                    job.append(&rest[..b], 0.0, normal.clone());
                    job.append(&rest[b + 2..b + 2 + end], 0.0, bold.clone());
                    rest = &rest[b + 4 + end..];
                    continue;
                }
                job.append(rest, 0.0, normal.clone());
                break;
            }
            _ => {
                job.append(rest, 0.0, normal.clone());
                break;
            }
        }
    }
    job
}

/// Fenced code blocks of a markdown answer (for the overlay's «Код» tab).
pub fn code_blocks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<Vec<&str>> = None;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            match cur.take() {
                Some(body) => out.push(body.join("\n").trim_end().to_string()),
                None => cur = Some(Vec::new()),
            }
        } else if let Some(b) = cur.as_mut() {
            b.push(line);
        }
    }
    out
}
