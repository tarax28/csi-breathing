// No console window on Windows for the app; the subcommands still print when run from a terminal.
#![cfg_attr(windows, windows_subsystem = "windows")]

use csi_breathing::analysis as an;
use std::path::PathBuf;

const USAGE: &str = "csi-breathing: WiFi CSI breathing monitor

usage:
  csi-breathing                         open the app
  csi-breathing analyze <log> [--ref-bpm X] [--ref-hr X] [--breaths out.csv]
  csi-breathing taps <results/stamp>    breath test vs its taps
  csi-breathing night <night-folder> [--epoch MIN] [--no-health] [--refresh-health]
  csi-breathing record-night --port P [--hours H] [--part-min M] [--min-free-gb G] [--results DIR]
  csi-breathing selftest";

fn arg(args: &[String], k: &str) -> Option<String> {
    args.iter().position(|a| a == k).and_then(|i| args.get(i + 1).cloned())
}
fn num(args: &[String], k: &str, d: f64) -> anyhow::Result<f64> {
    arg(args, k).map(|v| v.parse::<f64>().map_err(|_| anyhow::anyhow!("{k} needs a number, got {v}"))).unwrap_or(Ok(d))
}
fn need(args: &[String], i: usize) -> anyhow::Result<String> {
    args.get(i).cloned().ok_or_else(|| anyhow::anyhow!("{USAGE}"))
}

#[cfg(windows)]
fn attach_console() {
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS);
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(windows)]
    if args.len() > 1 && args[1] != "record-night" {
        attach_console();
    }
    match args.get(1).map(|s| s.as_str()) {
        None => csi_breathing::gui::run().map_err(|e| anyhow::anyhow!("{e}"))?,
        Some("analyze") => {
            let rec = an::parse(&need(&args, 2)?)?;
            let r = an::report(&rec, arg(&args, "--ref-bpm").map(|v| v.parse()).transpose()?, arg(&args, "--ref-hr").map(|v| v.parse()).transpose()?, false)?;
            print!("{}", r.text);
            if let Some(p) = arg(&args, "--breaths") {
                an::write_breaths_csv(&r.breaths, &p)?;
            }
        }
        Some("taps") => {
            let stamp = need(&args, 2)?;
            let j: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(format!("{stamp}.json"))?)?;
            let f = |k: &str| j[k].as_array().map(|a| a.iter().filter_map(|v| v.as_f64()).collect::<Vec<f64>>()).unwrap_or_default();
            let seq: Vec<i64> = j["seq"].as_array().map(|a| a.iter().filter_map(|v| v.as_i64()).collect()).unwrap_or_default();
            let rec = an::parse(&format!("{stamp}.log"))?;
            let r = an::tap_report(&rec, &f("taps"), &seq, &f("host"))?;
            print!("{}", r.text);
            println!("{}", r.row.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "));
        }
        Some("night") => {
            let opt = csi_breathing::night::Options {
                epoch_min: num(&args, "--epoch", 5.0)?,
                health: !args.iter().any(|a| a == "--no-health"),
                refresh_health: args.iter().any(|a| a == "--refresh-health"),
                ..Default::default()
            };
            let t = std::time::Instant::now();
            let n = csi_breathing::night::analyse(std::path::Path::new(&need(&args, 2)?), &opt, &|l| {
                if !(l.starts_with("skipped ") && l.contains("malformed")) {
                    println!("{l}")
                }
            })?;
            eprintln!("total {:.1} s, {} breaths", t.elapsed().as_secs_f64(), n.b.len());
        }
        Some("record-night") => {
            let port = arg(&args, "--port").ok_or_else(|| anyhow::anyhow!("record-night needs --port"))?;
            csi_breathing::recorder::run(csi_breathing::recorder::Args {
                port,
                hours: num(&args, "--hours", 9.0)?,
                part_min: num(&args, "--part-min", 10.0)?,
                min_free_gb: num(&args, "--min-free-gb", 1.0)?,
                results: arg(&args, "--results").map(PathBuf::from).unwrap_or_else(csi_breathing::gui::default_results),
            })?;
        }
        Some("selftest") => print!("{}", csi_breathing::session::selftest()),
        Some("-h" | "--help" | "help") => println!("{USAGE}"),
        Some(o) => anyhow::bail!("unknown command {o}\n\n{USAGE}"),
    }
    Ok(())
}
