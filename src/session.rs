//! A finished breath test: the dashboard.analyze equivalent. Writes <stamp>.json, -breaths.csv,
//! -report.txt and appends to sessions.csv / tap_sessions.csv (same columns as the Python tools).

use crate::analysis::{self as an, Report, TapReport};
use anyhow::Result;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const SESSIONS_HEADER: &str = "stamp,secs,breaths_counted,ref_bpm,csi_median_bpm,mae,ref_hr,csi_hr";

pub struct Outcome {
    pub stamp: String,
    pub secs: f64,
    pub count: usize,
    pub reference: f64,
    pub text: String,
    pub report: Option<Report>,
    pub tap_text: String,
    pub tap: Option<TapReport>,
}

/// Append a row, starting the file (or rotating it aside if its columns changed).
fn append_csv(path: &Path, header: &str, row: &str) -> Result<()> {
    if let Ok(s) = std::fs::read_to_string(path) {
        if s.lines().next().map(str::trim) != Some(header) {
            let aside = path.with_file_name(format!(
                "{}-until-{}.csv",
                path.file_stem().unwrap().to_string_lossy(),
                chrono::Local::now().format("%Y%m%d-%H%M%S")
            ));
            std::fs::rename(path, aside)?;
        }
    }
    let new = !path.exists();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    if new {
        write!(f, "{header}\r\n")?;
    }
    write!(f, "{row}\r\n")?;
    Ok(())
}

pub fn stamp_path(results: &Path, stamp: &str, suffix: &str) -> PathBuf {
    results.join(format!("{stamp}{suffix}"))
}

/// Analyse results/<stamp>.log: `count` = breaths the user counted, `taps` = host times of the
/// inhale taps, (`seq`, `host`) = host clock of every captured line.
pub fn analyse(results: &Path, stamp: &str, secs: f64, count: usize, taps: &[f64], seq: &[i64], host: &[f64], ref_hr: Option<f64>) -> Result<Outcome> {
    let log = stamp_path(results, stamp, ".log");
    let csv = stamp_path(results, stamp, "-breaths.csv");
    let reference = count as f64 * 60.0 / secs;
    let rec = an::parse(&log.to_string_lossy());
    let (mut text, report) = match rec.as_ref().map_err(|e| anyhow::anyhow!("{e}")).and_then(|r| an::report(r, Some(reference), ref_hr, false)) {
        Ok(r) => {
            an::write_breaths_csv(&r.breaths, &csv.to_string_lossy())?;
            (r.text.clone(), Some(r))
        }
        Err(e) => (format!("{e}\n"), None),
    };
    let j = serde_json::json!({"taps": taps, "seq": seq, "host": host});
    std::fs::write(stamp_path(results, stamp, ".json"), serde_json::to_vec(&j)?)?;
    let (mut tap_text, mut tap) = (String::new(), None);
    if taps.len() >= 4 {
        if let Ok(rec) = &rec {
            match an::tap_report(rec, taps, seq, host) {
                Ok(t) => {
                    an::write_breaths_csv(&t.breaths, &csv.to_string_lossy())?;
                    tap_text = t.text.clone();
                    tap = Some(t);
                }
                Err(e) => tap_text = format!("{e}\n"),
            }
        }
    }
    if let Some(t) = &tap {
        let header = ["stamp", "secs"].iter().map(|s| s.to_string()).chain(t.row.iter().map(|(k, _)| k.clone())).collect::<Vec<_>>().join(",");
        let row = [stamp.to_string(), an::pyf(secs).trim_end_matches(".0").into()]
            .into_iter()
            .chain(t.row.iter().map(|(_, v)| v.clone()))
            .collect::<Vec<_>>()
            .join(",");
        append_csv(&results.join("tap_sessions.csv"), &header, &row)?;
    }
    let (est, mae, hr) = match &report {
        Some(r) => (
            r.est.map(|v| format!("{v:.1}")).unwrap_or_else(|| "none".into()),
            r.mae.map(|v| format!("{v:.2}")).unwrap_or_default(),
            r.hr.map(|v| format!("{v:.0}")).unwrap_or_default(),
        ),
        None => (String::new(), String::new(), String::new()),
    };
    let row = format!(
        "{stamp},{},{count},{reference:.2},{est},{mae},{},{hr}",
        an::pyf(secs).trim_end_matches(".0"),
        ref_hr.map(|v| an::pyf(v).trim_end_matches(".0").to_string()).unwrap_or_default()
    );
    append_csv(&results.join("sessions.csv"), SESSIONS_HEADER, &row)?;
    if !tap_text.is_empty() {
        text.push_str("\n\n=== single breaths vs taps ===\n");
        text.push_str(&tap_text);
    }
    std::fs::write(stamp_path(results, stamp, "-report.txt"), &text)?;
    Ok(Outcome { stamp: stamp.into(), secs, count, reference, text, report, tap_text, tap })
}

