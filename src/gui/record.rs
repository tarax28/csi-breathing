//! Night recording: starts `csi-breathing record-night` as a detached process (it keeps going if
//! the window closes), follows its output in results/night.out and reattaches on the next start.

use super::theme::{self, Level, pal};
use super::{Log, Shared};
use crate::hw;
use eframe::egui::{self, RichText};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Default, Clone)]
struct Stats {
    elapsed_h: Option<f64>,
    fps: String,
    lost: String,
    disk: String,
    free: String,
}

pub struct RecordPage {
    pid: Option<u32>,
    dir: Option<PathBuf>,
    pos: u64,
    log: Log,
    stats: Stats,
    flowing: bool,
    started: Option<Instant>,
    screen_done: bool,
    last_poll: Option<Instant>,
    last_done: Option<String>,
    msg: Option<(Level, String)>,
}

fn pid_file(sh: &Shared) -> PathBuf {
    sh.set.results.join("night-recorder.pid")
}
fn out_file(sh: &Shared) -> PathBuf {
    sh.set.results.join("night.out")
}

pub fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    unsafe {
        // our own child: reap it, or a finished recorder stays a zombie that kill(pid, 0) sees
        let mut st = 0;
        let r = libc::waitpid(pid as i32, &mut st, libc::WNOHANG);
        if r == pid as i32 {
            return false;
        }
        libc::kill(pid as i32, 0) == 0
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut code = 0u32;
        GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        code == 259
    }
}

