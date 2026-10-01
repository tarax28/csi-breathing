//! Breath test: record the RX for a minute or two while the user taps every inhale, then
//! compare the WiFi estimate with the taps. While it runs, the page shows the breathing trace
//! the WiFi sees, live, with the taps on it.

use super::charts::{self, Mark, alpha, faint};
use super::theme::{self, Level, pal};
use super::{Job, Shared};
use crate::analysis::{self as an, FS, Report, TapReport};
use crate::hw;
use crate::session::{self, Past};
use eframe::egui::{self, Align, Color32, FontId, Key, Layout, Modifiers, RichText, Stroke, Vec2};
use egui_plot::{HLine, Line, LineStyle, MarkerShape, Points, VLine};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A test needs this many packets per second on average to be worth analysing.
const FS_MIN: f64 = 20.0;
const LIVE_S: f64 = 30.0;

pub fn nice_stamp(stamp: &str) -> String {
    chrono::NaiveDateTime::parse_from_str(stamp, "%Y%m%d-%H%M%S").map(|d| d.format("%-d %b %Y, %H:%M").to_string()).unwrap_or_else(|_| stamp.to_string())
}

// ----------------------------------------------------------------- live capture state
#[derive(Default)]
struct Live {
    t0: Option<f64>,
    packets: usize,
    elapsed: f64,
    ring: VecDeque<(f64, Vec<u8>)>,
    /// (seconds since start, normalised waveform)
    wave: Vec<[f64; 2]>,
    bpm: Option<(f64, f64)>,
}

struct Run {
    stamp: String,
    secs: f64,
    stop: Arc<AtomicBool>,
    live: Arc<Mutex<Live>>,
    job: Job<Result<(Vec<i64>, Vec<f64>, f64), String>>,
    taps: Vec<f64>,
    last_tap: Option<Instant>,
}

struct Captured {
    stamp: String,
    secs: f64,
    seq: Vec<i64>,
    host: Vec<f64>,
    taps: Vec<f64>,
    count: u32,
    hr: u32,
}

enum Phase {
    Idle,
    Countdown { until: Instant, stamp: String, secs: f64, src: hw::Source },
    Running(Run),
    Count(Captured),
    Analysing(Job<Result<session::Outcome, String>>),
}

/// A test on screen (just analysed, or reloaded from results/).
pub struct Shown {
    pub stamp: String,
    pub secs: f64,
    pub reference: Option<f64>,
    pub report: Option<Report>,
    pub tap: Option<TapReport>,
    pub text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ResultTab {
    Rate,
    Trace,
    Shape,
    Report,
}

pub struct BreathPage {
    phase: Phase,
    error: Option<String>,
    history: Vec<Past>,
    shown: Option<Shown>,
    loading: Option<(String, Job<Result<Shown, String>>)>,
    tab: ResultTab,
}

impl BreathPage {
    pub fn new(sh: &Shared) -> Self {
        BreathPage { phase: Phase::Idle, error: None, history: session::history(&sh.set.results), shown: None, loading: None, tab: ResultTab::Rate }
    }

    pub fn demo_start(&mut self, sh: &mut Shared) {
        self.start(sh);
    }
    pub fn demo_elapsed(&self) -> Option<f64> {
        match &self.phase {
            Phase::Running(r) => {
                let l = r.live.lock().unwrap();
                l.t0.map(|_| l.elapsed)
            }
            _ => None,
        }
    }
    pub fn demo_tap(&mut self) {
        if let Phase::Running(r) = &mut self.phase {
            r.taps.push(hw::now());
            r.last_tap = Some(Instant::now());
        }
    }
    pub fn demo_idle(&self) -> bool {
        self.loading.is_none() && matches!(self.phase, Phase::Idle)
    }
    pub fn demo_open_past(&mut self, ctx: &egui::Context, sh: &Shared) {
        let results = sh.set.results.clone();
        let want = std::env::var("CSI_STAMP").ok();
        if let Some(h) = self.history.iter().find(|h| want.as_ref().map_or(session::stamp_path(&results, &h.stamp, ".json").exists(), |w| &h.stamp == w)).cloned() {
            self.loading = Some((h.stamp.clone(), Job::spawn(ctx, move || load_past(&results, &h))));
        }
        if let Ok(t) = std::env::var("CSI_TAB") {
            self.tab = match t.as_str() {
                "trace" => ResultTab::Trace,
                "shape" => ResultTab::Shape,
                "report" => ResultTab::Report,
                _ => ResultTab::Rate,
            };
        }
    }

    pub fn reload_history(&mut self, sh: &Shared) {
        self.history = session::history(&sh.set.results);
    }

