//! The desktop app: a sidebar of pages over the same results folder the command-line tools use.

mod boards;
mod breath;
mod charts;
mod demo;
mod fitbit;
mod nights;
mod nightview;
mod record;
mod theme;
mod tools;

use crate::hw::{self, PortInfo};
use eframe::egui::{self, CornerRadius, FontId, Margin, RichText, Stroke, Vec2};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use theme::{Level, pal};

// ----------------------------------------------------------------- background work
/// A result computed on another thread; poll it each frame.
pub struct Job<T> {
    rx: mpsc::Receiver<T>,
}
impl<T: Send + 'static> Job<T> {
    pub fn spawn(ctx: &egui::Context, f: impl FnOnce() -> T + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            ctx.request_repaint();
        });
        Job { rx }
    }
    pub fn poll(&self) -> Option<T> {
        self.rx.try_recv().ok()
    }
}

/// Lines written by a worker thread, shown by the page.
#[derive(Clone, Default)]
pub struct Log(Arc<Mutex<Vec<String>>>);
impl Log {
    pub fn push(&self, s: impl Into<String>) {
        let mut v = self.0.lock().unwrap();
        v.push(s.into());
        let n = v.len();
        if n > 5000 {
            v.drain(..n - 5000);
        }
    }
    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
    pub fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
    /// A Fn(String) that appends here and repaints (for the analysis callbacks).
    pub fn sink(&self, ctx: &egui::Context) -> impl Fn(String) + Send + Sync + 'static {
        let (me, ctx) = (self.clone(), ctx.clone());
        move |s| {
            me.push(s);
            ctx.request_repaint();
        }
    }
}

// ----------------------------------------------------------------- settings
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub enum ThemeChoice {
    System,
    Light,
    Dark,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// the folder in use: $CSI_RESULTS, else `results_custom`, else default_results()
    #[serde(skip)]
    pub results: PathBuf,
    /// a folder the user chose on the Tools page
    pub results_custom: Option<PathBuf>,
    pub tx_mac: String,
    pub rx_mac: String,
    pub theme: ThemeChoice,
    pub breath_secs: u32,
    pub link_secs: u32,
    pub night_hours: f64,
    pub part_min: u32,
    pub keep_awake: bool,
    pub screen_off: bool,
    pub epoch_min: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            results: default_results(),
            results_custom: None,
            tx_mac: hw::DEFAULT_TX_MAC.into(),
            rx_mac: hw::DEFAULT_RX_MAC.into(),
            theme: ThemeChoice::System,
            breath_secs: 90,
            link_secs: 10,
            night_hours: 9.0,
            part_min: 10,
            keep_awake: true,
            screen_off: true,
            epoch_min: 5,
        }
    }
}

/// $CSI_RESULTS, else a `results` folder next to the program or up to four levels above it
/// (the project's single_txrx/results when run from the source tree), else Documents/csi-breathing.
pub fn default_results() -> PathBuf {
    if let Ok(p) = std::env::var("CSI_RESULTS") {
        return PathBuf::from(p);
    }
    let mut starts = vec![];
    if let Ok(e) = std::env::current_exe() {
        starts.push(e);
    }
    if let Ok(c) = std::env::current_dir() {
        starts.push(c.join("x"));
    }
    for s in starts {
        for a in s.ancestors().skip(1).take(5) {
            let r = a.join("results");
            if r.join("sessions.csv").exists() || a.join("record_night.py").exists() {
                return r;
            }
        }
    }
    dirs::document_dir().or_else(dirs::home_dir).unwrap_or_default().join("csi-breathing")
}

// ----------------------------------------------------------------- shared state
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Breath,
    Record,
    Nights,
    Fitbit,
    Boards,
    Tools,
}

pub struct Shared {
    pub set: Settings,
    pub replay: Option<PathBuf>,
    pub ports: Vec<PortInfo>,
    ports_at: Option<Instant>,
    /// a breath test or link check holds the RX
    pub capturing: bool,
    /// a flash is running
    pub flashing: bool,
    /// the detached night recorder is running
    pub recording: bool,
    pub record_status: String,
    pub goto: Option<Page>,
    /// set by a breath test / tools analysis, read by the Tools page when it changes
    pub tools_analysis: Option<PathBuf>,
}

impl Shared {
    pub fn results(&self) -> PathBuf {
        let _ = std::fs::create_dir_all(&self.set.results);
        self.set.results.clone()
    }
    /// The results folder, created (with its parents) if absent; Err says why it could not be.
    pub fn ensure_results(&self) -> Result<PathBuf, String> {
        std::fs::create_dir_all(&self.set.results)
            .map(|_| self.set.results.clone())
            .map_err(|e| format!("Cannot create the results folder {}: {e}", self.set.results.display()))
    }
    pub fn port_of(&self, mac: &str) -> Option<&PortInfo> {
        let m = hw::norm_mac(mac);
        self.ports.iter().find(|p| !m.is_empty() && hw::norm_mac(&p.serial) == m)
    }
    pub fn tx(&self) -> Option<&PortInfo> {
        self.port_of(&self.set.tx_mac)
    }
    pub fn rx(&self) -> Option<&PortInfo> {
        self.port_of(&self.set.rx_mac)
    }
    pub fn rx_busy(&self) -> Option<&'static str> {
        if self.recording {
            Some("The night recording is using the RX. Stop it first (Night recording page).")
        } else if self.capturing {
            Some("A breath test or link check is using the RX.")
        } else if self.flashing {
            Some("A board is being flashed.")
        } else {
            None
        }
    }
    pub fn refresh_ports(&mut self) {
        self.ports = hw::list_ports();
        self.ports_at = Some(Instant::now());
    }
}

