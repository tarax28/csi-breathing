//! Tools: replay a recorded test instead of the RX, the selftest, analyse any log, settings.

use super::breath::{self, ResultTab, Shown};
use super::theme::{self, Level, pal};
use super::{Job, Shared, ThemeChoice};
use eframe::egui::{self, RichText};

pub struct ToolsPage {
    selftest: Option<Job<String>>,
    selftest_out: Option<String>,
    analysis: Option<Job<Result<Shown, String>>>,
    shown: Option<Result<Shown, String>>,
    tab: ResultTab,
}

impl Default for ToolsPage {
    fn default() -> Self {
        ToolsPage { selftest: None, selftest_out: None, analysis: None, shown: None, tab: ResultTab::Rate }
    }
}

impl ToolsPage {
    pub fn ui(&mut self, ui: &mut egui::Ui, sh: &mut Shared) {
        let p = pal(ui);
        if let Some(j) = &self.selftest {
            if let Some(s) = j.poll() {
                self.selftest_out = Some(s);
                self.selftest = None;
            }
        }
        if let Some(j) = &self.analysis {
            if let Some(r) = j.poll() {
                self.shown = Some(r);
                self.analysis = None;
            }
        }
        theme::title(ui, "Tools");
        ui.add_space(16.0);

        theme::h2(ui, "Replay");
        theme::small(ui, "Try the breath test without the boards: it plays a recorded test at the real speed.");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            match &sh.replay {
                Some(r) => theme::status(ui, Level::Warn, format!("The breath test replays {}", r.display())),
                None => theme::status(ui, Level::Good, "The breath test reads the live RX"),
            };
        });
        ui.horizontal(|ui| {
            if theme::secondary(ui, "Replay a test log ...", true).clicked() {
                if let Some(f) = rfd::FileDialog::new().add_filter("CSI test logs", &["log"]).set_directory(sh.results()).pick_file() {
                    sh.replay = Some(f);
                }
            }
            if theme::secondary(ui, "Use the live RX", sh.replay.is_some()).clicked() {
                sh.replay = None;
            }
        });

        ui.add_space(22.0);
        theme::h2(ui, "Check the analysis");
        theme::small(ui, "The selftest makes up two links breathing at 15 and 21 BPM and analyses them.");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if theme::secondary(ui, if self.selftest.is_some() { "Running the selftest ..." } else { "Run the selftest" }, self.selftest.is_none()).clicked() {
                self.selftest = Some(Job::spawn(ui.ctx(), crate::session::selftest));
            }
            if theme::secondary(ui, if self.analysis.is_some() { "Analysing ..." } else { "Analyse a CSI log ..." }, self.analysis.is_none()).clicked() {
                if let Some(f) = rfd::FileDialog::new().add_filter("CSI logs", &["log", "gz"]).set_directory(sh.results()).pick_file() {
                    self.shown = None;
                    self.analysis = Some(Job::spawn(ui.ctx(), move || breath::analyse_file(f)));
                }
            }
        });
        if let Some(s) = &self.selftest_out {
            ui.add_space(6.0);
            let ok = s.matches("fused rate: median").count() == 2;
            theme::status(ui, if ok { Level::Good } else { Level::Bad }, if ok { "Both made-up rates were found" } else { "The selftest did not find both rates" });
            theme::text_view(ui, "selftest", s, 300.0);
        }
        match &self.shown {
            Some(Ok(s)) => {
                ui.add_space(10.0);
                breath::result_ui(ui, s, &mut self.tab, "tools");
            }
            Some(Err(e)) => {
                ui.add_space(6.0);
                theme::status(ui, Level::Bad, format!("Could not analyse it: {e}"));
            }
            None => {}
        }

        ui.add_space(22.0);
        theme::h2(ui, "Settings");
        ui.add_space(6.0);
        egui::Grid::new("settings").num_columns(2).spacing([24.0, 12.0]).show(ui, |ui| {
            ui.label(RichText::new("Results folder").color(p.text2));
            ui.horizontal(|ui| {
                ui.label(RichText::new(sh.set.results.display().to_string()).monospace());
                if ui.button("Change ...").clicked() {
                    if let Some(d) = rfd::FileDialog::new().set_directory(&sh.set.results).pick_folder() {
                        sh.set.results = d.clone();
                        sh.set.results_custom = Some(d);
                    }
                }
                if ui.button("Open").clicked() {
                    let _ = open::that(sh.results());
                }
            });
            ui.end_row();
            ui.label(RichText::new("Appearance").color(p.text2));
            if theme::segmented(ui, &mut sh.set.theme, &[(ThemeChoice::System, "System"), (ThemeChoice::Light, "Light"), (ThemeChoice::Dark, "Dark")]) {
                super::apply_theme(ui.ctx(), sh.set.theme);
            }
            ui.end_row();
            ui.label(RichText::new("Fitbit login").color(p.text2));
            ui.label(RichText::new(crate::health::conf_dir().display().to_string()).monospace());
            ui.end_row();
        });
        ui.add_space(12.0);
        theme::small(ui, format!("csi-breathing {}. Fonts: Noto Sans (SIL Open Font License).", env!("CARGO_PKG_VERSION")));
    }
}
