//! Fitbit: connect the Google Health API and look at a night's sleep stages and breathing rate.

use super::charts;
use super::nights::stage_color;
use super::theme::{self, Level, pal};
use super::{Job, Shared};
use crate::health;
use crate::night::clock;
use chrono::{Datelike, Duration as CDuration, Local, NaiveDate};
use eframe::egui::{self, RichText, Vec2};
use egui_plot::Polygon;

struct Session {
    span: (f64, f64),
    device: String,
    stages: Vec<(f64, f64, String)>,
    restless: Vec<f64>,
    resp: Vec<(String, f64, f64, f64)>,
}

pub struct FitbitPage {
    status: health::Status,
    date: NaiveDate,
    job: Option<Job<Result<Vec<Session>, String>>>,
    sessions: Option<(NaiveDate, Vec<Session>)>,
    action: Option<(&'static str, Job<Result<String, String>>)>,
    msg: Option<(Level, String)>,
}

impl FitbitPage {
    pub fn new() -> Self {
        FitbitPage { status: health::status(), date: Local::now().date_naive(), job: None, sessions: None, action: None, msg: None }
    }

    fn fetch(&mut self, ctx: &egui::Context) {
        let d = self.date;
        self.msg = None;
        self.job = Some(Job::spawn(ctx, move || {
            let (a, b) = health::day_range(d.year(), d.month(), d.day());
            let mut out = vec![];
            for dp in health::sleep_sessions(a, b).map_err(|e| e.to_string())? {
                let stages = health::stages(&dp);
                let span = health::session_span(&dp).or_else(|| Some((stages.first()?.0, stages.last()?.1))).unwrap_or((a, b));
                out.push(Session {
                    span,
                    device: dp["dataSource"]["device"]["displayName"].as_str().unwrap_or("").to_string(),
                    restless: health::restless(&dp),
                    resp: health::respiration(span.1).unwrap_or_default(),
                    stages,
                });
            }
            Ok(out)
        }));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, _sh: &mut Shared) {
        let p = pal(ui);
        if let Some(j) = &self.job {
            if let Some(r) = j.poll() {
                match r {
                    Ok(s) => self.sessions = Some((self.date, s)),
                    Err(e) => self.msg = Some((Level::Bad, e)),
                }
                self.job = None;
            }
        }
        if let Some((what, j)) = &self.action {
            if let Some(r) = j.poll() {
                self.msg = Some(match r {
                    Ok(s) => (Level::Good, s),
                    Err(e) => (Level::Bad, format!("{what} failed: {e}")),
                });
                self.action = None;
                self.status = health::status();
            }
        }
        theme::title(ui, "Fitbit");
        ui.add_space(2.0);
        theme::note(ui, "Sleep stages, restless moments and breathing rate from a Fitbit or Pixel Watch, through the Google Health API. The Nights page uses them automatically.");
        ui.add_space(16.0);

        theme::panel(ui, |ui| {
            theme::h2(ui, "Connection");
            ui.add_space(4.0);
            let s = health::Status { ..self.status };
            theme::status(
                ui,
                if s.has_client { Level::Good } else { Level::Bad },
                if s.has_client { format!("OAuth client installed ({})", health::cred_path().display()) } else { "No OAuth client yet: choose the credentials.json of your Google Cloud desktop app".into() },
            );
            theme::status(
                ui,
                if s.has_token { Level::Good } else { Level::Off },
                match (s.has_token, s.token_valid) {
                    (false, _) => "Not logged in".to_string(),
                    (true, true) => "Logged in".into(),
                    (true, false) => "Logged in; the access token renews on the next request".into(),
                },
            );
            theme::small(ui, "While the Cloud app is in Testing, Google ends the login after 7 days: then log in again.");
            ui.add_space(10.0);
            let idle = self.action.is_none();
            ui.horizontal_wrapped(|ui| {
                if theme::secondary(ui, "Choose credentials.json ...", idle).clicked() {
                    if let Some(f) = rfd::FileDialog::new().add_filter("JSON", &["json"]).set_directory(dirs::download_dir().unwrap_or_default()).pick_file() {
                        self.msg = Some(match health::install_credentials(&f) {
                            Ok(()) => (Level::Good, format!("Installed. You can delete {}", f.display())),
                            Err(e) => (Level::Bad, format!("Could not install it: {e}")),
                        });
                        self.status = health::status();
                    }
                }
                if theme::primary(ui, if self.action.as_ref().is_some_and(|a| a.0 == "Login") { "Waiting for the browser ..." } else { "Log in" }, idle && s.has_client).clicked() {
                    self.action = Some(("Login", Job::spawn(ui.ctx(), || health::login().map_err(|e| e.to_string()))));
                    self.msg = Some((Level::Warn, "Finish the consent in the browser window that opened (up to 5 minutes).".into()));
                }
                if theme::secondary(ui, "Check connection", idle && s.has_token).clicked() {
                    self.action = Some(("Check", Job::spawn(ui.ctx(), || health::check().map(|_| "The connection works.".to_string()).map_err(|e| e.to_string()))));
                }
            });
            if let Some((l, m)) = &self.msg {
                ui.add_space(6.0);
                theme::status(ui, *l, m.clone());
            }
        });

        ui.add_space(20.0);
        theme::h2(ui, "Sleep for a night");
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Night ending on").color(p.text2));
            if ui.button("◀").on_hover_text("Previous day").clicked() {
                self.date -= CDuration::days(1);
            }
            ui.label(RichText::new(self.date.format("%a %-d %b %Y").to_string()).font(egui::FontId::new(15.0, theme::medium())));
            let today = Local::now().date_naive();
            if ui.add_enabled(self.date < today, egui::Button::new("▶")).on_hover_text("Next day").clicked() {
                self.date += CDuration::days(1);
            }
            if ui.add_enabled(self.date != today, egui::Button::new("Today")).clicked() {
                self.date = today;
            }
            ui.add_space(8.0);
            if theme::primary(ui, if self.job.is_some() { "Loading ..." } else { "Show sleep" }, self.job.is_none() && self.status.has_token).clicked() {
                self.fetch(ui.ctx());
            }
        });
        ui.add_space(10.0);
        let Some((d, sessions)) = &self.sessions else { return };
        if sessions.is_empty() {
            theme::note(ui, format!("No sleep session ends on {}. Has the watch synced?", d.format("%-d %b")));
            return;
        }
        for (k, s) in sessions.iter().enumerate() {
            session_ui(ui, s, k);
            ui.add_space(14.0);
        }
    }
}

