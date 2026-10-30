//! Port of record_night.py: record the RX for hours into compressed 10-minute parts.
//! Same files and log lines (the GUI and night.sh read "recording to", "DATA FLOWING" and the
//! once-a-minute status line). Runs as `csi-breathing record-night ...`, normally started
//! detached by the GUI. Stop: Ctrl-C/SIGTERM, or a file named STOP in the output folder.
//! An interruption (the computer slept, the port failed, no data) prints "DATA STOPPED" and "DATA BACK",
//! goes into night.json as a gap, and starts a new part, so that each part has one chip clock.

use crate::hw;
use anyhow::Result;
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::json;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static STOP: AtomicBool = AtomicBool::new(false);

/// The read loop turns over at least every 200 ms: a longer jump of the wall clock means this process did
/// not run (the computer slept, Modern Standby suspended it), which no serial call reports by itself.
const FROZEN_S: f64 = 10.0;
/// After `stall_s` without data the port is reopened, then at growing intervals up to this.
const MAX_STALL_S: f64 = 300.0;

fn stamp(fmt: &str) -> String {
    chrono::Local::now().format(fmt).to_string()
}

macro_rules! say {
    ($($a:tt)*) => {{ println!($($a)*); let _ = std::io::stdout().flush(); }};
}

pub struct Args {
    pub port: String,
    pub hours: f64,
    pub part_min: f64,
    pub min_free_gb: f64,
    pub results: PathBuf,
    pub stall_s: f64,
    pub keep_awake: bool,
}

fn mins(s: f64) -> String {
    if s < 90.0 { format!("{s:.0} s") } else { format!("{:.1} min", s / 60.0) }
}

fn close_gap(summary: &mut serde_json::Value, (from, why): (f64, &str), to: f64, back: bool) {
    let clock = |t: f64| crate::night::clock(t, "%H:%M:%S");
    summary["gaps"].as_array_mut().unwrap().push(json!({"from": clock(from), "to": clock(to), "from_epoch": from, "to_epoch": to, "s": to - from, "why": why}));
    if back {
        say!("{}  DATA BACK after {} ({why})", stamp("%H:%M:%S"), mins(to - from));
    } else {
        say!("{}  no data for the last {} of the recording ({why})", stamp("%H:%M:%S"), mins(to - from));
    }
}

fn dir_size(d: &Path) -> u64 {
    std::fs::read_dir(d).map(|r| r.filter_map(|e| e.ok()).filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0)
}

