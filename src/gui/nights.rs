//! Nights: pick a recorded night, analyse it, and read it on one shared clock-time axis
//! (sleep stages, breathing spectrum, rate, regularity, variability), plus the Fitbit comparison.

use super::charts::{self, Mark, alpha, faint};
use super::nightview::NightView;
use super::theme::{self, Level, pal};
use super::{Job, Log, Shared};
use crate::night::{self, REG_MIN};
use eframe::egui::{self, Color32, ColorImage, RichText, TextureHandle, TextureOptions, Vec2};
use egui_plot::{HLine, Line, LineStyle, MarkerShape, PlotImage, PlotPoint, Points, Polygon};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Item {
    dir: PathBuf,
    day: String,
    start: String,
    hours: Option<f64>,
    parts: usize,
    view: bool,
    analysed: bool,
    fitbit: bool,
}

fn scan(results: &Path) -> Vec<Item> {
    night::list_nights(results)
        .into_iter()
        .map(|d| {
            let name = d.file_name().unwrap_or_default().to_string_lossy().to_string();
            let j: Option<serde_json::Value> = std::fs::read_to_string(d.join("night.json")).ok().and_then(|s| serde_json::from_str(&s).ok());
            let hours = j.as_ref().and_then(|j| Some((j["end_epoch"].as_f64().or(j["start_epoch"].as_f64())? - j["start_epoch"].as_f64()?) / 3600.0));
            let dt = chrono::NaiveDateTime::parse_from_str(name.trim_start_matches("night-"), "%Y%m%d-%H%M%S").ok();
            Item {
                day: dt.map(|d| d.format("%a %-d %b").to_string()).unwrap_or(name.clone()),
                start: dt.map(|d| d.format("%H:%M").to_string()).unwrap_or_default(),
                hours,
                parts: night::parts(&d).len(),
                view: d.join(super::nightview::FILE).exists(),
                analysed: d.join("night-summary.txt").exists(),
                fitbit: d.join("health-stages.csv").exists(),
                dir: d,
            }
        })
        .collect()
}

pub struct NightsPage {
    items: Vec<Item>,
    scanned: Option<Instant>,
    sel: Option<PathBuf>,
    view: Option<(PathBuf, NightView)>,
    tex: Option<(PathBuf, bool, TextureHandle)>,
    job: Option<(PathBuf, Job<Result<NightView, String>>)>,
    log: Log,
    refresh_health: bool,
    no_health: bool,
    error: Option<String>,
    show_summary: bool,
}

impl NightsPage {
    pub fn new(sh: &Shared) -> Self {
        let items = scan(&sh.set.results);
        let sel = items.first().map(|i| i.dir.clone());
        NightsPage {
            items,
            scanned: Some(Instant::now()),
            sel,
            view: None,
            tex: None,
            job: None,
            log: Log::default(),
            refresh_health: false,
            no_health: false,
            error: None,
            show_summary: false,
        }
    }

    pub fn demo_idle(&self) -> bool {
        self.job.is_none() && self.view.is_some()
    }
    pub fn demo_open(&mut self, ctx: &egui::Context, sh: &Shared, name: Option<String>) {
        let pick = self.items.iter().find(|i| name.as_ref().map_or(i.parts > 1, |n| i.dir.ends_with(n))).map(|i| i.dir.clone());
        if let Some(d) = pick {
            self.sel = Some(d.clone());
            if NightView::load(&d).is_none() {
                self.run(ctx, sh, d);
            }
        }
    }