    fn start(&mut self, sh: &mut Shared) {
        self.error = None;
        let src = if let Some(r) = &sh.replay {
            hw::Source::Replay(r.to_string_lossy().into())
        } else if let Some(msg) = sh.rx_busy() {
            self.error = Some(msg.into());
            return;
        } else if let Some(p) = sh.rx() {
            hw::Source::Serial(p.name.clone())
        } else {
            self.error = Some("The RX board is not plugged in. Connect it by USB (Boards page), or replay a recorded test (Tools page).".into());
            return;
        };
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        self.phase = Phase::Countdown { until: Instant::now() + Duration::from_secs(3), stamp, secs: sh.set.breath_secs as f64, src };
    }

    /// Advance the test state; runs every frame whichever page is showing.
    pub fn poll(&mut self, ctx: &egui::Context, sh: &mut Shared) {
        if let Some((_, j)) = &self.loading {
            if let Some(r) = j.poll() {
                match r {
                    Ok(s) => self.shown = Some(s),
                    Err(e) => self.error = Some(e),
                }
                self.loading = None;
            }
        }
        match std::mem::replace(&mut self.phase, Phase::Idle) {
            Phase::Countdown { until, stamp, secs, src } if Instant::now() >= until => {
                sh.capturing = !matches!(src, hw::Source::Replay(_));
                self.phase = Phase::Running(begin(ctx, sh, stamp, secs, src));
            }
            Phase::Countdown { until, stamp, secs, src } => {
                ctx.request_repaint_after(Duration::from_millis(100));
                self.phase = Phase::Countdown { until, stamp, secs, src };
            }
            Phase::Running(run) => match run.job.poll() {
                Some(res) => {
                    run.stop.store(true, Ordering::SeqCst);
                    sh.capturing = false;
                    let log = session::stamp_path(&sh.results(), &run.stamp, ".log");
                    match res {
                        Err(e) => {
                            self.error = Some(format!("The capture stopped: {e}"));
                            let _ = std::fs::remove_file(log);
                        }
                        Ok((seq, host, secs)) => {
                            if (seq.len() as f64) < FS_MIN * 20.0 {
                                let _ = std::fs::remove_file(log);
                                self.error = Some(format!(
                                    "Only {} CSI packets arrived, not enough to analyse. Is the TX powered and within range? Check the link on the Boards page.",
                                    seq.len()
                                ));
                            } else {
                                let n = run.taps.len() as u32;
                                self.phase = Phase::Count(Captured { stamp: run.stamp, secs, seq, host, taps: run.taps, count: n, hr: 0 });
                            }
                        }
                    }
                }
                None => {
                    ctx.request_repaint_after(Duration::from_millis(33));
                    self.phase = Phase::Running(run);
                }
            },
            Phase::Analysing(job) => match job.poll() {
                Some(Ok(o)) => {
                    self.reload_history(sh);
                    self.shown = Some(Shown {
                        stamp: o.stamp,
                        secs: o.secs,
                        reference: Some(o.reference),
                        report: o.report,
                        tap: o.tap,
                        text: o.text,
                    });
                    self.tab = ResultTab::Rate;
                }
                Some(Err(e)) => self.error = Some(format!("The analysis failed: {e}")),
                None => self.phase = Phase::Analysing(job),
            },
            other => self.phase = other,
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, sh: &mut Shared) {
        match &self.phase {
            Phase::Countdown { until, .. } => {
                let left = until.saturating_duration_since(Instant::now()).as_secs_f64().ceil().max(1.0);
                countdown_ui(ui, left);
                if theme::secondary(ui, "Cancel", true).clicked() {
                    self.phase = Phase::Idle;
                }
                return;
            }
            Phase::Running(_) => {
                self.running_ui(ui, sh);
                return;
            }
            Phase::Count(_) | Phase::Analysing(_) => {
                self.count_ui(ui, sh);
                return;
            }
            Phase::Idle => {}
        }
        self.idle_ui(ui, sh);
    }

    fn idle_ui(&mut self, ui: &mut egui::Ui, sh: &mut Shared) {
        let p = pal(ui);
        theme::title(ui, "Breath test");
        ui.add_space(2.0);
        ui.set_max_width(ui.available_width());
        theme::note(ui, "Sit still and breathe normally. Tap Space at the start of every inhale; the app then compares what the WiFi saw with your taps.");
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            theme::segmented(ui, &mut sh.set.breath_secs, &[(60, "60 s"), (90, "90 s"), (120, "2 min"), (180, "3 min")]);
            ui.add_space(8.0);
            if theme::primary(ui, "Start test", true).clicked() {
                self.start(sh);
            }
            ui.add_space(8.0);
            match (&sh.replay, sh.rx()) {
                (Some(r), _) => theme::status(ui, Level::Warn, format!("Replays {} instead of the RX", r.file_name().unwrap_or_default().to_string_lossy())),
                (None, Some(rx)) => theme::status(ui, Level::Good, format!("Reads the RX on {}", rx.name)),
                (None, None) => theme::status(ui, Level::Bad, "RX not plugged in"),
            };
        });
        if let Some(e) = &self.error {
            ui.add_space(8.0);
            theme::status(ui, Level::Bad, e.clone());
        }
        ui.add_space(22.0);

