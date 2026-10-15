//! Colours, type and the few custom widgets every page shares.
//!
//! Palette: the validated dataviz palette (categorical order is fixed, status colours are only
//! used for state and always come with an icon and a word). Surfaces are near-white / near-black
//! neutrals; the one accent is the categorical blue, which is also the colour of breathing.

use eframe::egui::{self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Margin, RichText, Stroke, TextStyle, Vec2};
use std::sync::Arc;

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

#[derive(Clone, Copy)]
pub struct Pal {
    pub dark: bool,
    pub bg: Color32,
    pub side: Color32,
    pub panel: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text2: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    /// categorical series colours, in their fixed order
    pub cat: [Color32; 8],
    pub good: Color32,
    pub warn: Color32,
    pub critical: Color32,
}

/// Sequential blue ramp, light to dark (magnitude: spectrogram, sleep depth).
pub const RAMP: [u32; 13] = [
    0xcde2fb, 0xb7d3f6, 0x9ec5f4, 0x86b6ef, 0x6da7ec, 0x5598e7, 0x3987e5, 0x2a78d6, 0x256abf, 0x1c5cab, 0x184f95, 0x104281, 0x0d366b,
];

pub fn ramp(v: f64) -> Color32 {
    let x = v.clamp(0.0, 1.0) * (RAMP.len() - 1) as f64;
    let i = (x.floor() as usize).min(RAMP.len() - 2);
    let f = (x - i as f64) as f32;
    let (a, b) = (hex(RAMP[i]), hex(RAMP[i + 1]));
    let l = |p: u8, q: u8| (p as f32 + (q as f32 - p as f32) * f).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

pub fn pal_for(dark: bool) -> Pal {
    if dark {
        Pal {
            dark,
            bg: hex(0x1a1a19),
            side: hex(0x131312),
            panel: hex(0x222221),
            border: hex(0x353431),
            text: hex(0xffffff),
            text2: hex(0xc3c2b7),
            muted: hex(0x8d8c84),
            accent: hex(0x3987e5),
            accent_soft: Color32::from_rgba_unmultiplied(0x39, 0x87, 0xe5, 40),
            cat: [hex(0x3987e5), hex(0xd95926), hex(0x199e70), hex(0xc98500), hex(0xd55181), hex(0x008300), hex(0x9085e9), hex(0xe66767)],
            good: hex(0x0ca30c),
            warn: hex(0xfab219),
            critical: hex(0xd03b3b),
        }
    } else {
        Pal {
            dark,
            bg: hex(0xfcfcfb),
            side: hex(0xf0f0ee),
            panel: hex(0xffffff),
            border: hex(0xe2e1dc),
            text: hex(0x0b0b0b),
            text2: hex(0x52514e),
            muted: hex(0x8a8984),
            accent: hex(0x2a78d6),
            accent_soft: Color32::from_rgba_unmultiplied(0x2a, 0x78, 0xd6, 30),
            cat: [hex(0x2a78d6), hex(0xeb6834), hex(0x1baf7a), hex(0xeda100), hex(0xe87ba4), hex(0x008300), hex(0x4a3aa7), hex(0xe34948)],
            good: hex(0x0ca30c),
            warn: hex(0xc98a00),
            critical: hex(0xd03b3b),
        }
    }
}

pub fn pal(ui: &egui::Ui) -> Pal {
    pal_for(ui.visuals().dark_mode)
}

pub fn medium() -> FontFamily {
    FontFamily::Name("medium".into())
}
pub fn bold() -> FontFamily {
    FontFamily::Name("bold".into())
}

pub fn install_fonts(ctx: &egui::Context) {
    let mut f = FontDefinitions::default();
    let add = |f: &mut FontDefinitions, name: &str, data: &'static [u8]| {
        f.font_data.insert(name.into(), Arc::new(FontData::from_static(data)));
    };
    add(&mut f, "noto", include_bytes!("../../assets/fonts/NotoSans-Regular.ttf"));
    add(&mut f, "noto-medium", include_bytes!("../../assets/fonts/NotoSans-Medium.ttf"));
    add(&mut f, "noto-bold", include_bytes!("../../assets/fonts/NotoSans-Bold.ttf"));
    add(&mut f, "noto-mono", include_bytes!("../../assets/fonts/NotoSansMono-Regular.ttf"));
    // egui's own fonts stay behind as fallbacks for symbols (status icons, arrows)
    let fallback = f.families[&FontFamily::Proportional].clone();
    f.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "noto".into());
    f.families.get_mut(&FontFamily::Monospace).unwrap().insert(0, "noto-mono".into());
    for (fam, face) in [(medium(), "noto-medium"), (bold(), "noto-bold")] {
        let mut v = vec![face.to_string()];
        v.extend(fallback.iter().cloned());
        f.families.insert(fam, v);
    }
    ctx.set_fonts(f);
}