fn which(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

impl RecordPage {
    pub fn new(sh: &mut Shared) -> Self {
        let mut me = RecordPage {
            pid: None,
            dir: None,
            pos: 0,
            log: Log::default(),
            stats: Stats::default(),
            flowing: false,
            started: None,
            screen_done: false,
            last_poll: None,
            last_done: None,
            msg: None,
        };
        me.pid = std::fs::read_to_string(pid_file(sh)).ok().and_then(|s| s.split_whitespace().next()?.parse().ok()).filter(|&p| pid_alive(p));
        if let Some(p) = me.pid {
            me.screen_done = true; // don't blank the screen again on reattach
            me.log.push(format!("A recording is in progress (process {p}): following it."));
        }
        me.read_out(sh);
        sh.recording = me.pid.is_some();
        me
    }

    fn read_out(&mut self, sh: &mut Shared) {
        let Ok(mut f) = std::fs::File::open(out_file(sh)) else { return };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.pos {
            self.pos = 0;
        }
        if f.seek(SeekFrom::Start(self.pos)).is_err() {
            return;
        }
        let mut s = String::new();
        let _ = f.read_to_string(&mut s);
        // keep a partial last line for next time
        let cut = s.rfind('\n').map(|i| i + 1).unwrap_or(0);
        self.pos += cut as u64;
        for line in s[..cut].lines() {
            self.log.push(line);
            if let Some(rest) = line.strip_prefix("recording to ") {
                if let Some(i) = rest.find("  (") {
                    self.dir = Some(PathBuf::from(&rest[..i]));
                }
            }
            if line.contains("DATA FLOWING") {
                self.flowing = true;
            }
            if line.starts_with("done:") {
                self.last_done = Some(line.to_string());
            }
            if let Some(st) = parse_status(line) {
                self.stats = st;
            }
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context, sh: &mut Shared) {
        if self.last_poll.is_some_and(|t| t.elapsed() < Duration::from_millis(900)) {
            return;
        }
        self.last_poll = Some(Instant::now());
        let was_flowing = self.flowing;
        self.read_out(sh);
        if self.flowing && !was_flowing && !self.screen_done && self.pid.is_some() {
            self.screen_done = true;
            if sh.set.screen_off {
                std::thread::spawn(|| {
                    std::thread::sleep(Duration::from_secs(2));
                    hw::screen_off();
                });
            }
        }
        if let Some(p) = self.pid {
            if !pid_alive(p) {
                self.pid = None;
                let _ = std::fs::remove_file(pid_file(sh));
                ctx.request_repaint();
            }
        }
        sh.recording = self.pid.is_some();
        sh.record_status = match (self.pid.is_some(), self.stats.elapsed_h, self.flowing) {
            (true, Some(h), _) => format!("Recording, {}", hours(h)),
            (true, None, true) => "Recording".into(),
            (true, None, false) => "Recorder waiting for data".into(),
            _ => String::new(),
        };
    }

    fn start(&mut self, sh: &mut Shared) {
        self.msg = None;
        let Some(port) = sh.rx().map(|p| p.name.clone()) else {
            self.msg = Some((Level::Bad, "The RX board is not plugged in (see the Boards page).".into()));
            return;
        };
        if let Some(m) = sh.rx_busy() {
            self.msg = Some((Level::Bad, m.into()));
            return;
        }
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => {
                self.msg = Some((Level::Bad, format!("Cannot find this program's path: {e}")));
                return;
            }
        };
        let results = match sh.ensure_results() {
            Ok(r) => r,
            Err(e) => {
                self.msg = Some((Level::Bad, e));
                return;
            }
        };
        let mut cmd: Vec<String> = vec![
            exe.to_string_lossy().into(),
            "record-night".into(),
            "--port".into(),
            port,
            "--hours".into(),
            format!("{}", sh.set.night_hours),
            "--part-min".into(),
            sh.set.part_min.to_string(),
            "--results".into(),
            results.to_string_lossy().into(),
        ];
        if cfg!(unix) && sh.set.keep_awake {
            if cfg!(target_os = "macos") && which("caffeinate") {
                let mut c: Vec<String> = vec!["caffeinate".into(), "-i".into(), "-s".into()];
                c.extend(cmd);
                cmd = c;
            }
            if which("systemd-inhibit") {
                let mut c = vec!["systemd-inhibit", "--what=idle:sleep", "--who=csi-breathing", "--why=Recording CSI overnight", "--mode=block"]
                    .into_iter()
                    .map(String::from)
                    .collect::<Vec<_>>();
                c.extend(cmd);
                cmd = c;
            }
            if which("omarchy-toggle-idle") {
                // Omarchy's stay-awake stops the lock screen / screensaver; restore it afterwards
                let mut c: Vec<String> = vec![
                    "bash".into(),
                    "-c".into(),
                    "f=$HOME/.local/state/omarchy/indicators/stay-awake; had=0; [[ -f $f ]] && had=1; omarchy-toggle-idle stay-awake >/dev/null; \"$@\"; (( had )) || omarchy-toggle-idle allow-idle >/dev/null".into(),
                    "_".into(),
                ];
                c.extend(cmd);
                cmd = c;
            }
        }
        let out = match std::fs::File::create(out_file(sh)) {
            Ok(f) => f,
            Err(e) => {
                self.msg = Some((Level::Bad, format!("Cannot write {}: {e}", out_file(sh).display())));
                return;
            }
        };
        let mut c = std::process::Command::new(&cmd[0]);
        c.args(&cmd[1..]).current_dir(&results).stdin(std::process::Stdio::null()).stdout(out.try_clone().unwrap()).stderr(out);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            c.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x00000008 | 0x00000200 | 0x08000000); // DETACHED_PROCESS | NEW_PROCESS_GROUP | NO_WINDOW
        }
        match c.spawn() {
            Ok(child) => {
                let pid = child.id();
                std::mem::forget(child);
                let _ = std::fs::write(pid_file(sh), pid.to_string());
                self.pid = Some(pid);
                self.dir = None;
                self.pos = 0;
                self.flowing = false;
                self.stats = Stats::default();
                self.screen_done = false;
                self.started = Some(Instant::now());
                self.last_done = None;
                self.log.clear();
                self.log.push(format!("Started (process {pid}). Output goes to {}.", out_file(sh).display()));
                sh.recording = true;
            }
            Err(e) => self.msg = Some((Level::Bad, format!("Could not start the recorder: {e}"))),
        }
    }

    fn stop(&mut self) {
        let Some(pid) = self.pid else { return };
        if let Some(d) = self.dir.as_ref().filter(|d| d.is_dir()) {
            let _ = std::fs::write(d.join("STOP"), b"");
            self.log.push("Stopping: the current part is being saved ...");
            return;
        }
        #[cfg(unix)]
        unsafe {
            libc::killpg(pid as i32, libc::SIGTERM);
        }
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).output();
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, sh: &mut Shared) {
        let p = pal(ui);
        let rec = self.pid.is_some();
        theme::title(ui, "Night recording");
        ui.add_space(2.0);
        theme::note(
            ui,
            "Records the RX all night into 10-minute compressed parts. The recording keeps running if you close this window; open the app again to follow it.",
        );
        ui.add_space(16.0);

        // state
        let (lvl, head, sub) = if rec {
            let waited = self.started.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
            if self.flowing {
                (Level::Good, "Recording".to_string(), "Data is flowing.".to_string())
            } else if waited > 60.0 {
                (Level::Bad, "No data yet".into(), "Nothing has arrived for a minute: check that the TX is powered and the RX plugged in.".into())
            } else {
                (Level::Warn, "Waiting for data".into(), "The recorder is running and waiting for the first packets.".into())
            }
        } else {
            (Level::Off, "Not recording".into(), self.last_done.clone().map(|d| format!("Last recording {d}")).unwrap_or_default())
        };
        theme::panel(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(theme::level_icon(lvl)).size(26.0).color(theme::level_color(&p, lvl)));
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(RichText::new(&head).font(egui::FontId::new(20.0, theme::medium())).color(p.text));
                    if !sub.is_empty() {
                        theme::note(ui, &sub);
                    }
                });
            });
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 40.0;
                let s = &self.stats;
                theme::stat(ui, &s.elapsed_h.map(hours).unwrap_or("–".into()), "", "recorded");
                theme::stat(ui, if s.fps.is_empty() { "–" } else { &s.fps }, "/s", "frames");
                theme::stat(ui, if s.lost.is_empty() { "–" } else { &s.lost }, "%", "lost");
                theme::stat(ui, if s.disk.is_empty() { "–" } else { &s.disk }, "MB", "on disk");
                theme::stat(ui, if s.free.is_empty() { "–" } else { &s.free }, "GB", "free");
            });
            if let Some(d) = &self.dir {
                ui.add_space(6.0);
                theme::small(ui, format!("Folder {}", d.display()));
            }
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                if rec {
                    if theme::primary(ui, "Stop recording", true).clicked() {
                        self.stop();
                    }
                } else if theme::primary(ui, "Start recording", true).clicked() {
                    self.start(sh);
                }
                if theme::secondary(ui, "Turn the screen off", true).clicked() && !hw::screen_off() {
                    self.msg = Some((Level::Warn, "This desktop has no known way to turn the screen off.".into()));
                }
                if theme::secondary(ui, "Open folder", self.dir.is_some()).clicked() {
                    if let Some(d) = &self.dir {
                        let _ = open::that(d);
                    }
                }
            });
            if let Some((l, m)) = &self.msg {
                ui.add_space(6.0);
                theme::status(ui, *l, m.clone());
            }
        });

        ui.add_space(20.0);
        theme::h2(ui, "Settings for the next recording");
        ui.add_space(4.0);
        ui.add_enabled_ui(!rec, |ui| {
            egui::Grid::new("recset").num_columns(2).spacing([24.0, 12.0]).show(ui, |ui| {
                ui.label("Length");
                ui.add(egui::Slider::new(&mut sh.set.night_hours, 0.5..=14.0).step_by(0.5).suffix(" h"));
                ui.end_row();
                ui.label("Part length");
                ui.add(egui::Slider::new(&mut sh.set.part_min, 1..=60).suffix(" min"));
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut sh.set.keep_awake, "Keep the computer awake").on_hover_text(
                    "Linux: systemd-inhibit (and Omarchy stay-awake). macOS: caffeinate. Windows: the recorder itself. Keep a laptop's lid open: closing it can still suspend.",
                );
                ui.end_row();
                ui.label("");
                ui.checkbox(&mut sh.set.screen_off, "Turn the screen off once data flows");
                ui.end_row();
            });
        });
        ui.add_space(20.0);
        theme::h2(ui, "Recorder output");
        ui.add_space(4.0);
        theme::log_view(ui, "night-out", &self.log.lines(), 280.0);
    }
}

fn hours(h: f64) -> String {
    format!("{} h {:02} min", h as u64, ((h.fract()) * 60.0) as u64)
}

/// "HH:MM:SS  1.23 h  99.8 frames/s  lost  0.1%  on disk   12.3 MB (raw ...)  free  100.0 GB"
fn parse_status(line: &str) -> Option<Stats> {
    let w: Vec<&str> = line.split_whitespace().collect();
    let at = |k: &str| w.iter().position(|x| *x == k);
    let h = at("h")?;
    let fr = at("frames/s")?;
    let lost = at("lost")?;
    let disk = at("disk")?;
    let free = at("free")?;
    Some(Stats {
        elapsed_h: w.get(h.checked_sub(1)?)?.parse().ok(),
        fps: w.get(fr.checked_sub(1)?)?.to_string(),
        lost: w.get(lost + 1)?.trim_end_matches('%').to_string(),
        disk: w.get(disk + 1)?.to_string(),
        free: w.get(free + 1)?.to_string(),
    })
}