        let wide = ui.available_width() > 1000.0;
        let side = 330.0;
        let body = |me: &mut Self, ui: &mut egui::Ui| {
            if let Some((stamp, _)) = &me.loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Loading {} ...", nice_stamp(stamp)));
                });
            } else if let Some(s) = &me.shown {
                result_ui(ui, s, &mut me.tab, "breath");
            } else {
                theme::panel(ui, |ui| {
                    theme::h2(ui, "No test on screen");
                    theme::note(ui, "Run a test, or pick one from the list of past tests.");
                });
            }
        };
        if wide {
            ui.horizontal_top(|ui| {
                let w = ui.available_width() - side - 28.0;
                ui.allocate_ui_with_layout(Vec2::new(w, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(w);
                    body(self, ui)
                });
                ui.add_space(28.0);
                ui.allocate_ui_with_layout(Vec2::new(side, 0.0), Layout::top_down(Align::Min), |ui| {
                    ui.set_width(side);
                    self.history_ui(ui, sh)
                });
            });
        } else {
            body(self, ui);
            ui.add_space(20.0);
            self.history_ui(ui, sh);
        }
        let _ = p;
    }

    fn history_ui(&mut self, ui: &mut egui::Ui, sh: &mut Shared) {
        let p = pal(ui);
        theme::h2(ui, "Past tests");
        theme::small(ui, "BPM you counted, and what the WiFi measured");
        ui.add_space(6.0);
        if self.history.is_empty() {
            theme::note(ui, "None yet.");
            return;
        }
        let sel = self.shown.as_ref().map(|s| s.stamp.clone()).or(self.loading.as_ref().map(|l| l.0.clone()));
        let mut pick = None;
        // No inner scroll area: the page scrolls, and a nested one gets squeezed inside the row.
        ui.scope(|ui| {
            for (i, h) in self.history.iter().enumerate() {
                let on = sel.as_deref() == Some(&h.stamp);
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), egui::Sense::click());
                let fill = if on {
                    p.accent_soft
                } else if resp.hovered() {
                    ui.visuals().widgets.hovered.weak_bg_fill
                } else if i % 2 == 0 {
                    Color32::TRANSPARENT
                } else {
                    ui.visuals().faint_bg_color
                };
                ui.painter().rect_filled(rect, 6.0, fill);
                let pt = ui.painter();
                pt.text(rect.left_top() + Vec2::new(10.0, 6.0), egui::Align2::LEFT_TOP, nice_stamp(&h.stamp), FontId::proportional(13.5), p.text);
                let secs = h.secs.trim_end_matches(".0");
                pt.text(rect.left_top() + Vec2::new(10.0, 25.0), egui::Align2::LEFT_TOP, format!("{secs} s"), FontId::proportional(11.5), p.text2);
                let rv = |v: &str| if v.is_empty() || v == "none" || v.parse::<f64>().is_ok_and(|x| x == 0.0) { "–".to_string() } else { v.to_string() };
                let right = format!("{}  /  {}", rv(&h.reference), rv(&h.csi));
                pt.text(rect.right_top() + Vec2::new(-10.0, 6.0), egui::Align2::RIGHT_TOP, right, FontId::new(13.5, theme::medium()), p.text);
                if !h.mae.is_empty() {
                    pt.text(rect.right_top() + Vec2::new(-10.0, 25.0), egui::Align2::RIGHT_TOP, format!("off by {} BPM", h.mae), FontId::proportional(11.5), p.text2);
                }
                if resp.clicked() {
                    pick = Some(h.clone());
                }
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
            }
        });
        if let Some(h) = pick {
            let results = sh.results();
            let stamp = h.stamp.clone();
            self.error = None;
            self.loading = Some((stamp.clone(), Job::spawn(ui.ctx(), move || load_past(&results, &h))));
        }
    }

    fn running_ui(&mut self, ui: &mut egui::Ui, sh: &mut Shared) {
        let p = pal(ui);
        let Phase::Running(run) = &mut self.phase else { return };
        let started = run.live.lock().unwrap().t0.is_some();
        if started && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Space)) {
            run.taps.push(hw::now());
            run.last_tap = Some(Instant::now());
        }
        let (el, packets, wave, bpm, t0) = {
            let l = run.live.lock().unwrap();
            (l.elapsed, l.packets, l.wave.clone(), l.bpm, l.t0)
        };
        let left = (run.secs - el).max(0.0).ceil();
        // header: time left, then the counts
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{left:.0}")).font(FontId::new(76.0, theme::bold())).color(p.text));
            ui.vertical(|ui| {
                ui.add_space(18.0);
                ui.label(RichText::new("seconds left").size(14.0).color(p.text2));
                ui.label(
                    RichText::new(if started { "Tap Space at the start of every inhale" } else { "Opening the RX ..." }).font(FontId::new(16.0, theme::medium())).color(p.text),
                );
            });
            // stats pinned to the right edge, laid out left to right inside a fixed box
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.allocate_ui_with_layout(Vec2::new(360.0, 80.0), Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 30.0;
                    theme::stat(ui, &run.taps.len().to_string(), "", "taps");
                    theme::stat(ui, &format!("{:.0}", packets as f64 / el.max(0.1)), "/s", "packets");
                    theme::stat(ui, &bpm.map(|b| format!("{:.1}", b.0)).unwrap_or("–".into()), "BPM", "live estimate, last 30 s");
                });
            });
        });
        if started && packets == 0 && el > 5.0 {
            theme::status(ui, Level::Bad, "No CSI data is arriving. Is the TX powered and within range?");
        }
        ui.add_space(10.0);
        // the live trace
        theme::panel(ui, |ui| {
            let lo = (el - LIVE_S).max(0.0);
            let hi = lo + LIVE_S;
            let taps: Vec<f64> = t0.map(|t0| run.taps.iter().map(|t| t - t0).filter(|t| *t >= lo).collect()).unwrap_or_default();
            charts::key(ui, &[(p.accent, "Breathing, as the WiFi sees it", Mark::Line), (p.cat[1], "Your taps", Mark::Line)]);
            charts::base(&p, "live", 300.0)
                .show_axes([true, false])
                .show_grid([true, false])
                .allow_drag(false)
                .allow_zoom(false)
                .show_x(false)
                .show_y(false)
                .x_axis_formatter(|m, _| format!("{:.0} s", m.value))
                .show(ui, |pu| {
                    pu.set_plot_bounds_x(lo..=hi);
                    pu.set_plot_bounds_y(-1.25..=1.25);
                    pu.line(Line::new("Breathing, as the WiFi sees it", wave).color(p.accent).width(2.5));
                    for (i, t) in taps.iter().enumerate() {
                        pu.vline(VLine::new(if i == 0 { "Your taps" } else { "" }, *t).color(p.cat[1]).width(1.5));
                    }
                });
            if wave_is_empty(&run.live) {
                theme::small(ui, "The trace appears after about ten seconds of data.");
            }
        });
        ui.add_space(18.0);
        // the tap target
        let mut cancel = false;
        ui.vertical_centered(|ui| {
            let d = 168.0;
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(d + 40.0), egui::Sense::click());
            let c = rect.center();
            let pressed = resp.is_pointer_button_down_on();
            let fill = if !started { p.muted } else if pressed { theme::pal(ui).cat[0].gamma_multiply(0.85) } else { p.accent };
            if let Some(t) = run.last_tap {
                let a = t.elapsed().as_secs_f32() / 0.45;
                if a < 1.0 {
                    ui.painter().circle_stroke(c, d / 2.0 + 18.0 * a, Stroke::new(3.0 * (1.0 - a), alpha(p.accent, (200.0 * (1.0 - a)) as u8)));
                    ui.ctx().request_repaint_after(Duration::from_millis(16));
                }
            }
            ui.painter().circle_filled(c, d / 2.0, fill);
            ui.painter().text(c - Vec2::new(0.0, 10.0), egui::Align2::CENTER_CENTER, "Inhale", FontId::new(26.0, theme::medium()), Color32::WHITE);
            ui.painter().text(c + Vec2::new(0.0, 20.0), egui::Align2::CENTER_CENTER, "or press Space", FontId::proportional(13.0), alpha(Color32::WHITE, 210));
            if started && resp.clicked() {
                run.taps.push(hw::now());
                run.last_tap = Some(Instant::now());
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            ui.add_space(6.0);
            theme::small(ui, "Keep the tapping hand still and out of the line between the boards.");
            ui.add_space(8.0);
            cancel = theme::secondary(ui, "Cancel test", true).clicked();
        });
        if cancel {
            run.stop.store(true, Ordering::SeqCst);
            let log = session::stamp_path(&sh.results(), &run.stamp, ".log");
            sh.capturing = false;
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                let _ = std::fs::remove_file(log);
            });
            self.phase = Phase::Idle;
        }
    }

    fn count_ui(&mut self, ui: &mut egui::Ui, sh: &mut Shared) {
        let p = pal(ui);
        let busy = matches!(self.phase, Phase::Analysing(_));
        theme::title(ui, if busy { "Analysing ..." } else { "Time's up" });
        ui.add_space(6.0);
        let mut go = false;
        let mut discard = false;
        if let Phase::Count(c) = &mut self.phase {
            theme::note(
                ui,
                format!("You tapped {} inhales in {:.0} s ({} packets). Correct the count if you missed some.", c.taps.len(), c.secs, c.seq.len()),
            );
            ui.add_space(16.0);
            theme::panel(ui, |ui| {
                egui::Grid::new("count").num_columns(2).spacing([24.0, 14.0]).show(ui, |ui| {
                    ui.label("Inhales counted");
                    ui.add(egui::DragValue::new(&mut c.count).range(0..=500).speed(0.2));
                    ui.end_row();
                    ui.label("Heart rate from a watch (optional)");
                    ui.add(egui::DragValue::new(&mut c.hr).range(0..=220).speed(0.3).custom_formatter(|v, _| if v == 0.0 { "–".into() } else { format!("{v:.0}") }));
                    ui.end_row();
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    go = theme::primary(ui, "Analyse", true).clicked() || ui.input(|i| i.key_pressed(Key::Enter));
                    discard = theme::secondary(ui, "Discard test", true).clicked();
                });
            });
        } else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Finding breaths, rates and how they line up with your taps").color(p.text2));
            });
        }
        if discard {
            if let Phase::Count(c) = &self.phase {
                let _ = std::fs::remove_file(session::stamp_path(&sh.results(), &c.stamp, ".log"));
            }
            self.phase = Phase::Idle;
        } else if go {
            if let Phase::Count(c) = std::mem::replace(&mut self.phase, Phase::Idle) {
                let results = sh.results();
                let hr = (c.hr > 0).then_some(c.hr as f64);
                self.phase = Phase::Analysing(Job::spawn(ui.ctx(), move || {
                    session::analyse(&results, &c.stamp, c.secs.round(), c.count as usize, &c.taps, &c.seq, &c.host, hr).map_err(|e| e.to_string())
                }));
            }
        }
    }
}