fn visuals(dark: bool) -> egui::Visuals {
    let p = pal_for(dark);
    let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = p.bg;
    v.window_fill = p.panel;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.extreme_bg_color = if dark { hex(0x161615) } else { hex(0xffffff) };
    v.faint_bg_color = if dark { hex(0x1f1f1e) } else { hex(0xf6f6f4) };
    v.code_bg_color = v.faint_bg_color;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent_soft;
    v.selection.stroke = Stroke::new(1.0, p.accent);
    v.override_text_color = None;
    v.window_shadow = egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(if dark { 90 } else { 28 }) };
    v.popup_shadow = v.window_shadow;
    v.striped = false;
    let r = CornerRadius::same(7);
    let w = &mut v.widgets;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;
    w.inactive.weak_bg_fill = if dark { hex(0x2b2b29) } else { hex(0xefefec) };
    // bg_fill: checkboxes, radio buttons, slider rails (need to show on the page); buttons use weak_bg_fill
    w.inactive.bg_fill = if dark { hex(0x45443f) } else { hex(0xd6d5d0) };
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;
    w.hovered.weak_bg_fill = if dark { hex(0x363533) } else { hex(0xe5e5e1) };
    w.hovered.bg_fill = w.hovered.weak_bg_fill;
    w.hovered.bg_stroke = Stroke::new(1.0, p.border);
    w.hovered.fg_stroke = Stroke::new(1.0, p.text);
    w.hovered.corner_radius = r;
    w.hovered.expansion = 0.0;
    w.active.weak_bg_fill = if dark { hex(0x403f3c) } else { hex(0xdcdcd7) };
    w.active.bg_fill = w.active.weak_bg_fill;
    w.active.fg_stroke = Stroke::new(1.0, p.text);
    w.active.corner_radius = r;
    w.active.expansion = 0.0;
    w.open = w.active;
    v
}

/// Both themes, fonts sizes and spacing; which theme shows is ctx.set_theme(...).
pub fn setup(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_visuals_of(egui::Theme::Dark, visuals(true));
    ctx.set_visuals_of(egui::Theme::Light, visuals(false));
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Heading, FontId::new(24.0, medium())),
            (TextStyle::Monospace, FontId::new(12.5, FontFamily::Monospace)),
        ]
        .into();
        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(12.0, 6.0);
        s.spacing.interact_size.y = 30.0;
        s.spacing.combo_height = 300.0;
        s.visuals.slider_trailing_fill = true;
    });
}

// ----------------------------------------------------------------- text
pub fn title(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(FontId::new(26.0, medium())).color(pal(ui).text));
}

pub fn h2(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).font(FontId::new(16.0, medium())).color(pal(ui).text));
}

pub fn note(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    let c = pal(ui).text2;
    ui.add(egui::Label::new(RichText::new(text.into()).color(c)).wrap())
}

pub fn small(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    let c = pal(ui).text2;
    ui.label(RichText::new(text.into()).size(12.0).color(c))
}

/// A number with its unit and a caption under it. One widget of a known size, so a wrapping row
/// of them moves a whole stat to the next line instead of squeezing it at the edge.
pub fn stat(ui: &mut egui::Ui, value: &str, unit: &str, caption: &str) {
    let p = pal(ui);
    let pt = ui.painter().clone();
    let val = pt.layout_no_wrap(value.to_string(), FontId::new(24.0, medium()), p.text);
    let unit = (!unit.is_empty() && value != "–").then(|| pt.layout_no_wrap(unit.to_string(), FontId::proportional(13.0), p.text2));
    let cap = pt.layout_no_wrap(caption.to_string(), FontId::proportional(12.0), p.text2);
    let top = val.size().x + unit.as_ref().map_or(0.0, |u| 3.0 + u.size().x);
    let (r, _) = ui.allocate_exact_size(Vec2::new(top.max(cap.size().x), val.size().y + cap.size().y), egui::Sense::hover());
    let (vw, vh) = (val.size().x, val.size().y);
    pt.galley(r.min, val, p.text);
    if let Some(u) = unit {
        pt.galley(r.min + Vec2::new(vw + 3.0, (vh - u.size().y) / 2.0), u, p.text2);
    }
    pt.galley(r.min + Vec2::new(0.0, vh), cap, p.text2);
}