// ----------------------------------------------------------------- the app
pub struct App {
    pub(crate) page: Page,
    results_seen: PathBuf,
    demo: Option<demo::Demo>,
    pub(crate) sh: Shared,
    pub(crate) breath: breath::BreathPage,
    record: record::RecordPage,
    pub(crate) nights: nights::NightsPage,
    fitbit: fitbit::FitbitPage,
    boards: boards::BoardsPage,
    tools: tools::ToolsPage,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::setup(&cc.egui_ctx);
        let mut set: Settings = cc.storage.and_then(|s| eframe::get_value(s, "settings")).unwrap_or_default();
        set.results = match (std::env::var_os("CSI_RESULTS"), &set.results_custom) {
            (Some(p), _) => PathBuf::from(p),
            (None, Some(c)) => c.clone(),
            (None, None) => default_results(),
        };
        apply_theme(&cc.egui_ctx, set.theme);
        match std::env::var("CSI_THEME").as_deref() {
            Ok("dark") => apply_theme(&cc.egui_ctx, ThemeChoice::Dark),
            Ok("light") => apply_theme(&cc.egui_ctx, ThemeChoice::Light),
            _ => {}
        }
        let mut sh = Shared {
            set,
            replay: None,
            ports: vec![],
            ports_at: None,
            capturing: false,
            flashing: false,
            recording: false,
            record_status: String::new(),
            goto: None,
            tools_analysis: None,
        };
        sh.refresh_ports();
        // make the results folder on the first start, so recordings and tests have somewhere to go
        let _ = sh.ensure_results();
        let record = record::RecordPage::new(&mut sh);
        App {
            page: Page::Breath,
            results_seen: sh.set.results.clone(),
            demo: demo::Demo::from_env(),
            breath: breath::BreathPage::new(&sh),
            nights: nights::NightsPage::new(&sh),
            fitbit: fitbit::FitbitPage::new(),
            boards: boards::BoardsPage::default(),
            tools: tools::ToolsPage::default(),
            record,
            sh,
        }
    }
}

pub fn apply_theme(ctx: &egui::Context, t: ThemeChoice) {
    ctx.set_theme(match t {
        ThemeChoice::System => egui::ThemePreference::System,
        ThemeChoice::Light => egui::ThemePreference::Light,
        ThemeChoice::Dark => egui::ThemePreference::Dark,
    });
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, "settings", &self.sh.set);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.sh.ports_at.is_none_or(|t| t.elapsed() > Duration::from_secs(2)) && !self.sh.flashing {
            self.sh.refresh_ports();
        }
        self.record.poll(&ctx, &mut self.sh);
        self.breath.poll(&ctx, &mut self.sh);
        ctx.request_repaint_after(Duration::from_millis(1000));
        if self.sh.set.results != self.results_seen {
            self.results_seen = self.sh.set.results.clone();
            let _ = self.sh.ensure_results();
            self.breath.reload_history(&self.sh);
            self.nights = nights::NightsPage::new(&self.sh);
            self.record = record::RecordPage::new(&mut self.sh);
        }
        demo::step(self, &ctx);
        if let Some(p) = self.sh.goto.take() {
            self.page = p;
        }
        let p = pal(ui);
        egui::Panel::left("nav")
            .exact_size(216.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(egui::Frame::new().fill(p.side).inner_margin(Margin { left: 14, right: 14, top: 22, bottom: 16 }))
            .show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p.bg)).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt(("page", self.page as u8)).auto_shrink([false, false]).show(ui, |ui| {
                egui::Frame::new().inner_margin(Margin { left: 36, right: 36, top: 26, bottom: 32 }).show(ui, |ui| {
                    let w = ui.available_width().min(1320.0);
                    ui.set_max_width(w);
                    match self.page {
                        Page::Breath => self.breath.ui(ui, &mut self.sh),
                        Page::Record => self.record.ui(ui, &mut self.sh),
                        Page::Nights => self.nights.ui(ui, &mut self.sh),
                        Page::Fitbit => self.fitbit.ui(ui, &mut self.sh),
                        Page::Boards => self.boards.ui(ui, &mut self.sh),
                        Page::Tools => self.tools.ui(ui, &mut self.sh),
                    }
                });
            });
        });
    }
}