fn wave_is_empty(l: &Arc<Mutex<Live>>) -> bool {
    l.lock().unwrap().wave.is_empty()
}

fn countdown_ui(ui: &mut egui::Ui, left: f64) {
    let p = pal(ui);
    ui.add_space(80.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(format!("{left:.0}")).font(FontId::new(140.0, theme::bold())).color(p.accent));
        ui.label(RichText::new("Get ready. Sit still and breathe normally.").font(FontId::new(18.0, theme::medium())).color(p.text));
        ui.add_space(4.0);
        ui.label(RichText::new("Tap Space at the start of every inhale.").color(p.text2));
        ui.add_space(20.0);
    });
}

/// Start the capture thread and the live-trace thread.
fn begin(ctx: &egui::Context, sh: &Shared, stamp: String, secs: f64, src: hw::Source) -> Run {
    let path = session::stamp_path(&sh.results(), &stamp, ".log");
    let stop = Arc::new(AtomicBool::new(false));
    let live = Arc::new(Mutex::new(Live::default()));
    let (l1, l2, l3, s1, c1) = (live.clone(), live.clone(), live.clone(), stop.clone(), ctx.clone());
    let job = Job::spawn(ctx, move || {
        hw::capture(
            &src,
            secs,
            &path,
            &mut |t0| l1.lock().unwrap().t0 = Some(t0),
            &mut |n, el, _| {
                let mut l = l2.lock().unwrap();
                l.packets = n;
                l.elapsed = el;
                c1.request_repaint();
            },
            &mut |line, t| {
                let mut l = l3.lock().unwrap();
                l.ring.push_back((t, line.to_vec()));
                while l.ring.front().is_some_and(|f| t - f.0 > LIVE_S + 2.0) {
                    l.ring.pop_front();
                }
            },
            &|| s1.load(Ordering::SeqCst),
        )
        .map_err(|e| e.to_string())
    });
    let (l4, s2, c2) = (live.clone(), stop.clone(), ctx.clone());
    std::thread::spawn(move || {
        while !s2.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(600));
            let (t0, lines) = {
                let l = l4.lock().unwrap();
                (l.t0, l.ring.iter().map(|(t, b)| (*t, b.clone())).collect::<Vec<_>>())
            };
            let (Some(t0), Some(first)) = (t0, lines.first()) else { continue };
            if lines.last().unwrap().0 - first.0 < 10.0 || lines.len() < 300 {
                continue;
            }
            let start = first.0 - t0;
            let mut bytes = Vec::with_capacity(lines.len() * 1200);
            for (_, b) in &lines {
                bytes.extend_from_slice(b);
                bytes.push(b'\n');
            }
            let res = std::panic::catch_unwind(|| live_trace(&bytes));
            if let Ok(Some((w, bpm))) = res {
                let n = w.len();
                let span = n as f64 / FS;
                let end = lines.last().unwrap().0 - t0;
                let off = (end - span).max(start);
                let mut l = l4.lock().unwrap();
                l.wave = w.iter().enumerate().map(|(i, v)| [off + i as f64 / FS, *v]).collect();
                l.bpm = bpm;
                c2.request_repaint();
            }
        }
    });
    Run { stamp, secs, stop, live, job, taps: vec![], last_tap: None }
}