    fn run(&mut self, ctx: &egui::Context, sh: &Shared, dir: PathBuf) {
        self.log.clear();
        self.error = None;
        let opt = night::Options { epoch_min: sh.set.epoch_min as f64, health: !self.no_health, refresh_health: self.refresh_health, ..Default::default() };
        let sink = self.log.sink(ctx);
        let d = dir.clone();
        self.job = Some((
            dir,
            Job::spawn(ctx, move || {
                let sink = move |s: String| {
                    if !(s.starts_with("skipped ") && s.contains("malformed")) {
                        sink(s)
                    }
                };
                let n = night::analyse(&d, &opt, &sink).map_err(|e| e.to_string())?;
                let v = NightView::from(&n);
                v.save(&d).map_err(|e| e.to_string())?;
                Ok(v)
            }),
        ));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, sh: &mut Shared) {
        if self.scanned.is_none_or(|t| t.elapsed() > Duration::from_secs(15)) {
            self.items = scan(&sh.set.results);
            self.scanned = Some(Instant::now());
        }
        if let Some((d, j)) = &self.job {
            if let Some(r) = j.poll() {
                match r {
                    Ok(v) => {
                        self.view = Some((d.clone(), v));
                        self.tex = None;
                    }
                    Err(e) => self.error = Some(e),
                }
                self.job = None;
                self.items = scan(&sh.set.results);
            }
        }
        if let Some(sel) = &self.sel {
            if self.view.as_ref().is_none_or(|v| &v.0 != sel) && self.job.is_none() {
                self.view = NightView::load(sel).map(|v| (sel.clone(), v));
                self.tex = None;
            }
        }
        theme::title(ui, "Nights");
        ui.add_space(2.0);
        theme::note(ui, "Breathing through the night as the WiFi measured it, next to the sleep stages from your Fitbit.");
        ui.add_space(18.0);
        let side = 250.0;
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(Vec2::new(side, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(side);
                self.list_ui(ui, sh)
            });
            ui.add_space(28.0);
            let w = ui.available_width();
            ui.allocate_ui_with_layout(Vec2::new(w, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(w);
                self.main_ui(ui, sh)
            });
        });
    }

    fn list_ui(&mut self, ui: &mut egui::Ui, sh: &mut Shared) {
        let p = pal(ui);
        if self.items.is_empty() {
            theme::note(ui, "No recorded nights yet. Start one on the Night recording page.");
            return;
        }
        let mut pick = None;
        for (i, it) in self.items.iter().enumerate() {
            let on = self.sel.as_ref() == Some(&it.dir);
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 50.0), egui::Sense::click());
            let fill = if on {
                p.accent_soft
            } else if resp.hovered() {
                ui.visuals().widgets.hovered.weak_bg_fill
            } else if i % 2 == 1 {
                ui.visuals().faint_bg_color
            } else {
                Color32::TRANSPARENT
            };
            ui.painter().rect_filled(rect, 6.0, fill);
            let pt = ui.painter();
            pt.text(rect.left_top() + Vec2::new(10.0, 7.0), egui::Align2::LEFT_TOP, &it.day, egui::FontId::new(14.0, theme::medium()), p.text);
            let hrs = it.hours.map(|h| format!("{h:.1} h")).unwrap_or_else(|| format!("{} parts", it.parts));
            pt.text(rect.left_top() + Vec2::new(10.0, 28.0), egui::Align2::LEFT_TOP, format!("from {}, {hrs}", it.start), egui::FontId::proportional(12.0), p.text2);
            let tag = if it.view || it.analysed { if it.fitbit { "analysed, Fitbit" } else { "analysed" } } else { "not analysed" };
            pt.text(rect.right_top() + Vec2::new(-10.0, 9.0), egui::Align2::RIGHT_TOP, tag, egui::FontId::proportional(11.5), if it.view || it.analysed { p.text2 } else { p.muted });
            if resp.clicked() {
                pick = Some(it.dir.clone());
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
        if let Some(d) = pick {
            self.sel = Some(d);
            self.error = None;
        }
        ui.add_space(14.0);
        if theme::secondary(ui, "Open results folder", true).clicked() {
            let _ = open::that(sh.results());
        }
    }

    fn main_ui(&mut self, ui: &mut egui::Ui, sh: &mut Shared) {
        let p = pal(ui);
        let Some(sel) = self.sel.clone() else { return };
        let item = self.items.iter().find(|i| i.dir == sel).map(|i| (i.parts, i.analysed));
        let running = self.job.as_ref().is_some_and(|j| j.0 == sel);
        // controls
        ui.horizontal_wrapped(|ui| {
            let label = if self.view.is_some() || item.is_some_and(|i| i.1) { "Analyse again" } else { "Analyse night" };
            if theme::primary(ui, if running { "Analysing ..." } else { label }, self.job.is_none() && item.is_some_and(|i| i.0 > 0)).clicked() {
                self.run(ui.ctx(), sh, sel.clone());
            }
            ui.add_space(6.0);
            ui.label(RichText::new("Variability epoch").color(p.text2));
            theme::segmented(ui, &mut sh.set.epoch_min, &[(2, "2 min"), (5, "5 min"), (10, "10 min")]);
            ui.add_space(6.0);
            ui.checkbox(&mut self.no_health, "Skip Fitbit");
            ui.add_enabled(!self.no_health, egui::Checkbox::new(&mut self.refresh_health, "Fetch Fitbit data again"));
            if theme::secondary(ui, "Open folder", true).clicked() {
                let _ = open::that(&sel);
            }
        });
        if let Some(e) = &self.error {
            theme::status(ui, Level::Bad, format!("The analysis failed: {e}"));
        }
        ui.add_space(12.0);
        if running {
            ui.horizontal(|ui| {
                ui.spinner();
                theme::note(ui, "Reading every part, finding movements and breaths. This takes about ten seconds per night.");
            });
            theme::log_view(ui, "night-log", &self.log.lines(), 360.0);
            return;
        }
        let Some((_, v)) = &self.view else {
            if item.is_some_and(|i| i.0 == 0) {
                theme::panel(ui, |ui| {
                    theme::h2(ui, "This night has no recorded parts");
                    theme::note(ui, "The recording stopped before the first part was saved.");
                });
            } else {
                theme::panel(ui, |ui| {
                    theme::h2(ui, "Not analysed in this app yet");
                    theme::note(ui, "Press Analyse night to see the charts. It fetches the matching Fitbit sleep if you are logged in (Fitbit page).");
                });
                if let Ok(s) = std::fs::read_to_string(sel.join("night-summary.txt")) {
                    ui.add_space(12.0);
                    theme::h2(ui, "Summary from an earlier analysis");
                    theme::text_view(ui, "old-summary", &s, (ui.clip_rect().height() - 90.0).max(400.0));
                }
            }
            if !self.log.lines().is_empty() {
                theme::log_view(ui, "night-log", &self.log.lines(), 240.0);
            }
            return;
        };
        let dark = ui.visuals().dark_mode;
        if self.tex.as_ref().is_none_or(|t| t.0 != sel || t.1 != dark) {
            self.tex = Some((sel.clone(), dark, spectrogram_texture(ui.ctx(), v, &p)));
        }
        let tex = self.tex.as_ref().unwrap().2.id();
        night_ui(ui, v, tex, &mut self.show_summary);
    }
}