impl App {
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let p = pal(ui);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            breath_mark(ui, p.accent);
            ui.label(RichText::new("CSI breathing").font(FontId::new(17.0, theme::medium())).color(p.text));
        });
        ui.add_space(22.0);
        let items = [
            (Page::Breath, "Breath test", "tap along, compare"),
            (Page::Record, "Night recording", "record while you sleep"),
            (Page::Nights, "Nights", "analyse a recorded night"),
            (Page::Fitbit, "Fitbit", "sleep stages to compare"),
            (Page::Boards, "Boards", "find, check, flash"),
            (Page::Tools, "Tools", "replay, selftest, settings"),
        ];
        for (pg, name, sub) in items {
            let on = self.page == pg;
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), egui::Sense::click());
            let fill = if on {
                p.panel
            } else if resp.hovered() {
                charts::alpha(p.panel, 150)
            } else {
                egui::Color32::TRANSPARENT
            };
            ui.painter().rect(rect, CornerRadius::same(8), fill, if on { Stroke::new(1.0, p.border) } else { Stroke::NONE }, egui::StrokeKind::Inside);
            if on {
                let bar = egui::Rect::from_min_size(rect.left_top() + Vec2::new(0.0, 12.0), Vec2::new(3.0, rect.height() - 24.0));
                ui.painter().rect_filled(bar, CornerRadius::same(2), p.accent);
            }
            ui.painter().text(
                rect.left_top() + Vec2::new(14.0, 7.0),
                egui::Align2::LEFT_TOP,
                name,
                FontId::new(14.5, if on { theme::medium() } else { egui::FontFamily::Proportional }),
                p.text,
            );
            ui.painter().text(rect.left_top() + Vec2::new(14.0, 26.0), egui::Align2::LEFT_TOP, sub, FontId::proportional(11.5), p.text2);
            if resp.clicked() {
                self.page = pg;
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            ui.add_space(2.0);
        }
        // board and recorder state, pinned to the bottom
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(4.0);
            if let Some(r) = &self.sh.replay {
                theme::status(ui, Level::Warn, format!("Replaying {}", r.file_name().unwrap_or_default().to_string_lossy()));
            }
            if self.sh.recording {
                let st = self.sh.record_status.clone();
                if theme::status(ui, Level::Good, if st.is_empty() { "Recording a night".to_string() } else { st }).clicked() {
                    self.page = Page::Record;
                }
            }
            let rx = self.sh.rx().map(|p| p.name.clone());
            let tx = self.sh.tx().map(|p| p.name.clone());
            theme::status(ui, if rx.is_some() { Level::Good } else { Level::Off }, format!("RX {}", rx.unwrap_or_else(|| "not plugged in".into())));
            theme::status(ui, if tx.is_some() { Level::Good } else { Level::Off }, format!("TX {}", tx.unwrap_or_else(|| "not on USB".into())));
            ui.add_space(2.0);
            ui.label(RichText::new("Boards").size(12.0).color(p.text2));
        });
    }
}

/// The app's mark: one breath (a slow sine) in the accent colour.
fn breath_mark(ui: &mut egui::Ui, c: egui::Color32) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(26.0, 18.0), egui::Sense::hover());
    let pts: Vec<egui::Pos2> = (0..=24)
        .map(|i| {
            let x = i as f32 / 24.0;
            egui::pos2(r.left() + x * r.width(), r.center().y - (x * std::f32::consts::TAU).sin() * r.height() * 0.38)
        })
        .collect();
    ui.painter().add(egui::Shape::line(pts, Stroke::new(2.2, c)));
}

pub fn run() -> eframe::Result<()> {
    let icon = {
        let n = 64usize;
        let mut rgba = vec![0u8; n * n * 4];
        for i in 0..n {
            let x = i as f32 / (n - 1) as f32;
            let yc = 0.5 - (x * std::f32::consts::TAU).sin() * 0.28;
            for j in 0..n {
                let y = j as f32 / (n - 1) as f32;
                let d = (y - yc).abs() * n as f32;
                let a = (3.5 - d).clamp(0.0, 1.0);
                let k = (j * n + i) * 4;
                rgba[k..k + 4].copy_from_slice(&[0x2a, 0x78, 0xd6, (a * 255.0) as u8]);
            }
        }
        egui::IconData { rgba, width: n as u32, height: n as u32 }
    };
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CSI breathing")
            .with_app_id("csi-breathing")
            .with_inner_size(
                std::env::var("CSI_SIZE")
                    .ok()
                    .and_then(|s| {
                        let (a, b) = s.split_once('x')?;
                        Some([a.parse().ok()?, b.parse().ok()?])
                    })
                    .unwrap_or([1340.0, 900.0]),
            )
            .with_min_inner_size([900.0, 600.0])
            .with_icon(icon),
        // With vsync the GL swap waits for the compositor's frame callback, which never comes while the
        // window is on a hidden workspace: the app then stops answering and is marked "not responding".
        // Frames are only drawn on demand anyway, so no vsync costs nothing.
        glow_options: eframe::egui_glow::GlowConfiguration { vsync: false, ..Default::default() },
        ..Default::default()
    };
    eframe::run_native("csi-breathing", opts, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