/// Breathing waveform of the last few seconds (scaled to about +-1) and, with 30 s of data,
/// the rate and its confidence.
fn live_trace(bytes: &[u8]) -> Option<(Vec<f64>, Option<(f64, f64)>)> {
    let rec = an::parse_bytes(bytes).ok()?;
    if rec.npkt() < 300 {
        return None;
    }
    let (_, h) = an::resample(&rec);
    let (w, _, _) = an::breathing_waveform(&h, an::Method::Snr);
    let a: Vec<f64> = w.iter().map(|v| v.abs()).collect();
    let s = crate::dsp::percentile(&a, 95.0) + 1e-12;
    let bpm = an::windows(&w).last().map(|x| (x.1, x.2)).filter(|x| x.1 >= an::PRESENCE_CONF);
    Some((w.iter().map(|v| (v / s).clamp(-1.4, 1.4)).collect(), bpm))
}

/// Re-run the analysis of a past test from its files (nothing is written).
fn load_past(results: &std::path::Path, h: &Past) -> Result<Shown, String> {
    let log = session::stamp_path(results, &h.stamp, ".log");
    let reference: Option<f64> = h.reference.parse().ok();
    let secs: f64 = h.secs.parse().unwrap_or(f64::NAN);
    let text = std::fs::read_to_string(session::stamp_path(results, &h.stamp, "-report.txt")).ok();
    if !log.exists() {
        return Ok(Shown { stamp: h.stamp.clone(), secs, reference, report: None, tap: None, text: text.unwrap_or_else(|| "The log of this test is missing.".into()) });
    }
    let rec = an::parse(&log.to_string_lossy()).map_err(|e| e.to_string())?;
    let report = an::report(&rec, reference, None, false).ok();
    let mut tap = None;
    if let Ok(s) = std::fs::read_to_string(session::stamp_path(results, &h.stamp, ".json")) {
        if let Ok(j) = serde_json::from_str::<serde_json::Value>(&s) {
            let f = |k: &str| j[k].as_array().map(|a| a.iter().filter_map(|v| v.as_f64()).collect::<Vec<f64>>()).unwrap_or_default();
            let seq: Vec<i64> = j["seq"].as_array().map(|a| a.iter().filter_map(|v| v.as_i64()).collect()).unwrap_or_default();
            let taps = f("taps");
            if taps.len() >= 4 && !seq.is_empty() {
                tap = an::tap_report(&rec, &taps, &seq, &f("host")).ok();
            }
        }
    }
    let text = text.unwrap_or_else(|| {
        let mut t = report.as_ref().map(|r| r.text.clone()).unwrap_or_default();
        if let Some(tr) = &tap {
            t.push_str("\n\n=== single breaths vs taps ===\n");
            t.push_str(&tr.text);
        }
        t
    });
    Ok(Shown { stamp: h.stamp.clone(), secs, reference, report, tap, text })
}