fn spectrogram_texture(ctx: &egui::Context, v: &NightView, p: &theme::Pal) -> TextureHandle {
    let (nt, nb) = (v.spec_nt.max(1), v.spec_bpm.2.max(1));
    let mut img = ColorImage::filled([nt, nb], p.panel);
    for t in 0..v.spec_nt {
        for i in 0..nb {
            let val = v.spec[t * nb + i] as f64 / 255.0;
            let c = theme::ramp(val);
            let k = (val * 2.2).min(1.0) as f32;
            let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * k).round() as u8;
            // row 0 is the top of the image = highest BPM
            img.pixels[(nb - 1 - i) * nt + t] = Color32::from_rgb(mix(p.panel.r(), c.r()), mix(p.panel.g(), c.g()), mix(p.panel.b(), c.b()));
        }
    }
    ctx.load_texture("spectrogram", img, TextureOptions::LINEAR)
}

const LINK: &str = "night-x";

fn stage_level(s: &str) -> f64 {
    match s {
        "wake" => 3.0,
        "rem" => 2.0,
        "light" => 1.0,
        _ => 0.0,
    }
}

pub fn stage_color(s: &str) -> Color32 {
    // sleep depth is ordinal: lighter = shallower
    theme::ramp(match s {
        "wake" => 0.18,
        "rem" => 0.42,
        "light" => 0.62,
        _ => 0.95,
    })
}

fn stage_name(s: &str) -> &str {
    match s {
        "wake" => "Awake",
        "rem" => "REM",
        "light" => "Light",
        "deep" => "Deep",
        _ => s,
    }
}

