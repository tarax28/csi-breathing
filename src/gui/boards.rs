//! Boards: find the TX and RX by the MAC in their USB serial number, check the link, and flash
//! the firmware that is built into this program.

use super::theme::{self, Level, pal};
use super::{Job, Log, Shared};
use crate::hw::{self, Firmware, PortInfo};
use eframe::egui::{self, RichText, Vec2};

#[derive(Default)]
pub struct BoardsPage {
    check: Option<Job<Result<(bool, String), String>>>,
    result: Option<(bool, String)>,
    flash: Option<Job<Result<(), String>>>,
    log: Log,
    msg: Option<(Level, String)>,
}

fn built(fw: &Firmware) -> String {
    if !fw.embedded() {
        return "not built into this program".into();
    }
    format!("{}, built {}", fw.app, crate::night::clock(fw.built as f64, "%-d %b %Y %H:%M"))
}

impl BoardsPage {
    fn start_flash(&mut self, ctx: &egui::Context, sh: &mut Shared, roles: &[&'static str]) {
        self.msg = None;
        if let Some(m) = sh.rx_busy() {
            self.msg = Some((Level::Bad, m.into()));
            return;
        }
        let mut jobs: Vec<(&'static str, PortInfo, &'static Firmware)> = vec![];
        for &r in roles {
            let fw: &'static Firmware = if r == "TX" { &hw::FW_TX } else { &hw::FW_RX };
            let port = if r == "TX" { sh.tx() } else { sh.rx() };
            let Some(port) = port.cloned() else {
                self.msg = Some((Level::Bad, format!("The {r} board is not on USB. Plug it in, or check its MAC.")));
                return;
            };
            if !fw.embedded() {
                self.msg = Some((Level::Bad, format!("This program was built without the {r} firmware.")));
                return;
            }
            jobs.push((r, port, fw));
        }
        self.log.clear();
        sh.flashing = true;
        let log = self.log.clone();
        let c = ctx.clone();
        self.flash = Some(Job::spawn(ctx, move || {
            for (r, port, fw) in jobs {
                log.push(format!("Flashing the {r} ({}) on {}", fw.app, port.name));
                let mut say = |s: String| {
                    log.push(s);
                    c.request_repaint();
                };
                hw::flash(&port, fw, &mut say).map_err(|e| format!("{r}: {e}"))?;
                std::thread::sleep(std::time::Duration::from_millis(800));
            }
            Ok(())
        }));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, sh: &mut Shared) {
        let p = pal(ui);
        if let Some(j) = &self.check {
            if let Some(r) = j.poll() {
                sh.capturing = false;
                match r {
                    Ok(v) => self.result = Some(v),
                    Err(e) => self.result = Some((false, format!("The check could not read the RX: {e}"))),
                }
                self.check = None;
            }
        }
        if let Some(j) = &self.flash {
            if let Some(r) = j.poll() {
                sh.flashing = false;
                self.msg = Some(match r {
                    Ok(()) => (Level::Good, "Flashed. The boards restarted with the new firmware.".into()),
                    Err(e) => (Level::Bad, format!("Flashing failed: {e}")),
                });
                self.flash = None;
                sh.refresh_ports();
            }
        }
        theme::title(ui, "Boards");
        ui.add_space(2.0);
        theme::note(ui, "The transmitter sends 100 packets a second; the receiver measures how the WiFi channel changes and sends it to this computer over USB. Each board is recognised by its MAC.");
        ui.add_space(16.0);

        let busy = self.flash.is_some() || self.check.is_some();
        let wide = ui.available_width() > 820.0;
        let card = |me: &mut Self, ui: &mut egui::Ui, sh: &mut Shared, role: &'static str| {
            theme::panel(ui, |ui| {
                let (name, what) = if role == "TX" { ("Transmitter (TX)", "csi_send: sends the packets") } else { ("Receiver (RX)", "csi_recv: measures and reports CSI") };
                theme::h2(ui, name);
                theme::small(ui, what);
                ui.add_space(8.0);
                let port = if role == "TX" { sh.tx().cloned() } else { sh.rx().cloned() };
                match &port {
                    Some(pi) => theme::status(ui, Level::Good, format!("On USB at {}", pi.name)),
                    None if role == "TX" => theme::status(ui, Level::Off, "Not on USB. That is fine when it runs from a power bank."),
                    None => theme::status(ui, Level::Bad, "Not plugged in"),
                };
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("MAC").color(p.text2));
                    let mac = if role == "TX" { &mut sh.set.tx_mac } else { &mut sh.set.rx_mac };
                    ui.add(egui::TextEdit::singleline(mac).desired_width(170.0).font(egui::TextStyle::Monospace));
                });
                ui.add_space(4.0);
                theme::small(ui, format!("Firmware: {}", built(if role == "TX" { &hw::FW_TX } else { &hw::FW_RX })));
                ui.add_space(8.0);
                if theme::secondary(ui, &format!("Flash the {role}"), !busy && port.is_some()).clicked() {
                    me.start_flash(ui.ctx(), sh, &[role]);
                }
            });
        };
        if wide {
            ui.columns(2, |c| {
                card(self, &mut c[0], sh, "TX");
                card(self, &mut c[1], sh, "RX");
            });
        } else {
            card(self, ui, sh, "TX");
            ui.add_space(12.0);
            card(self, ui, sh, "RX");
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if theme::secondary(ui, "Flash both", !busy && sh.tx().is_some() && sh.rx().is_some()).clicked() {
                self.start_flash(ui.ctx(), sh, &["TX", "RX"]);
            }
            theme::small(ui, format!("Both use channel {} ({}). Changing it needs a rebuild of the firmware and of this program.", hw::FW_CHANNEL, hw::FW_COUNTRY));
        });
        if let Some((l, m)) = &self.msg {
            ui.add_space(6.0);
            theme::status(ui, *l, m.clone());
        }
        if self.flash.is_some() || !self.log.lines().is_empty() {
            ui.add_space(8.0);
            theme::log_view(ui, "flash-log", &self.log.lines(), 220.0);
        }

        ui.add_space(22.0);
        theme::h2(ui, "Link check");
        theme::small(ui, "Listens to the RX: packet rate, loss, gain lock and a frozen CSI buffer.");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut sh.set.link_secs, 3..=60).suffix(" s"));
            let can = !busy && sh.rx().is_some();
            if theme::primary(ui, if self.check.is_some() { "Checking ..." } else { "Check the link" }, can).clicked() {
                if let Some(m) = sh.rx_busy() {
                    self.result = Some((false, m.into()));
                } else if let Some(port) = sh.rx().map(|p| p.name.clone()) {
                    sh.capturing = true;
                    self.result = None;
                    let secs = sh.set.link_secs as f64;
                    self.check = Some(Job::spawn(ui.ctx(), move || hw::verify_link(&port, secs).map_err(|e| e.to_string())));
                }
            }
            if self.check.is_some() {
                ui.spinner();
            }
        });
        if let Some((ok, text)) = &self.result {
            ui.add_space(6.0);
            let head = if *ok {
                (Level::Good, "The link works")
            } else if text.contains("no CSI data") {
                (Level::Bad, "No packets: is the TX powered and in range?")
            } else {
                (Level::Bad, "The link has a problem")
            };
            theme::status(ui, head.0, head.1);
            theme::text_view(ui, "link", text, 260.0);
        }

        ui.add_space(22.0);
        theme::h2(ui, "USB serial ports");
        ui.add_space(4.0);
        if sh.ports.is_empty() {
            theme::note(ui, "No USB serial devices found.");
        } else {
            egui::Grid::new("ports").striped(true).num_columns(4).spacing([28.0, 6.0]).show(ui, |ui| {
                for h in ["Port", "Board", "USB serial", "Product"] {
                    ui.label(RichText::new(h).size(12.5).color(p.text2));
                }
                ui.end_row();
                let (tx, rx) = (hw::norm_mac(&sh.set.tx_mac), hw::norm_mac(&sh.set.rx_mac));
                for pi in &sh.ports {
                    let m = hw::norm_mac(&pi.serial);
                    ui.label(RichText::new(&pi.name).monospace());
                    ui.label(if m == tx && !m.is_empty() {
                        "TX"
                    } else if m == rx && !m.is_empty() {
                        "RX"
                    } else {
                        ""
                    });
                    ui.label(RichText::new(&pi.serial).monospace());
                    ui.label(&pi.product);
                    ui.end_row();
                }
            });
        }
        let _ = Vec2::ZERO;
    }
}