/// Analyse any log (Tools page): report only, no taps.
pub fn analyse_file(path: PathBuf) -> Result<Shown, String> {
    let rec = an::parse(&path.to_string_lossy()).map_err(|e| e.to_string())?;
    let r = an::report(&rec, None, None, false).map_err(|e| e.to_string())?;
    Ok(Shown {
        stamp: path.file_name().unwrap_or_default().to_string_lossy().into(),
        secs: rec.t.last().copied().unwrap_or(0.0),
        reference: None,
        text: r.text.clone(),
        report: Some(r),
        tap: None,
    })
}

// ----------------------------------------------------------------- result view
pub fn result_ui(ui: &mut egui::Ui, s: &Shown, tab: &mut ResultTab, id: &str) {
    let p = pal(ui);
    ui.horizontal(|ui| {
        theme::h2(ui, &nice_stamp(&s.stamp));
        if s.secs.is_finite() {
            theme::small(ui, format!("{:.0} s", s.secs));
        }
    });
    ui.add_space(8.0);
    let r = s.report.as_ref();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 36.0;
        if let Some(v) = s.reference {
            theme::stat(ui, &format!("{v:.1}"), "BPM", "you counted");
        }
        let est = r.and_then(|r| r.est);
        theme::stat(ui, &est.map(|v| format!("{v:.1}")).unwrap_or("–".into()), "BPM", if r.is_some() && est.is_none() { "WiFi: no breathing found" } else { "WiFi estimate" });
        if let Some(m) = r.and_then(|r| r.mae) {
            theme::stat(ui, &format!("{m:.2}"), "BPM", "average error, 30 s windows");
        }
        if let Some(t) = &s.tap {
            let get = |k: &str| t.row.iter().find(|x| x.0 == k).map(|x| x.1.clone()).unwrap_or_default();
            if let Ok(rc) = get("recall").parse::<f64>() {
                theme::stat(ui, &format!("{:.0}", 100.0 * rc), "%", "of your taps matched a breath");
            }
        }
        if let Some(h) = r.and_then(|r| r.hr) {
            theme::stat(ui, &format!("{h:.0}"), "BPM", "heart rate (experimental)");
        }
    });
    ui.add_space(12.0);
    let mut opts = vec![(ResultTab::Rate, "Rate over time"), (ResultTab::Trace, "Breaths and taps")];
    if s.tap.is_some() {
        opts.push((ResultTab::Shape, "Average breath"));
    }
    opts.push((ResultTab::Report, "Full report"));
    if *tab == ResultTab::Shape && s.tap.is_none() {
        *tab = ResultTab::Rate;
    }
    let before = *tab;
    theme::segmented(ui, tab, &opts);
    let opened_report = *tab == ResultTab::Report && before != ResultTab::Report;
    ui.add_space(8.0);
    let Some(r) = r else {
        theme::text_view(ui, &format!("{id}-text"), &s.text, 420.0);
        return;
    };
    let cat = p.cat;
    match tab {
        ResultTab::Rate => theme::panel(ui, |ui| {
            charts::heading(ui, "Breathing rate", "per breath, per 30 s window, and fused");
            let b = &r.breaths;
            let ok: Vec<usize> = (0..b.len()).filter(|&i| b.ok[i]).collect();
            let mut k = vec![(cat[0], "WiFi rate (fused)", Mark::Line), (cat[1], "30 s windows", Mark::Dot), (cat[2], "Single breaths", Mark::Dot)];
            if s.tap.is_some() {
                k.push((cat[3], "Your taps (3 breaths)", Mark::Line));
            }
            if s.reference.is_some() {
                k.push((p.text2, "You counted", Mark::Dashed));
            }
            charts::key(ui, &k);
            charts::base(&p, &format!("{id}-rate"), 340.0)
                .y_axis_label("BPM")
                .x_axis_formatter(|m, _| format!("{:.0} s", m.value))
                .label_formatter(charts::hover(None, "BPM", 1))
                .include_y(0.0)
                .show(ui, |pu| {
                    for run in charts::runs(&r.fused.0, &r.fused.1) {
                        pu.line(Line::new("WiFi rate (fused)", run).color(cat[0]).width(2.5));
                    }
                    let good: Vec<[f64; 2]> = r.windows.iter().filter(|w| w.2 >= an::PRESENCE_CONF).map(|w| [w.0, w.1]).collect();
                    pu.points(Points::new("30 s windows", good).color(cat[1]).radius(4.0).shape(MarkerShape::Diamond));
                    let pts: Vec<[f64; 2]> = ok.iter().map(|&i| [b.rise[i], 60.0 / b.ttot[i]]).collect();
                    pu.points(Points::new("Single breaths", pts).color(cat[2]).radius(3.0));
                    if let Some(t) = &s.tap {
                        for run in charts::runs(&t.rt, &t.rr3) {
                            pu.line(Line::new("Your taps (3 breaths)", run).color(cat[3]).width(2.0));
                        }
                    }
                    if let Some(v) = s.reference {
                        pu.hline(HLine::new("You counted", v).color(p.text2).style(LineStyle::dashed_loose()).width(1.2));
                    }
                });
        }),
        ResultTab::Trace => theme::panel(ui, |ui| {
            charts::heading(ui, "Breathing trace", "the WiFi signal with each breath it found, and your taps");
            let b = s.tap.as_ref().map(|t| &t.breaths).unwrap_or(&r.breaths);
            let x: Vec<[f64; 2]> = b.x.iter().enumerate().map(|(i, v)| [i as f64 / FS, *v]).collect();
            let at = |t: f64| b.x.get((t * FS).round() as usize).copied().unwrap_or(f64::NAN);
            let mut k = vec![(cat[0], "WiFi breathing signal", Mark::Line), (cat[0], "Breath found", Mark::Dot), (faint(&p), "Found, not usable", Mark::Ring)];
            if s.tap.is_some() {
                k.push((cat[1], "Your taps", Mark::Line));
            }
            charts::key(ui, &k);
            charts::base(&p, &format!("{id}-trace"), 320.0)
                .x_axis_formatter(|m, _| format!("{:.0} s", m.value))
                .show_y(false)
                .label_formatter(|h| match h {
                    egui_plot::HoverPosition::NearDataPoint { plot_name, position, .. } => Some(format!("{plot_name}\n{:.1} s", position.x)),
                    _ => None,
                })
                .show(ui, |pu| {
                    pu.line(Line::new("WiFi breathing signal", x).color(cat[0]).width(1.6));
                    let ok: Vec<[f64; 2]> = (0..b.len()).filter(|&i| b.ok[i]).map(|i| [b.rise[i], at(b.rise[i])]).filter(|v| v[1].is_finite()).collect();
                    let bad: Vec<[f64; 2]> = (0..b.len()).filter(|&i| !b.ok[i]).map(|i| [b.rise[i], at(b.rise[i])]).filter(|v| v[1].is_finite()).collect();
                    pu.points(Points::new("Breath found", ok).color(cat[0]).radius(4.5).filled(true));
                    pu.points(Points::new("Breath found, not usable", bad).color(faint(&p)).radius(4.0).filled(false));
                    if let Some(t) = &s.tap {
                        for (i, v) in t.taps.iter().enumerate() {
                            pu.vline(VLine::new(if i == 0 { "Your taps" } else { "" }, *v).color(cat[1]).width(1.3));
                        }
                    }
                });
            if let Some(t) = &s.tap {
                theme::small(ui, format!("Taps line up with the breath onsets after shifting them by {:.2} s (your reaction time plus the clock offset).", t.lag));
            }
        }),
        ResultTab::Shape => theme::panel(ui, |ui| {
            let Some(t) = &s.tap else { return };
            charts::heading(ui, "Average breath", "the signal around each tap, and their average");
            charts::key(ui, &[(alpha(p.text2, 90), "Single breaths", Mark::Line), (cat[0], "Average", Mark::Line), (cat[1], "Tap", Mark::Line)]);
            charts::base(&p, &format!("{id}-shape"), 320.0)
                .x_axis_label("seconds from the tap")
                .show_y(false)
                .label_formatter(|h| match h {
                    egui_plot::HoverPosition::NearDataPoint { plot_name, position, .. } => Some(format!("{plot_name}\n{:+.2} s", position.x)),
                    _ => None,
                })
                .show(ui, |pu| {
                    for (i, sg) in t.segs.iter().enumerate() {
                        let pts: Vec<[f64; 2]> = t.seg_t.iter().zip(sg).map(|(a, b)| [*a, *b]).collect();
                        pu.line(Line::new(if i == 0 { "Single breaths" } else { "" }, pts).color(alpha(p.text2, 60)).width(1.0).allow_hover(false));
                    }
                    let avg: Vec<[f64; 2]> = t.seg_t.iter().zip(&t.avg).map(|(a, b)| [*a, *b]).collect();
                    pu.line(Line::new("Average", avg).color(cat[0]).width(3.0));
                    pu.vline(VLine::new("Tap", 0.0).color(cat[1]).width(1.3));
                });
        }),
        ResultTab::Report => {
            // As tall as the window allows, leaving room for the tab bar above; shorter reports
            // shrink to fit. Opening the tab scrolls the page so the whole box is in view.
            let height = (ui.clip_rect().height() - 90.0).max(460.0);
            let resp = ui.scope(|ui| theme::text_view(ui, &format!("{id}-text"), &s.text, height)).response;
            if opened_report {
                resp.scroll_to_me(Some(egui::Align::BOTTOM));
            }
        }
    }
}
