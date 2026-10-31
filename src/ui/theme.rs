use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle};

use crate::types::Theme;

pub struct Pal {
    pub bg: Color32,
    pub surface: Color32,
    pub surface2: Color32,
    pub surface3: Color32,
    pub border: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub faint: Color32,
    pub accent: Color32,
    pub accent_ink: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub info: Color32,
}

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub static DARK: Pal = Pal {
    bg: hex(0x0f1115),
    surface: hex(0x161920),
    surface2: hex(0x1d212a),
    surface3: hex(0x252a35),
    border: hex(0x2a303c),
    text: hex(0xe9ebef),
    muted: hex(0x9aa3b2),
    faint: hex(0x6b7383),
    accent: hex(0xf5a623),
    accent_ink: hex(0x1a1204),
    ok: hex(0x3fb68b),
    warn: hex(0xe0a030),
    err: hex(0xe5534b),
    info: hex(0x6f9cf0),
};

pub static LIGHT: Pal = Pal {
    bg: hex(0xf4f4f1),
    surface: hex(0xffffff),
    surface2: hex(0xf2f2ee),
    surface3: hex(0xe8e8e3),
    border: hex(0xdcdcd5),
    text: hex(0x17191d),
    muted: hex(0x5d6470),
    faint: hex(0x8a909a),
    accent: hex(0xc97f00),
    accent_ink: hex(0xffffff),
    ok: hex(0x2e9c74),
    warn: hex(0xc4861a),
    err: hex(0xd0433b),
    info: hex(0x4a7bd8),
};

static IS_LIGHT: AtomicBool = AtomicBool::new(false);

pub fn p() -> &'static Pal {
    if IS_LIGHT.load(Ordering::Relaxed) {
        &LIGHT
    } else {
        &DARK
    }
}

pub const SEMIBOLD: &str = "semibold";

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

/// Segoe UI from Windows (falls back to egui's bundled fonts if missing), Consolas for code, symbol/emoji fallbacks.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let dir = std::path::PathBuf::from(std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into())).join("Fonts");
    let mut load = |name: &str, file: &str| -> bool {
        match std::fs::read(dir.join(file)) {
            Ok(bytes) => {
                fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
                true
            }
            Err(_) => false,
        }
    };
    let ui = load("segoe", "segoeui.ttf");
    let sb = load("segoe-sb", "seguisb.ttf");
    let mono = load("consolas", "consola.ttf");
    let sym = load("segoe-sym", "seguisym.ttf");
    let emoji = load("segoe-emoji", "seguiemj.ttf");
    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    if ui {
        prop.insert(0, "segoe".into());
    }
    if sym {
        prop.push("segoe-sym".into());
    }
    if emoji {
        prop.push("segoe-emoji".into());
    }
    let prop_list = prop.clone();
    let monos = fonts.families.entry(FontFamily::Monospace).or_default();
    if mono {
        monos.insert(0, "consolas".into());
    }
    if ui {
        monos.push("segoe".into());
    }
    let mut semi = if sb { vec!["segoe-sb".to_string()] } else { Vec::new() };
    semi.extend(prop_list);
    fonts.families.insert(FontFamily::Name(SEMIBOLD.into()), semi);
    ctx.set_fonts(fonts);
}

pub fn apply(ctx: &egui::Context, theme: Theme) {
    let light = theme == Theme::Light;
    let changed = IS_LIGHT.swap(light, Ordering::Relaxed) != light;
    let key = egui::Id::unique("copilot-theme-applied");
    if !changed && ctx.data(|d| d.get_temp::<bool>(key)).is_some() {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(key, true));
    let c = p();
    let mut v = if light { egui::Visuals::light() } else { egui::Visuals::dark() };
    v.panel_fill = c.bg;
    v.window_fill = c.surface;
    v.window_stroke = Stroke::new(1.0, c.border);
    v.extreme_bg_color = c.surface2;
    v.text_edit_bg_color = Some(c.surface2);
    v.faint_bg_color = c.surface2;
    v.code_bg_color = c.surface3;
    v.hyperlink_color = c.accent;
    v.override_text_color = Some(c.text);
    v.warn_fg_color = c.warn;
    v.error_fg_color = c.err;
    v.selection.bg_fill = c.accent.gamma_multiply(0.45);
    v.selection.stroke = Stroke::new(1.0, c.accent);
    let r = CornerRadius::same(6);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = c.surface;
    w.noninteractive.weak_bg_fill = c.surface;
    w.noninteractive.bg_stroke = Stroke::new(1.0, c.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, c.text);
    w.noninteractive.corner_radius = r;
    for (wv, fill) in [(&mut w.inactive, c.surface3), (&mut w.hovered, c.border), (&mut w.active, c.border), (&mut w.open, c.surface3)] {
        wv.bg_fill = fill;
        wv.weak_bg_fill = fill;
        wv.bg_stroke = Stroke::new(1.0, c.border);
        wv.fg_stroke = Stroke::new(1.0, c.text);
        wv.corner_radius = r;
    }
    w.hovered.bg_stroke = Stroke::new(1.0, c.faint);
    w.active.bg_stroke = Stroke::new(1.0, c.accent);
    // Both slots get the same visuals, so the app theme wins over the system light/dark switch.
    ctx.set_visuals_of(egui::Theme::Dark, v.clone());
    ctx.set_visuals_of(egui::Theme::Light, v);
    ctx.all_styles_mut(|s| {
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        s.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
        s.text_styles.insert(TextStyle::Heading, semibold(22.0));
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
    });
}