fn night_ui(ui: &mut egui::Ui, v: &NightView, tex: egui::TextureId, show_summary: &mut bool) {
    let p = pal(ui);
    let t0 = v.t0;
    ui.label(RichText::new(v.span_text()).font(egui::FontId::new(20.0, theme::medium())).color(p.text));
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 38.0;
        theme::stat(ui, &format!("{:.1}", v.dur / 3600.0), "h", "recorded");
        theme::stat(ui, &format!("{:.1}", v.median_bpm()), "BPM", "median breathing rate");
        theme::stat(ui, &format!("{:.0}", v.steady_pct()), "%", "of the night breathing steadily");
        theme::stat(ui, &v.moves.len().to_string(), "", "movements");
        theme::stat(ui, &format!("{:.0}", 100.0 * v.breath_t.len() as f64 / v.breaths_total.max(1) as f64), "%", "of breaths usable");
        if let Some((fit, wifi)) = v.whole {
            theme::stat(ui, &format!("{wifi:.1} / {fit:.1}"), "BPM", "WiFi / Fitbit, whole night");
        }
    });
    ui.add_space(14.0);
    let xmin = 0.0;
    let xmax = v.dur;
    let base = |id: &str, h: f32| {
        charts::base(&p, id, h)
            .link_axis(LINK, [true, false])
            .link_cursor(LINK, [true, false])
            .x_axis_formatter(charts::clock_axis(t0))
            .default_x_bounds(xmin, xmax)
    };
    theme::panel(ui, |ui| {
        ui.horizontal(|ui| {
            theme::small(ui, "Drag or scroll with Ctrl to zoom the time axis; all charts follow. Double-click to reset.");
        });
        if !v.stages.is_empty() || !v.moves.is_empty() {
            charts::heading(
                ui,
                if v.stages.is_empty() { "Movement" } else { "Sleep stages and movement" },
                if v.stages.is_empty() { "" } else { "stages and restless marks from Fitbit, movements from the WiFi" },
            );
            let has_stages = !v.stages.is_empty();
            let mut k = vec![];
            if has_stages {
                for st in ["wake", "rem", "light", "deep"] {
                    k.push((stage_color(st), stage_name(st), Mark::Box));
                }
                if !v.restless.is_empty() {
                    k.push((p.text2, "Restless (Fitbit)", Mark::Down));
                }
            }
            k.push((p.cat[1], "Movement (WiFi)", Mark::Up));
            charts::key(ui, &k);
            base("stages", if has_stages { 150.0 } else { 70.0 })
                .show_grid([true, false])
                .y_axis_formatter(|m, _| match m.value.round() as i32 {
                    3 => "Awake".into(),
                    2 => "REM".into(),
                    1 => "Light".into(),
                    0 => "Deep".into(),
                    _ => String::new(),
                })
                .y_grid_spacer(|_| (0..4).map(|i| egui_plot::GridMark { value: i as f64, step_size: 1.0 }).collect())
                .label_formatter(move |h| match h {
                    egui_plot::HoverPosition::NearDataPoint { plot_name, position, .. } => Some(format!("{plot_name}\n{}", night::clock(t0 + position.x, "%H:%M:%S"))),
                    egui_plot::HoverPosition::Elsewhere { position } => Some(night::clock(t0 + position.x, "%H:%M")),
                })
                .include_y(-1.0)
                .include_y(if has_stages { 4.0 } else { 0.0 })
                .show(ui, |pu| {
                    let mut named = std::collections::HashSet::new();
                    for (a, b, s) in &v.stages {
                        let y = stage_level(s);
                        let name = if named.insert(s.clone()) { stage_name(s).to_string() } else { String::new() };
                        let c = stage_color(s);
                        pu.polygon(
                            Polygon::new(name, vec![[*a, y - 0.42], [*b, y - 0.42], [*b, y + 0.42], [*a, y + 0.42]])
                                .fill_color(c)
                                .stroke(egui::Stroke::new(0.0, c))
                                .allow_hover(false),
                        );
                    }
                    if !v.restless.is_empty() {
                        let pts: Vec<[f64; 2]> = v.restless.iter().map(|t| [*t, 3.75]).collect();
                        pu.points(Points::new("Restless (Fitbit)", pts).shape(MarkerShape::Down).radius(4.5).color(p.text2).filled(true));
                    }
                    let mv: Vec<[f64; 2]> = v.moves.iter().map(|m| [m.0, -0.75]).collect();
                    pu.points(Points::new("Movement (WiFi)", mv).shape(MarkerShape::Up).radius(4.5).color(p.cat[1]).filled(true));
                });
        }

        charts::heading(ui, "Breathing spectrum", "how strong each breathing rate is through the night");
        charts::key(ui, &[(theme::ramp(0.9), "Share of the signal (darker = more)", Mark::Box), (p.cat[1], "Tracked rate, steady", Mark::Line), (alpha(p.cat[1], 110), "Tracked rate", Mark::Line)]);
        let (lo, hi, _) = v.spec_bpm;
        let x0 = v.track_t.first().copied().unwrap_or(0.0) - v.spec_dt / 2.0;
        let w = v.spec_nt as f64 * v.spec_dt;
        base("spec", 230.0)
            .y_axis_label("BPM")
            .include_y(lo)
            .include_y(hi)
            .show_grid([false, false])
            .label_formatter(charts::hover(Some(t0), "BPM", 1))
            .show(ui, |pu| {
                pu.image(PlotImage::new("", tex, PlotPoint::new(x0 + w / 2.0, (lo + hi) / 2.0), Vec2::new(w as f32, (hi - lo) as f32)).allow_hover(false));
                let steady: Vec<[f64; 2]> = v.track_t.iter().zip(&v.track_bpm).zip(&v.reg2).map(|((t, b), r)| [*t, if *r >= REG_MIN { *b } else { f64::NAN }]).collect();
                let all: Vec<[f64; 2]> = v.track_t.iter().zip(&v.track_bpm).map(|(t, b)| [*t, *b]).collect();
                pu.line(Line::new("Tracked rate", all).color(alpha(p.cat[1], 110)).width(1.0));
                let xs: Vec<f64> = steady.iter().map(|s| s[0]).collect();
                let ys: Vec<f64> = steady.iter().map(|s| s[1]).collect();
                for (i, r) in charts::runs(&xs, &ys).into_iter().enumerate() {
                    pu.line(Line::new(if i == 0 { "Tracked rate, steady" } else { "" }, r).color(p.cat[1]).width(2.0));
                }
            });

        charts::heading(ui, "Breathing rate", "every usable breath, and the median per epoch");
        charts::key(ui, &[(faint(&p), "Single breaths", Mark::Dot), (p.cat[0], "Epoch median", Mark::Line)]);
        let ep_t: Vec<f64> = v.epochs.iter().map(|e| e.t + v.epoch_min * 30.0).collect();
        base("rate", 210.0)
            .y_axis_label("BPM")
            .label_formatter(charts::hover(Some(t0), "BPM", 1))
            .show(ui, |pu| {
                let pts: Vec<[f64; 2]> = v.breath_t.iter().zip(&v.breath_bpm).filter(|(_, b)| **b < 40.0).map(|(t, b)| [*t, *b]).collect();
                pu.points(Points::new("Single breaths", pts).color(faint(&p)).radius(1.3));
                let r: Vec<f64> = v.epochs.iter().map(|e| e.rate).collect();
                for (i, run) in charts::runs(&ep_t, &r).into_iter().enumerate() {
                    pu.line(Line::new(if i == 0 { "Epoch median" } else { "" }, run).color(p.cat[0]).width(2.2));
                }
            });

        charts::heading(ui, "Regularity", "share of the spectrum at the tracked rate, 2 min average");
        charts::key(ui, &[(p.cat[0], "Regularity", Mark::Line), (p.text2, "Steady from here", Mark::Dashed)]);
        base("reg", 160.0)
            .include_y(0.0)
            .include_y(1.0)
            .label_formatter(charts::hover(Some(t0), "", 2))
            .show(ui, |pu| {
                pu.line(Line::new("Regularity", charts::pts(&v.track_t, &v.reg2)).color(p.cat[0]).width(1.8));
                pu.hline(HLine::new("Steady from here", REG_MIN).color(p.text2).style(LineStyle::dashed_loose()).width(1.0));
            });

        let good = |f: fn(&super::nightview::EpochV) -> f64| -> (Vec<[f64; 2]>, Vec<[f64; 2]>) {
            let mut a = vec![];
            let mut b = vec![];
            for (e, t) in v.epochs.iter().zip(&ep_t) {
                let y = f(e);
                if y.is_finite() {
                    if e.good { a.push([*t, y]) } else { b.push([*t, y]) }
                }
            }
            (a, b)
        };
        charts::heading(ui, "Variability", "coefficient of variation of breath length per epoch");
        let ep_key = [(p.cat[0], "Reliable epoch", Mark::Dot), (faint(&p), "Too few usable breaths", Mark::Ring)];
        charts::key(ui, &ep_key);
        let (a, b) = good(|e| e.cv);
        base("cv", 160.0).y_axis_label("%").include_y(0.0).label_formatter(charts::hover(Some(t0), "%", 1)).show(ui, |pu| {
            pu.line(Line::new("", a.clone()).color(alpha(p.cat[0], 90)).width(1.0).allow_hover(false));
            pu.points(Points::new("Reliable epoch", a).color(p.cat[0]).radius(3.0));
            pu.points(Points::new("Too few usable breaths", b).color(faint(&p)).radius(2.5).filled(false));
        });
        charts::heading(ui, "Breath-to-breath change", "RMSSD of breath length per epoch");
        charts::key(ui, &ep_key);
        let (a, b) = good(|e| e.rmssd);
        base("rmssd", 160.0).y_axis_label("s").include_y(0.0).label_formatter(charts::hover(Some(t0), "s", 2)).show(ui, |pu| {
            pu.line(Line::new("", a.clone()).color(alpha(p.cat[0], 90)).width(1.0).allow_hover(false));
            pu.points(Points::new("Reliable epoch", a).color(p.cat[0]).radius(3.0));
            pu.points(Points::new("Too few usable breaths", b).color(faint(&p)).radius(2.5).filled(false));
        });
    });

    if !v.rows.is_empty() {
        ui.add_space(20.0);
        theme::h2(ui, "By sleep stage");
        theme::small(ui, format!("Stages from {}", if v.source.is_empty() { "the wearable" } else { &v.source }));
        ui.add_space(6.0);
        theme::panel(ui, |ui| {
            let f = |x: f64, d: usize| if x.is_finite() { format!("{x:.d$}") } else { "–".into() };
            egui::Grid::new("stages-table").striped(true).num_columns(9).spacing([22.0, 8.0]).min_col_width(40.0).show(ui, |ui| {
                for h in ["Stage", "Minutes", "Regularity", "Steady", "WiFi rate", "Fitbit rate", "Usable breaths", "RMSSD", "Movements"] {
                    ui.label(RichText::new(h).size(12.5).color(p.text2));
                }
                ui.end_row();
                for r in &v.rows {
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(11.0), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 3.0, stage_color(&r.stage));
                        ui.label(RichText::new(stage_name(&r.stage)).color(p.text));
                    });
                    ui.label(f(r.minutes, 0));
                    ui.label(f(r.regularity, 2));
                    ui.label(format!("{} %", f(r.steady_pct, 0)));
                    ui.label(format!("{} BPM", f(r.breath_bpm, 1)));
                    ui.label(r.wearable.map(|w| format!("{:.1} ± {:.1} BPM", w.0, w.1)).unwrap_or("–".into()));
                    ui.label(format!("{} %", f(r.usable_pct, 0)));
                    ui.label(format!("{} s", f(r.rmssd, 2)));
                    ui.label(format!("{} /h", f(r.moves_h, 1)));
                    ui.end_row();
                }
            });
            if let Some((d, pv)) = &v.deep_minus_rem {
                ui.add_space(10.0);
                let pv_txt = if pv.starts_with('<') { format!("p {pv}") } else { format!("p = {pv}") };
                theme::note(ui, format!("Breathing is more regular in deep sleep than in REM by {d:+.3} ({pv_txt}, stage labels shifted in time as the null)."));
            }
            if let Some(h) = v.restless_hits.iter().find(|h| h.0 == 3).or(v.restless_hits.first()) {
                theme::note(
                    ui,
                    format!("{:.0}% of the {} Fitbit restless marks have a WiFi movement within {} min (by chance: {:.0}%).", h.1, h.2, h.0, h.3),
                );
            }
        });
    }
    ui.add_space(16.0);
    let r = egui::CollapsingHeader::new(RichText::new("Full text summary").color(p.text)).open(Some(*show_summary)).show(ui, |ui| {
        let height = (ui.clip_rect().height() - 90.0).max(480.0);
        theme::text_view(ui, "summary", &v.summary, height);
    });
    if r.header_response.clicked() {
        *show_summary = !*show_summary;
    }
}