/// One row of sessions.csv.
#[derive(Clone, Debug, Default)]
pub struct Past {
    pub stamp: String,
    pub secs: String,
    pub counted: String,
    pub reference: String,
    pub csi: String,
    pub mae: String,
}

pub fn history(results: &Path) -> Vec<Past> {
    let Ok(s) = std::fs::read_to_string(results.join("sessions.csv")) else { return vec![] };
    let mut v: Vec<Past> = s
        .lines()
        .skip(1)
        .filter_map(|l| {
            let c: Vec<&str> = l.trim().split(',').collect();
            (c.len() >= 6).then(|| Past {
                stamp: c[0].into(),
                secs: c[1].into(),
                counted: c[2].into(),
                reference: c[3].into(),
                csi: c[4].into(),
                mae: c[5].into(),
            })
        })
        .collect();
    v.reverse();
    v
}

/// breath_analyze.selftest: synthetic 100 Hz links at 15 and 21 BPM, analysed in memory.
pub fn selftest() -> String {
    use rand::{Rng, SeedableRng};
    use rand_distr_like::normal;
    let mut rng = rand::rngs::StdRng::seed_from_u64(0);
    let (nsc, n) = (114usize, 100 * 120);
    let mut out = String::new();
    let cplx = |rng: &mut rand::rngs::StdRng, s: f64| (0..nsc).map(|_| (normal(rng) * s, normal(rng) * s)).collect::<Vec<_>>();
    let stat = cplx(&mut rng, 40.0);
    let refl = cplx(&mut rng, 3.0);
    for (label, f) in [("15 BPM", 0.25), ("21 BPM", 0.35)] {
        let mut log = Vec::with_capacity(n * 1200);
        for i in 0..n {
            let t = i as f64 / 100.0;
            let breath = (2.0 * std::f64::consts::PI * f * t + 0.3 * (2.0 * std::f64::consts::PI * 0.02 * t).sin()).sin();
            let dph = 2.0 * breath;
            let cfo: f64 = rng.random_range(0.0..std::f64::consts::TAU);
            let slope = normal(&mut rng) * 0.05;
            let mut v = Vec::with_capacity(2 * nsc);
            for k in 0..nsc {
                let a = dph * (1.0 + k as f64 / nsc as f64);
                let (rr, ri) = refl[k];
                let (mut re, mut im) = (stat[k].0 + rr * a.cos() - ri * a.sin(), stat[k].1 + rr * a.sin() + ri * a.cos());
                let p = cfo + slope * k as f64;
                (re, im) = (re * p.cos() - im * p.sin(), re * p.sin() + im * p.cos());
                re += normal(&mut rng);
                im += normal(&mut rng);
                v.push(im.clamp(-127.0, 127.0) as i32);
                v.push(re.clamp(-127.0, 127.0) as i32);
            }
            let body = v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",");
            let _ = writeln!(log, "CSI_DATA,{i},1a:00:00:00:00:00,-50,11,-95,0,0,161,{},0,1,{},0,\"[{body}]\"", (t * 1e6) as i64, 2 * nsc);
        }
        out.push_str(&format!("--- synthetic {label} ---\n"));
        match an::parse_bytes(&log).and_then(|r| an::report(&r, Some(f * 60.0), None, false)) {
            Ok(r) => out.push_str(&r.text),
            Err(e) => out.push_str(&format!("{e}\n")),
        }
    }
    out
}

mod rand_distr_like {
    /// Standard normal (Box-Muller); keeps us off an extra crate.
    pub fn normal(rng: &mut impl rand::Rng) -> f64 {
        let u: f64 = rng.random_range(f64::EPSILON..1.0);
        let v: f64 = rng.random();
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}
