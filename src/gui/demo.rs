//! Scripted runs for checking the app's look without clicking through it (development aid).
//!
//!   CSI_DEMO=<page>[:<action>] CSI_SHOT=out.png [CSI_THEME=dark|light] [CSI_SIZE=WxH] [CSI_SCALE=%] csi-breathing
//!
//! pages: breath, record, nights, fitbit, boards, tools. Actions: breath:past (open the newest
//! past test with taps), breath:run (replay CSI_REPLAY=<stamp>.log with its recorded taps and
//! shoot at CSI_AT seconds), nights:analyse (open CSI_NIGHT or the newest multi-part night,
//! analysing it if needed). The app saves a screenshot of its window to CSI_SHOT and quits.
//! CSI_SIZE is the window size in points, CSI_SCALE the interface size (Settings::ui_scale).
//! A demo run keeps its settings and window state in a file of its own in the temp folder.

use super::{App, Page};
use eframe::egui;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub struct Demo {
    page: Page,
    action: String,
    shot: PathBuf,
    started: Instant,
    ready_at: Option<Instant>,
    asked: bool,
    taps: Vec<f64>,
    at: f64,
    began: bool,
}

impl Demo {
    pub fn from_env() -> Option<Demo> {
        let spec = std::env::var("CSI_DEMO").ok()?;
        let (page, action) = spec.split_once(':').unwrap_or((&spec, ""));
        let page = match page {
            "record" => Page::Record,
            "nights" => Page::Nights,
            "fitbit" => Page::Fitbit,
            "boards" => Page::Boards,
            "tools" => Page::Tools,
            _ => Page::Breath,
        };
        Some(Demo {
            page,
            action: action.to_string(),
            shot: std::env::var("CSI_SHOT").map(PathBuf::from).unwrap_or_else(|_| "csi-shot.png".into()),
            started: Instant::now(),
            ready_at: None,
            asked: false,
            taps: vec![],
            at: std::env::var("CSI_AT").ok().and_then(|v| v.parse().ok()).unwrap_or(40.0),
            began: false,
        })
    }
}

pub fn step(app: &mut App, ctx: &egui::Context) {
    let Some(mut d) = app.demo.take() else { return };
    ctx.request_repaint_after(Duration::from_millis(100));
    if !d.began {
        d.began = true;
        app.page = d.page;
        if let Some(s) = std::env::var("CSI_SCALE").ok().and_then(|v| v.parse().ok()) {
            app.sh.set.ui_scale = s;
        }
        match (d.page, d.action.as_str()) {
            (Page::Breath, "past") => app.breath.demo_open_past(ctx, &app.sh),
            (Page::Breath, "run") => {
                if let Ok(r) = std::env::var("CSI_REPLAY") {
                    let j: Option<serde_json::Value> = std::fs::read_to_string(r.replace(".log", ".json")).ok().and_then(|s| serde_json::from_str(&s).ok());
                    if let Some(j) = j {
                        let host = j["host"].as_array().and_then(|a| a.first()?.as_f64()).unwrap_or(0.0);
                        d.taps = j["taps"].as_array().map(|a| a.iter().filter_map(|v| Some(v.as_f64()? - host)).collect()).unwrap_or_default();
                    }
                    app.sh.replay = Some(PathBuf::from(r));
                }
                app.breath.demo_start(&mut app.sh);
            }
            (Page::Nights, "analyse") => app.nights.demo_open(ctx, &app.sh, std::env::var("CSI_NIGHT").ok()),
            _ => {}
        }
    }
    let ready = match (d.page, d.action.as_str()) {
        (Page::Breath, "run") => {
            let el = app.breath.demo_elapsed();
            if let Some(el) = el {
                while d.taps.first().is_some_and(|t| *t <= el) {
                    d.taps.remove(0);
                    app.breath.demo_tap();
                }
            }
            el.is_some_and(|e| e >= d.at)
        }
        (Page::Breath, "past") => app.breath.demo_idle(),
        (Page::Nights, _) => app.nights.demo_idle(),
        _ => d.started.elapsed() > Duration::from_secs(2),
    };
    if ready && d.ready_at.is_none() {
        d.ready_at = Some(Instant::now());
    }
    if d.ready_at.is_some_and(|t| t.elapsed() > Duration::from_millis(1500)) && !d.asked {
        d.asked = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
    }
    let shot = ctx.input(|i| {
        i.raw.events.iter().find_map(|e| match e {
            egui::Event::Screenshot { image, .. } => Some(image.clone()),
            _ => None,
        })
    });
    if let Some(img) = shot {
        match write_png(&d.shot, &img) {
            Ok(()) => eprintln!("saved {}", d.shot.display()),
            Err(e) => eprintln!("could not save {}: {e}", d.shot.display()),
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    if d.started.elapsed() > Duration::from_secs(240) {
        eprintln!("demo timed out");
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    app.demo = Some(d);
}

/// Minimal RGBA PNG writer (no extra crates).
pub fn write_png(path: &std::path::Path, img: &egui::ColorImage) -> std::io::Result<()> {
    let [w, h] = img.size;
    let mut raw = Vec::with_capacity(h * (1 + 4 * w));
    for y in 0..h {
        raw.push(0u8);
        for c in &img.pixels[y * w..(y + 1) * w] {
            raw.extend_from_slice(&c.to_srgba_unmultiplied());
        }
    }
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(6));
    z.write_all(&raw)?;
    let idat = z.finish()?;
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut chunk = |kind: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = flate2::Crc::new();
        c.update(kind);
        c.update(data);
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&c.sum().to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &idat);
    chunk(b"IEND", &[]);
    std::fs::write(path, out)
}