fn session_ui(ui: &mut egui::Ui, s: &Session, k: usize) {
    let p = pal(ui);
    theme::panel(ui, |ui| {
        let dev = if s.device.is_empty() { String::new() } else { format!(", {}", s.device) };
        theme::h2(ui, &format!("{} to {}{dev}", clock(s.span.0, "%a %H:%M"), clock(s.span.1, "%H:%M")));
        ui.add_space(6.0);
        let mut mins = std::collections::BTreeMap::new();
        for (a, b, t) in &s.stages {
            *mins.entry(t.as_str()).or_insert(0.0) += (b - a) / 60.0;
        }
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 34.0;
            for st in ["deep", "light", "rem", "wake"] {
                if let Some(m) = mins.get(st) {
                    let name = match st {
                        "deep" => "deep",
                        "light" => "light",
                        "rem" => "REM",
                        _ => "awake",
                    };
                    theme::stat(ui, &format!("{m:.0}"), "min", name);
                }
            }
            theme::stat(ui, &s.restless.len().to_string(), "", "restless marks");
            for (st, bpm, sd, _) in &s.resp {
                let name = if st == "full" { "breathing, whole night".to_string() } else { format!("breathing in {}", if st == "rem" { "REM" } else { st }) };
                theme::stat(ui, &format!("{bpm:.1} ± {sd:.1}"), "BPM", &name);
            }
        });
        if s.stages.is_empty() {
            return;
        }
        ui.add_space(8.0);
        let t0 = s.span.0;
        let mut keys: Vec<(egui::Color32, &str, charts::Mark)> =
            [("wake", "Awake"), ("rem", "REM"), ("light", "Light"), ("deep", "Deep")].iter().map(|(s, n)| (stage_color(s), *n, charts::Mark::Box)).collect();
        if !s.restless.is_empty() {
            keys.push((p.text2, "Restless", charts::Mark::Down));
        }
        charts::key(ui, &keys);
        charts::base(&p, &format!("fitbit-{k}"), 140.0)
            .x_axis_formatter(charts::clock_axis(t0))
            .show_grid([true, false])
            .y_axis_formatter(|m, _| match m.value.round() as i32 {
                3 => "Awake".into(),
                2 => "REM".into(),
                1 => "Light".into(),
                0 => "Deep".into(),
                _ => String::new(),
            })
            .y_grid_spacer(|_| (0..4).map(|i| egui_plot::GridMark { value: i as f64, step_size: 1.0 }).collect())
            .include_y(-0.6)
            .include_y(3.9)
            .label_formatter(move |h| match h {
                egui_plot::HoverPosition::Elsewhere { position } | egui_plot::HoverPosition::NearDataPoint { position, .. } => Some(clock(t0 + position.x, "%H:%M")),
            })
            .show(ui, |pu| {
                for (a, b, st) in &s.stages {
                    let y = match st.as_str() {
                        "wake" => 3.0,
                        "rem" => 2.0,
                        "light" => 1.0,
                        _ => 0.0,
                    };
                    let c = stage_color(st);
                    let (a, b) = (a - t0, b - t0);
                    pu.polygon(Polygon::new("", vec![[a, y - 0.42], [b, y - 0.42], [b, y + 0.42], [a, y + 0.42]]).fill_color(c).stroke(egui::Stroke::new(0.0, c)).allow_hover(false));
                }
                let pts: Vec<[f64; 2]> = s.restless.iter().map(|t| [t - t0, 3.7]).collect();
                pu.points(egui_plot::Points::new("Restless", pts).shape(egui_plot::MarkerShape::Down).radius(4.5).color(p.text2));
            });
        let _ = Vec2::ZERO;
    });
}