pub fn run(a: Args) -> Result<()> {
    #[cfg(unix)]
    unsafe {
        extern "C" fn on_sig(_: libc::c_int) {
            STOP.store(true, Ordering::SeqCst);
        }
        libc::signal(libc::SIGINT, on_sig as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_sig as *const () as libc::sighandler_t);
    }
    let awake = a.keep_awake.then(hw::keep_awake);
    if matches!(awake, Some(None)) {
        say!("{}  could not ask the system to stay awake: it may sleep during the recording", stamp("%H:%M:%S"));
    }
    let start = hw::now();
    let out = a.results.join(format!("night-{}", stamp("%Y%m%d-%H%M%S")));
    std::fs::create_dir_all(&out)?;
    let stop_file = out.join("STOP");
    let stopped = || STOP.load(Ordering::SeqCst) || stop_file.exists();
    let end_at = if a.hours > 0.0 { start + a.hours * 3600.0 } else { f64::INFINITY };
    let mut summary = json!({"port": a.port, "start": stamp("%Y-%m-%d %H:%M:%S"), "start_epoch": start,
        "part_min": a.part_min, "parts": [], "gaps": [], "reconnects": 0});
    let save = |summary: &mut serde_json::Value| {
        summary["end"] = json!(stamp("%Y-%m-%d %H:%M:%S"));
        summary["end_epoch"] = json!(hw::now());
        let parts = summary["parts"].as_array().cloned().unwrap_or_default();
        summary["frames"] = json!(parts.iter().map(|p| p["frames"].as_u64().unwrap_or(0)).sum::<u64>());
        summary["lost"] = json!(parts.iter().map(|p| p["lost"].as_u64().unwrap_or(0)).sum::<u64>());
        let _ = std::fs::write(out.join("night.json"), serde_json::to_vec_pretty(summary).unwrap());
    };
    let open_port = || -> Option<Box<dyn serialport::SerialPort>> {
        let mut said = 0.0;
        while !stopped() && hw::now() < end_at {
            match hw::open(&a.port, 200) {
                Ok(s) => {
                    if said > 0.0 {
                        say!("{}  RX is back", stamp("%H:%M:%S"));
                    }
                    return Some(s);
                }
                Err(_) => {
                    if hw::now() - said >= 60.0 {
                        say!("{}  RX not found ({}), waiting: is it plugged in?", stamp("%H:%M:%S"), a.port);
                        said = hw::now();
                    }
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
        }
        None
    };
    say!(
        "recording to {}  ({}, {} min per part)",
        out.display(),
        if a.hours > 0.0 { format!("{} h", crate::analysis::pyf(a.hours).trim_end_matches(".0")) } else { "until stopped".into() },
        crate::analysis::pyf(a.part_min).trim_end_matches(".0")
    );
    let mut s = open_port();
    let mut part_no = 0;
    let mut buf: Vec<u8> = vec![];
    let mut chunk = vec![0u8; 65536];
    let (mut last_seq, mut last_clock, mut last_status, mut flowing) = (None::<i64>, 0.0, hw::now(), false);
    let (mut tot_frames, mut tot_lost, mut tot_raw) = (0u64, 0u64, 0u64);
    let mut gap: Option<(f64, &str)> = None;
    let (mut last_tick, mut last_data, mut stall_after) = (hw::now(), None::<f64>, a.stall_s);
    while !stopped() && hw::now() < end_at && s.is_some() {
        part_no += 1;
        let name = format!("part-{part_no:04}");
        let (mut frames, mut lost, mut raw) = (0u64, 0u64, 0u64);
        let pstart = stamp("%H:%M:%S");
        let part_end = hw::now() + a.part_min * 60.0;
        {
            let mut gz = GzEncoder::new(std::io::BufWriter::new(std::fs::File::create(out.join(format!("{name}.log.gz")))?), Compression::new(6));
            let mut clk = std::io::BufWriter::new(std::fs::File::create(out.join(format!("{name}.clock.csv")))?);
            writeln!(clk, "seq,host_time")?;
            while !stopped() && hw::now() < part_end.min(end_at) {
                let read = s.as_mut().unwrap().read(&mut chunk);
                let now = hw::now();
                let frozen_from = (now - last_tick > FROZEN_S).then_some(last_tick);
                last_tick = now;
                let (n, serial_err) = match read {
                    Ok(n) => (n, None),
                    Err(e) if e.kind() == std::io::ErrorKind::TimedOut => (0, None),
                    Err(e) => (0, Some(e)),
                };
                let silent = last_data.filter(|&t| now - t > stall_after);
                if frozen_from.is_some() || serial_err.is_some() || silent.is_some() {
                    let clock = stamp("%H:%M:%S");
                    let (from, why) = if let Some(t) = frozen_from {
                        let was = crate::night::clock(t, "%H:%M:%S");
                        say!("{clock}  DATA STOPPED: the computer slept or suspended this program from {was} ({}), nothing was recorded", mins(now - t));
                        (t, "computer asleep")
                    } else if let Some(e) = &serial_err {
                        say!("{clock}  DATA STOPPED: serial error: {e}; reconnecting");
                        (last_data.unwrap_or(now), "serial error")
                    } else if gap.is_none() {
                        say!("{clock}  DATA STOPPED: nothing for {}; reopening the port", mins(now - silent.unwrap()));
                        (silent.unwrap(), "no data")
                    } else {
                        say!("{clock}  still no data; reopening the port");
                        (now, "no data")
                    };
                    let first = gap.is_none();
                    gap.get_or_insert((from, why));
                    summary["reconnects"] = json!(summary["reconnects"].as_u64().unwrap_or(0) + 1);
                    buf.clear();
                    last_seq = None; // what went past during the gap is the gap, not packet loss
                    drop(s.take()); // close first: a port stays exclusive while it is open
                    s = open_port();
                    last_tick = hw::now();
                    stall_after = if first { a.stall_s } else { (stall_after * 2.0).min(MAX_STALL_S) };
                    last_data = last_data.map(|_| last_tick);
                    if first || s.is_none() {
                        break;
                    }
                    continue;
                }
                buf.extend_from_slice(&chunk[..n]);
                while let Some(i) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=i).collect();
                    let line = &line[..line.len() - 1];
                    if !line.starts_with(b"CSI_DATA") {
                        continue;
                    }
                    gz.write_all(line)?;
                    gz.write_all(b"\n")?;
                    frames += 1;
                    last_data = Some(now);
                    stall_after = a.stall_s;
                    if let Some(g) = gap.take() {
                        close_gap(&mut summary, g, now, true);
                        save(&mut summary);
                    }
                    raw += line.len() as u64 + 1;
                    if !flowing && frames >= 100 {
                        flowing = true;
                        say!("{}  DATA FLOWING", stamp("%H:%M:%S"));
                    }
                    let seq = std::str::from_utf8(line).ok().and_then(|l| l.split(',').nth(1)).and_then(|v| v.parse::<i64>().ok());
                    let Some(seq) = seq else { continue };
                    if let Some(ls) = last_seq {
                        if seq > ls {
                            lost += (seq - ls - 1) as u64;
                        }
                    }
                    last_seq = Some(seq);
                    if now - last_clock >= 1.0 {
                        writeln!(clk, "{seq},{now:.3}")?;
                        last_clock = now;
                    }
                }
                if now - last_status >= 60.0 {
                    last_status = now;
                    let el = now - start;
                    let fr = tot_frames + frames;
                    let lo = tot_lost + lost;
                    let free = hw::free_space(&out).map(|b| b as f64 / 1e9).unwrap_or(f64::NAN);
                    say!(
                        "{}  {:5.2} h  {:5.1} frames/s  lost {:4.1}%  on disk {:7.1} MB (raw {:7.1} MB)  free {:5.1} GB",
                        stamp("%H:%M:%S"),
                        el / 3600.0,
                        fr as f64 / el,
                        100.0 * lo as f64 / (fr + lo).max(1) as f64,
                        dir_size(&out) as f64 / 1e6,
                        (tot_raw + raw) as f64 / 1e6,
                        free
                    );
                    if free < a.min_free_gb {
                        say!("less than {} GB free: stopping", a.min_free_gb);
                        STOP.store(true, Ordering::SeqCst);
                    }
                }
            }
            gz.finish()?.flush()?;
            clk.flush()?;
        }
        let gzsz = std::fs::metadata(out.join(format!("{name}.log.gz"))).map(|m| m.len()).unwrap_or(0);
        tot_frames += frames;
        tot_lost += lost;
        tot_raw += raw;
        summary["parts"].as_array_mut().unwrap().push(json!({"file": format!("{name}.log.gz"), "start": pstart,
            "frames": frames, "lost": lost, "bytes_raw": raw, "end": stamp("%H:%M:%S"), "bytes_gz": gzsz}));
        save(&mut summary);
        say!("{}  {name}: {frames} frames, {lost} lost, {:.1} MB raw -> {:.1} MB gz", stamp("%H:%M:%S"), raw as f64 / 1e6, gzsz as f64 / 1e6);
    }
    if let Some(g) = gap.take() {
        close_gap(&mut summary, g, hw::now(), false);
    }
    save(&mut summary);
    let el = hw::now() - start;
    let gaps = summary["gaps"].as_array().map(|g| (g.len(), g.iter().filter_map(|x| x["s"].as_f64()).sum::<f64>())).unwrap_or_default();
    say!(
        "done: {:.2} h, {} frames ({:.1}/s), {} lost, {}{} parts in {}",
        el / 3600.0,
        tot_frames,
        tot_frames as f64 / el.max(1.0),
        tot_lost,
        if gaps.0 > 0 { format!("{} interruptions ({} without data), ", gaps.0, mins(gaps.1)) } else { String::new() },
        part_no,
        out.display()
    );
    Ok(())
}