/// A settings row: its name in a fixed column, then the controls, which wrap in a narrow window.
pub fn field(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    let p = pal(ui);
    ui.horizontal_top(|ui| {
        let (r, _) = ui.allocate_exact_size(Vec2::new(120.0, ui.spacing().interact_size.y), egui::Sense::hover());
        ui.painter().text(r.left_center(), egui::Align2::LEFT_CENTER, label, FontId::proportional(14.0), p.text2);
        ui.vertical(|ui| ui.horizontal_wrapped(add));
    });
}

// ----------------------------------------------------------------- status
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Good,
    Warn,
    Bad,
    Off,
}

pub fn level_color(p: &Pal, l: Level) -> Color32 {
    match l {
        Level::Good => p.good,
        Level::Warn => p.warn,
        Level::Bad => p.critical,
        Level::Off => p.muted,
    }
}

pub fn level_icon(l: Level) -> &'static str {
    match l {
        Level::Good => "✔",
        Level::Warn => "⚠",
        Level::Bad => "✖",
        Level::Off => "○",
    }
}

/// Icon + text; the icon carries the colour, the words stay in text ink.
pub fn status(ui: &mut egui::Ui, l: Level, text: impl Into<String>) -> egui::Response {
    let p = pal(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new(level_icon(l)).color(level_color(&p, l)).strong());
        ui.add(egui::Label::new(RichText::new(text.into()).color(p.text)).wrap());
    })
    .response
}

/// A status in a word, for tight places: the coloured icon and a short label, as one widget
/// `height` tall (so it lines up with the buttons beside it).
pub fn chip(ui: &mut egui::Ui, l: Level, text: &str, height: f32) -> egui::Response {
    let p = pal(ui);
    let pt = ui.painter().clone();
    let icon = pt.layout_no_wrap(level_icon(l).to_string(), FontId::proportional(13.0), level_color(&p, l));
    let label = pt.layout_no_wrap(text.to_string(), FontId::proportional(12.5), p.text2);
    let (r, resp) = ui.allocate_exact_size(Vec2::new(icon.size().x + 5.0 + label.size().x, height), egui::Sense::click());
    let iw = icon.size().x;
    pt.galley(egui::pos2(r.left(), r.center().y - icon.size().y / 2.0), icon, p.text2);
    pt.galley(egui::pos2(r.left() + iw + 5.0, r.center().y - label.size().y / 2.0), label, p.text2);
    resp
}

// ----------------------------------------------------------------- containers and buttons
pub fn panel(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let p = pal(ui);
    egui::Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        });
}

pub fn primary(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    let p = pal(ui);
    let b = egui::Button::new(RichText::new(text).color(Color32::WHITE).font(FontId::new(14.5, medium())))
        .fill(if enabled { p.accent } else { p.muted })
        .corner_radius(CornerRadius::same(8))
        .min_size(Vec2::new(0.0, 36.0));
    ui.add_enabled(enabled, b)
}

pub fn secondary(ui: &mut egui::Ui, text: &str, enabled: bool) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(text).min_size(Vec2::new(0.0, 36.0)).corner_radius(CornerRadius::same(8)))
}

/// Segmented choice (a row of joined toggle buttons).
pub fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let p = pal(ui);
    let mut changed = false;
    egui::Frame::new().fill(ui.visuals().widgets.inactive.weak_bg_fill).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(3)).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (v, label) in options {
                let on = *value == *v;
                let b = egui::Button::new(RichText::new(*label).color(p.text).font(FontId::new(13.5, if on { medium() } else { FontFamily::Proportional })))
                    .fill(if on { p.panel } else { Color32::TRANSPARENT })
                    .stroke(if on { Stroke::new(1.0, p.border) } else { Stroke::NONE })
                    .corner_radius(CornerRadius::same(6))
                    .min_size(Vec2::new(44.0, 28.0));
                if ui.add(b).clicked() && !on {
                    *value = *v;
                    changed = true;
                }
            }
        });
    });
    changed
}

/// Tool output: monospace, selectable, sticks to the bottom as lines arrive. Up to `height` tall
/// even low on the page (egui would otherwise squeeze it to the space left in the window);
/// shorter text shrinks to fit.
pub fn log_view(ui: &mut egui::Ui, id: &str, lines: &[String], height: f32) {
    let p = pal(ui);
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::ScrollArea::both().id_salt(id).max_height(height).min_scrolled_height(height).auto_shrink([false, true]).stick_to_bottom(true).show(ui, |ui| {
                if lines.is_empty() {
                    ui.label(RichText::new("Nothing here yet.").color(p.muted));
                }
                let text = lines.join("\n");
                ui.add(egui::Label::new(RichText::new(text).monospace().color(p.text2)).extend().selectable(true));
            });
        });
}

pub fn text_view(ui: &mut egui::Ui, id: &str, text: &str, height: f32) {
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    log_view(ui, id, &lines, height);
}
