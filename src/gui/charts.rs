//! Chart plumbing shared by the pages: a quiet plot style, clock-time axes, hover labels.

use super::theme::{Pal, pal};
use eframe::egui::{self, Color32, RichText};
use egui_plot::{GridMark, HoverPosition, Plot, PlotPoints};

/// Plot with the house style: pan/zoom on x only (so the page still scrolls), recessive grid.
pub fn base<'a>(p: &Pal, id: &str, height: f32) -> Plot<'a> {
    Plot::new(id)
        .height(height)
        .grid_color(if p.dark { Color32::from_gray(78) } else { Color32::from_gray(200) })
        .grid_spacing(egui::Rangef::new(18.0, 140.0))
        .allow_scroll(false)
        .allow_zoom([true, false])
        .allow_drag([true, false])
        .allow_boxed_zoom(false)
        .y_axis_min_width(36.0)
        .set_margin_fraction(egui::vec2(0.01, 0.08))
}

#[derive(Clone, Copy)]
pub enum Mark {
    Line,
    Dashed,
    Dot,
    Ring,
    Box,
    Up,
    Down,
}

/// The legend, as a row of keys above the chart (so it never covers data).
pub fn key(ui: &mut egui::Ui, items: &[(Color32, &str, Mark)]) {
    let p = pal(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 16.0;
        for (c, label, m) in items {
            // one widget per key (mark + label), so a key that does not fit moves to the next line whole
            let text = ui.painter().layout_no_wrap(label.to_string(), egui::FontId::proportional(12.5), p.text2);
            let (all, _) = ui.allocate_exact_size(egui::vec2(24.0 + text.size().x, text.size().y.max(12.0)), egui::Sense::hover());
            {
                let r = egui::Rect::from_min_size(egui::pos2(all.left(), all.center().y - 6.0), egui::vec2(18.0, 12.0));
                let pt = ui.painter();
                let cy = r.center().y;
                match m {
                    Mark::Line => {
                        pt.line_segment([egui::pos2(r.left(), cy), egui::pos2(r.right(), cy)], egui::Stroke::new(2.5, *c));
                    }
                    Mark::Dashed => {
                        for i in 0..3 {
                            let x = r.left() + i as f32 * 7.0;
                            pt.line_segment([egui::pos2(x, cy), egui::pos2(x + 4.0, cy)], egui::Stroke::new(1.5, *c));
                        }
                    }
                    Mark::Dot => {
                        pt.circle_filled(r.center(), 4.0, *c);
                    }
                    Mark::Ring => {
                        pt.circle_stroke(r.center(), 3.5, egui::Stroke::new(1.3, *c));
                    }
                    Mark::Box => {
                        pt.rect_filled(egui::Rect::from_center_size(r.center(), egui::vec2(14.0, 10.0)), 2.0, *c);
                    }
                    Mark::Up | Mark::Down => {
                        let s = if matches!(m, Mark::Up) { -1.0 } else { 1.0 };
                        let q = r.center();
                        pt.add(egui::Shape::convex_polygon(
                            vec![egui::pos2(q.x - 5.0, q.y - 4.0 * s), egui::pos2(q.x + 5.0, q.y - 4.0 * s), egui::pos2(q.x, q.y + 4.0 * s)],
                            *c,
                            egui::Stroke::NONE,
                        ));
                    }
                }
                pt.galley(egui::pos2(all.left() + 24.0, all.center().y - text.size().y / 2.0), text, p.text2);
            }
        }
    });
}

/// x axis in seconds from `t0` (unix), labelled as wall-clock time.
pub fn clock_axis(t0: f64) -> impl Fn(GridMark, &std::ops::RangeInclusive<f64>) -> String {
    move |m, _| crate::night::clock(t0 + m.value, "%H:%M")
}

/// Hover text "<series>: y unit at hh:mm:ss" (or "at 12.3 s" when t0 is None).
pub fn hover(t0: Option<f64>, unit: &'static str, digits: usize) -> impl Fn(&HoverPosition<'_>) -> Option<String> {
    move |h| {
        let (name, p) = match h {
            HoverPosition::NearDataPoint { plot_name, position, .. } => (*plot_name, *position),
            HoverPosition::Elsewhere { position } => ("", *position),
        };
        let at = match t0 {
            Some(t0) => crate::night::clock(t0 + p.x, "%H:%M:%S"),
            None => format!("{:.1} s", p.x),
        };
        Some(if name.is_empty() { format!("{:.*} {unit}\nat {at}", digits, p.y) } else { format!("{name}\n{:.*} {unit} at {at}", digits, p.y) })
    }
}

pub fn pts(x: &[f64], y: &[f64]) -> PlotPoints<'static> {
    PlotPoints::from(x.iter().zip(y).filter(|(a, b)| a.is_finite() && b.is_finite()).map(|(a, b)| [*a, *b]).collect::<Vec<_>>())
}

/// Line points with NaN gaps kept as breaks: returns one run per finite stretch.
pub fn runs(x: &[f64], y: &[f64]) -> Vec<Vec<[f64; 2]>> {
    let mut out = vec![];
    let mut cur: Vec<[f64; 2]> = vec![];
    for (a, b) in x.iter().zip(y) {
        if a.is_finite() && b.is_finite() {
            cur.push([*a, *b]);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Chart heading: title plus an optional one-line explanation.
pub fn heading(ui: &mut egui::Ui, title: &str, sub: &str) {
    let p = pal(ui);
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(title).font(egui::FontId::new(15.0, super::theme::medium())).color(p.text));
        if !sub.is_empty() {
            ui.label(RichText::new(sub).size(12.5).color(p.text2));
        }
    });
}

pub fn alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

pub fn faint(p: &Pal) -> Color32 {
    alpha(p.text2, if p.dark { 120 } else { 110 })
}
