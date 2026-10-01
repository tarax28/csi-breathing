//! Port of night_analyze.py: a record_night.py folder -> whole-night breathing analysis,
//! optionally compared with a wearable's sleep stages (Google Health API).
//!
//! Differences from the Python: no on-disk caches (parsing is fast enough), the movement
//! profile and the spectrogram are computed in double precision (numpy/scipy did them in
//! float32), and the circular-shift / chance-level tests use a different random generator,
//! so those two Monte Carlo numbers vary slightly.

use crate::analysis::{self as an, Breaths, CMat, FS};
use crate::dsp;
use crate::health;
use anyhow::{Result, bail};
use chrono::{Local, TimeZone};
use rayon::prelude::*;
use rustfft::num_complex::Complex64 as C64;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub const CHUNK_S: f64 = 600.0;
pub const PAD_S: f64 = 60.0;
pub const GOOD_FRAC: f64 = 0.7;
pub const GOOD_MIN: usize = 20;
pub const SHIFT_W: usize = 10;
pub const SHIFT_ON: f64 = 7.5; // 4.5 / 2.0 before the amplitude-first resample (see night_analyze.py)
pub const SHIFT_OFF: f64 = 3.5;
pub const TRACK_LAMBDA: f64 = 1.0;
pub const REG_MIN: f64 = 0.45;
pub const STAGES: [&str; 4] = ["deep", "light", "rem", "wake"];

pub fn clock(t: f64, fmt: &str) -> String {
    Local.timestamp_opt(t.floor() as i64, 0).single().map(|d| d.format(fmt).to_string()).unwrap_or_default()
}

// ----------------------------------------------------------------- timeline
/// All parts on one FS grid: complex f32, column-major (m subcarriers of n samples).
pub struct Timeline {
    pub t0: f64,
    pub n: usize,
    pub m: usize,
    pub g: Vec<[f32; 2]>,
}

impl Timeline {
    pub fn rows(&self, a: usize, b: usize) -> CMat {
        let n = b - a;
        let mut d = Vec::with_capacity(n * self.m);
        for j in 0..self.m {
            d.extend(self.g[j * self.n + a..j * self.n + b].iter().map(|v| C64::new(v[0] as f64, v[1] as f64)));
        }
        CMat { n, m: self.m, d }
    }
}

fn read_clock(path: &Path) -> Result<(Vec<i64>, Vec<f64>)> {
    let s = std::fs::read_to_string(path)?;
    let (mut seq, mut host) = (vec![], vec![]);
    for l in s.lines().skip(1) {
        if let Some((a, b)) = l.split_once(',') {
            if let (Ok(a), Ok(b)) = (a.trim().parse::<f64>(), b.trim().parse::<f64>()) {
                seq.push(a as i64);
                host.push(b);
            }
        }
    }
    Ok((seq, host))
}

struct Part {
    off: f64,
    n: usize,
    m: usize,
    h: Vec<[f32; 2]>,
    note: Option<String>,
}

fn load_part(path: &Path) -> Result<Part> {
    let mut rec = an::parse(path.to_str().unwrap())?;
    let gaps: Vec<usize> = rec.t.windows(2).enumerate().filter(|(_, w)| w[1] - w[0] > 2.0).map(|(i, _)| i).collect();
    let lead: Vec<usize> = gaps.iter().copied().filter(|&g| (g as f64) < 0.05 * rec.npkt() as f64).collect();
    if let Some(&k) = lead.last() {
        rec.drop_head(k + 1);
    }
    let rest: Vec<f64> = rec.t.windows(2).map(|w| w[1] - w[0]).filter(|&d| d > 2.0).collect();
    let note = (!rest.is_empty()).then(|| {
        format!(
            "  {}: {} gaps > 2 s inside the part (longest {:.1} s), interpolated",
            path.file_name().unwrap().to_string_lossy(),
            rest.len(),
            rest.iter().cloned().fold(0.0, f64::max)
        )
    });
    let (cs, ch) = read_clock(&PathBuf::from(path.to_str().unwrap().replace(".log.gz", ".clock.csv")))?;
    let off = an::chip_offset(&rec.t, &rec.seq, &cs, &ch).ok_or_else(|| anyhow::anyhow!("{}: no clock match", path.display()))?;
    let (_, hr) = an::resample(&rec);
    let h = hr.d.iter().map(|c| [c.re as f32, c.im as f32]).collect();
    Ok(Part { off, n: hr.n, m: hr.m, h, note })
}

pub fn parts(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|r| {
            r.filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    let n = p.file_name().unwrap().to_string_lossy().to_string();
                    n.starts_with("part-") && n.ends_with(".log.gz")
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn timeline(dir: &Path, log: &(dyn Fn(String) + Sync)) -> Result<Timeline> {
    let ps = parts(dir);
    if ps.is_empty() {
        bail!("no part-*.log.gz in {}", dir.display());
    }
    log(format!("parsing {} parts ...", ps.len()));
    let t_ = std::time::Instant::now();
    let res: Vec<Result<Part>> = ps.par_iter().map(|p| load_part(p)).collect();
    let mut parts = vec![];
    for r in res {
        let p = r?;
        if let Some(n) = &p.note {
            log(n.clone());
        }
        parts.push(p);
    }
    log(format!("  parsed in {:.0} s", t_.elapsed().as_secs_f64()));
    let m = parts[0].m;
    if parts.iter().any(|p| p.m != m) {
        bail!("parts have different subcarrier counts");
    }
    let t0 = parts[0].off;
    let last = parts.last().unwrap();
    let n = ((last.off - t0) * FS).round_ties_even() as usize + last.n;
    let mut g = vec![[f32::NAN, f32::NAN]; n * m];
    for p in &parts {
        let i = ((p.off - t0) * FS).round_ties_even() as usize;
        let len = p.n.min(n - i);
        for j in 0..m {
            g[j * n + i..j * n + i + len].copy_from_slice(&p.h[j * p.n..j * p.n + len]);
        }
    }
    let miss: Vec<usize> = (0..n).filter(|&i| g[i][0].is_nan()).collect();
    if !miss.is_empty() {
        log(format!("  {:.1} s missing between parts, interpolated", miss.len() as f64 / FS));
        let good: Vec<f64> = (0..n).filter(|&i| !g[i][0].is_nan()).map(|i| i as f64).collect();
        let missf: Vec<f64> = miss.iter().map(|&i| i as f64).collect();
        for j in 0..m {
            let re: Vec<f64> = good.iter().map(|&i| g[j * n + i as usize][0] as f64).collect();
            let im: Vec<f64> = good.iter().map(|&i| g[j * n + i as usize][1] as f64).collect();
            let a = dsp::interp(&missf, &good, &re);
            let b = dsp::interp(&missf, &good, &im);
            for (k, &i) in miss.iter().enumerate() {
                g[j * n + i] = [a[k] as f32, b[k] as f32];
            }
        }
    }
    Ok(Timeline { t0, n, m, g })
}

// ----------------------------------------------------------------- movement
pub fn profile_shift(tl: &Timeline) -> Vec<f64> {
    let step = FS as usize;
    let n = tl.n / step;
    let m = tl.m;
    // P[t][k]: log of the per-second median of each subcarrier's amplitude / packet mean amplitude
    let rows: Vec<Vec<f64>> = (0..n)
        .into_par_iter()
        .map(|t| {
            let mut cols = vec![Vec::with_capacity(step); m];
            for s in t * step..(t + 1) * step {
                let amps: Vec<f64> = (0..m).map(|j| {
                    let v = tl.g[j * tl.n + s];
                    ((v[0] as f64).powi(2) + (v[1] as f64).powi(2)).sqrt()
                }).collect();
                let mean = amps.iter().sum::<f64>() / m as f64 + 1e-9;
                for j in 0..m {
                    cols[j].push(amps[j] / mean);
                }
            }
            cols.iter().map(|c| (dsp::median(c) + 1e-3).ln()).collect()
        })
        .collect();
    let keep: Vec<usize> = (0..m).filter(|&j| rows.iter().map(|r| r[j]).sum::<f64>() / n as f64 > -3.0).collect();
    let mut d = vec![0.0; n];
    if n > 2 * SHIFT_W {
        let vals: Vec<f64> = (SHIFT_W..n - SHIFT_W)
            .into_par_iter()
            .map(|t| {
                let per: Vec<f64> = keep
                    .iter()
                    .map(|&j| {
                        let a: Vec<f64> = (t - SHIFT_W..t).map(|i| rows[i][j]).collect();
                        let b: Vec<f64> = (t..t + SHIFT_W).map(|i| rows[i][j]).collect();
                        let (ma, mb) = (dsp::median(&a), dsp::median(&b));
                        let sa = dsp::median(&a.iter().map(|v| (v - ma).abs()).collect::<Vec<_>>());
                        let sb = dsp::median(&b.iter().map(|v| (v - mb).abs()).collect::<Vec<_>>());
                        (ma - mb).abs() / (0.5 * (sa + sb) + 1e-4)
                    })
                    .collect();
                dsp::median(&per)
            })
            .collect();
        d[SHIFT_W..n - SHIFT_W].copy_from_slice(&vals);
    }
    d
}

/// (mask per FS sample, events (start s, end s, peak x median))
pub fn movement(tl: &Timeline) -> (Vec<bool>, Vec<(usize, usize, f64)>) {
    let d = profile_shift(tl);
    let inner = if d.len() > 2 * SHIFT_W { &d[SHIFT_W..d.len() - SHIFT_W] } else { &d[..] };
    let med = dsp::median(inner) + 1e-12;
    let r: Vec<f64> = d.iter().map(|v| v / med).collect();
    let above: Vec<u8> = r.iter().map(|&v| (v > SHIFT_OFF) as u8).collect();
    let ev: Vec<(usize, usize, f64)> = dsp::runs(&above)
        .into_iter()
        .map(|(lo, hi, _)| (lo, hi, r[lo..hi].iter().cloned().fold(f64::MIN, f64::max)))
        .filter(|e| e.2 > SHIFT_ON)
        .collect();
    let mut mask = vec![false; tl.n];
    for &(lo, hi, _) in &ev {
        for v in &mut mask[((lo as f64 * FS) as usize).min(tl.n)..((hi as f64 * FS) as usize).min(tl.n)] {
            *v = true;
        }
    }
    (mask, ev)
}

// ----------------------------------------------------------------- chunks
struct Chunk {
    b: Breaths,
    ft: Vec<f64>,
    fv: Vec<f64>,
    q: Vec<u8>,
    present: Vec<bool>,
    x: Vec<f32>,
}

fn analyse_chunk(tl: &Timeline, mot: &[bool], c0: f64) -> Chunk {
    let len = tl.n;
    let n0 = (c0 * FS) as usize;
    let n1 = (((c0 + CHUNK_S) * FS) as usize).min(len);
    let mut a = n0.saturating_sub((PAD_S * FS) as usize);
    let b = (n1 + (PAD_S * FS) as usize).min(len);
    if ((n1 - n0) as f64) < 60.0 * FS {
        a = n1.saturating_sub(((CHUNK_S + PAD_S) * FS) as usize);
    }
    let h = tl.rows(a, b);
    let (w, _, _) = an::breathing_waveform(&h, an::Method::Snr);
    let br = an::detect_breaths(&w, None, Some(&mot[a..b]));
    let (ft, fv) = an::fused_rate(&br);
    let s0 = a as f64 / FS;
    let (lo, hi) = (n0 as f64 / FS, n1 as f64 / FS);
    let keep: Vec<usize> = (0..br.len()).filter(|&i| br.rise[i] + s0 >= lo && br.rise[i] + s0 < hi).collect();
    let pick = |v: &Vec<f64>, add: f64| keep.iter().map(|&i| v[i] + add).collect::<Vec<f64>>();
    let bb = Breaths {
        tr: pick(&br.tr, s0),
        rise: pick(&br.rise, s0),
        pk: pick(&br.pk, s0),
        nxt: pick(&br.nxt, s0),
        slope: pick(&br.slope, s0),
        ttot: pick(&br.ttot, 0.0),
        ti: pick(&br.ti, 0.0),
        te: pick(&br.te, 0.0),
        ti_frac: pick(&br.ti_frac, 0.0),
        depth: pick(&br.depth, 0.0),
        ok: keep.iter().map(|&i| br.ok[i]).collect(),
        why: keep.iter().map(|&i| br.why[i]).collect(),
        ..Default::default()
    };
    let kf: Vec<usize> = (0..ft.len()).filter(|&i| ft[i] + s0 >= lo && ft[i] + s0 < hi).collect();
    let core = n0 - a..n1 - a;
    let med = dsp::median(&br.x[core.clone()].iter().map(|v| v.abs()).collect::<Vec<_>>()) + 1e-12;
    Chunk {
        b: bb,
        ft: kf.iter().map(|&i| ft[i] + s0).collect(),
        fv: kf.iter().map(|&i| fv[i]).collect(),
        q: br.q[core.clone()].to_vec(),
        present: br.present[core.clone()].to_vec(),
        x: br.x[core].iter().map(|v| (v / med) as f32).collect(),
    }
}

// ----------------------------------------------------------------- track / epochs
pub struct Track {
    pub t: Vec<f64>,
    pub bpm: Vec<f64>,
    pub reg: Vec<f64>,
    pub reg2: Vec<f64>,
    pub spec_bpm: Vec<f64>,
    /// normalised spectrum per column (time), rows = spec_bpm
    pub spec: Vec<Vec<f64>>,
}

pub fn track(x: &[f32], q: &[u8]) -> Track {
    let xs: Vec<f64> = x.iter().zip(q).map(|(v, &qq)| if qq == 2 { 0.0 } else { *v as f64 }).collect();
    let (f, ts, s) = dsp::spectrogram(&xs, FS, (60.0 * FS) as usize, (45.0 * FS) as usize, 8192);
    let band: Vec<usize> = (0..f.len()).filter(|&i| f[i] >= 0.08 && f[i] <= 0.6).collect();
    let bpm: Vec<f64> = band.iter().map(|&i| 60.0 * f[i]).collect();
    let nb = band.len();
    let p: Vec<Vec<f64>> = s
        .iter()
        .map(|col| {
            let c: Vec<f64> = band.iter().map(|&i| col[i]).collect();
            let tot = c.iter().sum::<f64>() + 1e-12;
            c.iter().map(|v| v / tot).collect()
        })
        .collect();
    let nt = p.len();
    let mut path = vec![0usize; nt];
    if nt > 0 {
        let l: Vec<Vec<f64>> = p.iter().map(|c| c.iter().map(|v| (v + 1e-6).ln()).collect()).collect();
        let mut acc = l[0].clone();
        let mut back = vec![vec![0usize; nb]; nt];
        for t in 1..nt {
            let mut nacc = vec![0.0; nb];
            for i in 0..nb {
                let (mut bj, mut bv) = (0, f64::NEG_INFINITY);
                for j in 0..nb {
                    let v = acc[j] - TRACK_LAMBDA * (bpm[i] - bpm[j]).abs();
                    if v > bv {
                        bv = v;
                        bj = j;
                    }
                }
                back[t][i] = bj;
                nacc[i] = bv + l[t][i];
            }
            acc = nacc;
        }
        path[nt - 1] = dsp::argmax(&acc);
        for t in (1..nt).rev() {
            path[t - 1] = back[t][path[t]];
        }
    }
    let reg: Vec<f64> = (0..nt).map(|t| (0..nb).filter(|&i| (bpm[i] - bpm[path[t]]).abs() <= 1.5).map(|i| p[t][i]).sum()).collect();
    let reg2 = if reg.len() >= 8 { dsp::convolve_box_same(&reg, 8, 1.0 / 8.0) } else { reg.clone() };
    Track { t: ts, bpm: path.iter().map(|&i| bpm[i]).collect(), reg, reg2, spec_bpm: bpm, spec: p }
}

/// next_ok[i]: breath i+1 directly follows breath i and both are usable.
pub fn consecutive(b: &Breaths) -> Vec<bool> {
    let n = b.len();
    let mut c = vec![false; n];
    for i in 0..n.saturating_sub(1) {
        c[i] = (b.rise[i + 1] - (b.rise[i] + b.ttot[i])).abs() < 0.3 && b.ok[i] && b.ok[i + 1];
    }
    c
}

#[derive(Clone, Debug)]
pub struct Epoch {
    pub start: String,
    pub t_h: f64,
    pub rate_bpm: f64,
    pub breath_bpm: f64,
    pub cv_pct: f64,
    pub rmssd_s: f64,
    pub ti_frac: f64,
    pub breaths_ok: usize,
    pub breaths: usize,
    pub reading_pct: f64,
    pub present_pct: f64,
    pub motion_pct: f64,
    pub movements: usize,
    pub regularity: f64,
    pub track_bpm: f64,
    pub good: bool,
}

fn epochs(n: &NightData, ep_s: f64) -> Vec<Epoch> {
    let b = &n.b;
    let cons = consecutive(b);
    let dur = n.q.len() as f64 / FS;
    let mut rows = vec![];
    let mut e0 = 0.0;
    while e0 < dur {
        let e1 = e0 + ep_s;
        let m: Vec<bool> = b.rise.iter().map(|&r| r >= e0 && r < e1).collect();
        let okm: Vec<usize> = (0..b.len()).filter(|&i| m[i] && b.ok[i]).collect();
        let tt: Vec<f64> = okm.iter().map(|&i| b.ttot[i]).collect();
        let d: Vec<f64> = (0..b.len().saturating_sub(1)).filter(|&i| cons[i] && m[i]).map(|i| b.ttot[i + 1] - b.ttot[i]).collect();
        let fm: Vec<usize> = (0..n.ft.len()).filter(|&i| n.ft[i] >= e0 && n.ft[i] < e1).collect();
        let f: Vec<f64> = fm.iter().map(|&i| n.fv[i]).filter(|v| v.is_finite()).collect();
        let (s0, s1) = ((e0 * FS) as usize, ((e1.min(dur)) * FS) as usize);
        let s1 = s1.min(n.q.len());
        let span = (s1 - s0).max(1) as f64;
        let tk: Vec<usize> = (0..n.track.t.len()).filter(|&i| n.track.t[i] >= e0 && n.track.t[i] < e1).collect();
        let tks: Vec<f64> = tk.iter().filter(|&&i| n.track.reg2[i] >= REG_MIN).map(|&i| n.track.bpm[i]).collect();
        let nan = f64::NAN;
        rows.push(Epoch {
            start: clock(n.t0 + e0, "%H:%M"),
            t_h: e0 / 3600.0,
            rate_bpm: if f.is_empty() { nan } else { dsp::median(&f) },
            breath_bpm: if tt.len() >= 5 { 60.0 / dsp::median(&tt) } else { nan },
            cv_pct: if tt.len() >= 10 { 100.0 * dsp::std(&tt) / dsp::mean(&tt) } else { nan },
            rmssd_s: if d.len() >= 10 { dsp::mean(&d.iter().map(|v| v * v).collect::<Vec<_>>()).sqrt() } else { nan },
            ti_frac: if tt.len() >= 5 { dsp::nanmedian(&okm.iter().map(|&i| b.ti_frac[i]).collect::<Vec<_>>()) } else { nan },
            breaths_ok: okm.len(),
            breaths: m.iter().filter(|&&v| v).count(),
            reading_pct: 100.0 * f.len() as f64 / fm.len().max(1) as f64,
            present_pct: 100.0 * n.present[s0..s1].iter().filter(|&&v| v).count() as f64 / span,
            motion_pct: 100.0 * n.q[s0..s1].iter().filter(|&&v| v == 2).count() as f64 / span,
            movements: n.events.iter().filter(|e| e0 <= e.0 as f64 && (e.0 as f64) < e1).count(),
            regularity: if tk.is_empty() { nan } else { dsp::mean(&tk.iter().map(|&i| n.track.reg[i]).collect::<Vec<_>>()) },
            track_bpm: if tks.len() >= 4 { dsp::median(&tks) } else { nan },
            good: okm.len() >= GOOD_MIN && okm.len() as f64 >= GOOD_FRAC * m.iter().filter(|&&v| v).count() as f64,
        });
        e0 += ep_s;
    }
    rows
}

// ----------------------------------------------------------------- reference (wearable)
#[derive(Clone, Default)]
pub struct Reference {
    /// per-second label over the recording ("" = not covered)
    pub lab: Vec<&'static str>,
    pub restless: Vec<f64>,
    pub resp: Vec<(String, f64, f64, f64)>,
    pub source: String,
}

fn clock_s(hm: &str, t0: f64, dur: f64) -> Option<f64> {
    let v: Vec<u32> = hm.trim().split(':').filter_map(|p| p.parse().ok()).collect();
    if v.len() < 2 {
        return None;
    }
    let b = Local.timestamp_opt(t0 as i64, 0).single()?;
    let day = b.date_naive().and_hms_opt(v[0], v[1], *v.get(2).unwrap_or(&0))?;
    let s0 = Local.from_local_datetime(&day).earliest()?.timestamp() as f64 - t0;
    [-86400.0, 0.0, 86400.0]
        .iter()
        .map(|d| s0 + d)
        .min_by(|a, b| (0f64.max(-a).max(a - dur)).partial_cmp(&(0f64.max(-b).max(b - dur))).unwrap())
}

fn stage_static(s: &str) -> &'static str {
    match s {
        "deep" => "deep",
        "light" => "light",
        "rem" => "rem",
        "wake" => "wake",
        _ => "",
    }
}

pub fn load_stages(path: &Path, t0: f64, dur: f64) -> Result<Vec<&'static str>> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty() && !l.starts_with('#'));
    let head: Vec<&str> = lines.next().unwrap_or("").split(',').map(|s| s.trim()).collect();
    let col = |k: &str| head.iter().position(|h| *h == k);
    let (ia, ib, is) = (col("start").unwrap_or(0), col("end").unwrap_or(1), col("stage").unwrap_or(2));
    let mut se = vec![];
    for l in lines {
        let p: Vec<&str> = l.split(',').collect();
        if let (Some(a), Some(b), Some(s)) = (p.get(ia), p.get(ib), p.get(is)) {
            if let (Some(a), Some(b)) = (clock_s(a, t0, dur), clock_s(b, t0, dur)) {
                se.push((a, b, stage_static(&s.trim().to_lowercase())));
            }
        }
    }
    let n = dur as usize;
    let mut lab = vec![""; n];
    if se.is_empty() {
        return Ok(lab);
    }
    if !se.iter().any(|x| x.2 == "light") {
        let lo = se.iter().map(|x| x.0).fold(f64::INFINITY, f64::min);
        let hi = se.iter().map(|x| x.1).fold(f64::NEG_INFINITY, f64::max);
        for (t, l) in lab.iter_mut().enumerate() {
            if t as f64 >= lo && (t as f64) < hi {
                *l = "light";
            }
        }
    }
    for (a, b, s) in se {
        for (t, l) in lab.iter_mut().enumerate() {
            if t as f64 >= a && (t as f64) < b {
                *l = s;
            }
        }
    }
    Ok(lab)
}

/// Google Health reference for the recording, cached in the night folder (same files as the
/// Python tool: health-sleep.json, health-stages.csv, health-restless.csv, health-respiration.json).
pub fn health_reference(dir: &Path, t0: f64, dur: f64, refresh: bool, log: &(dyn Fn(String) + Sync)) -> Option<Reference> {
    let f_st = dir.join("health-stages.csv");
    let f_rs = dir.join("health-restless.csv");
    let f_rr = dir.join("health-respiration.json");
    if !f_st.exists() || refresh {
        let fetched = (|| -> Result<Option<String>> {
            let Some(dp) = health::night_reference(t0, t0 + dur)? else { return Ok(None) };
            let st = health::stages(&dp);
            if st.is_empty() {
                return Ok(None);
            }
            let marks = health::restless(&dp);
            let rr = match health::session_span(&dp).map(|s| health::respiration(s.1)) {
                Some(Ok(r)) => r,
                Some(Err(e)) => {
                    log(format!("reference: no breathing-rate summary ({e})"));
                    vec![]
                }
                None => vec![],
            };
            let f = |t: f64| clock(t, "%H:%M:%S");
            std::fs::write(dir.join("health-sleep.json"), serde_json::to_vec_pretty(&dp)?)?;
            let mut s = String::from("# Google Health API sleep stages for this recording (csi-breathing)\nstart,end,stage\n");
            for (a, b, typ) in &st {
                let _ = writeln!(s, "{},{},{}", f(*a), f(*b), typ);
            }
            std::fs::write(&f_st, s)?;
            let mut s = String::from("# Google Health API short awakenings (the app's restless marks), start times\n");
            for m in &marks {
                let _ = writeln!(s, "{}", f(*m));
            }
            std::fs::write(&f_rs, s)?;
            let mut o = serde_json::Map::new();
            for (k, b, sd, snr) in &rr {
                o.insert(k.clone(), serde_json::json!([b, sd, snr]));
            }
            std::fs::write(&f_rr, serde_json::to_vec_pretty(&o)?)?;
            Ok(Some(format!(
                "reference: {} stages {}-{}, {} restless marks, breathing rate {} (Google Health API)",
                st.len(),
                &f(st[0].0)[..5],
                &f(st[st.len() - 1].1)[..5],
                marks.len(),
                if rr.is_empty() { "none".into() } else { rr.iter().map(|r| format!("{} {}", r.0, an::pyf(r.1).trim_end_matches(".0"))).collect::<Vec<_>>().join(", ") }
            )))
        })();
        match fetched {
            Ok(Some(m)) => log(m),
            Ok(None) => {
                log("reference: no Google Health sleep session overlapping the recording (has the watch synced?)".into());
                return None;
            }
            Err(e) => {
                log(format!("reference: not fetched ({e})"));
                return None;
            }
        }
    } else {
        log(format!("reference: cached Google Health data in {} (refresh to fetch again)", dir.display()));
    }
    let lab = load_stages(&f_st, t0, dur).ok()?;
    let restless = std::fs::read_to_string(&f_rs)
        .map(|s| {
            s.lines()
                .filter(|l| l.starts_with(|c: char| c.is_ascii_digit()))
                .filter_map(|l| clock_s(l, t0, dur))
                .filter(|&v| v >= 0.0 && v < dur)
                .collect()
        })
        .unwrap_or_default();
    let resp = std::fs::read(&f_rr)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| v.as_object().cloned())
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| {
                    let a = v.as_array()?;
                    Some((k.clone(), a.first()?.as_f64()?, a.get(1)?.as_f64()?, a.get(2).and_then(|x| x.as_f64()).unwrap_or(0.0)))
                })
                .collect()
        })
        .unwrap_or_default();
    Some(Reference { lab, restless, resp, source: "Google Health".into() })
}

#[derive(Clone, Debug)]
pub struct StageRow {
    pub stage: &'static str,
    pub minutes: f64,
    pub regularity: f64,
    pub steady_pct: f64,
    pub breath_bpm: f64,
    pub wearable: Option<(f64, f64)>,
    pub usable_pct: f64,
    pub rmssd: f64,
    pub moves_h: f64,
}

#[derive(Clone, Default)]
pub struct Comparison {
    pub rows: Vec<StageRow>,
    pub whole: Option<(f64, f64)>,
    pub deep_minus_rem: Option<(f64, String)>,
    pub restless_hits: Vec<(u32, f64, usize, f64)>,
}

fn compare(n: &NightData, r: &Reference) -> Comparison {
    use rand::{Rng, SeedableRng};
    let lab = &r.lab;
    let nl = lab.len();
    if nl == 0 {
        return Comparison::default();
    }
    let tr = &n.track;
    let ti: Vec<usize> = tr.t.iter().map(|&t| (t as usize).min(nl - 1)).collect();
    let b = &n.b;
    let cons = consecutive(b);
    let sb: Vec<&str> = b.rise.iter().map(|&t| lab[(t as usize).min(nl - 1)]).collect();
    let se: Vec<&str> = n.events.iter().map(|e| lab[e.0.min(nl - 1)]).collect();
    let mut out = Comparison::default();
    for st in STAGES {
        let k: Vec<usize> = (0..ti.len()).filter(|&i| lab[ti[i]] == st).collect();
        let mins = lab.iter().filter(|&&l| l == st).count() as f64 / 60.0;
        if mins < 5.0 {
            continue;
        }
        let kb: Vec<usize> = (0..b.len()).filter(|&i| sb[i] == st).collect();
        let kbo: Vec<f64> = kb.iter().filter(|&&i| b.ok[i]).map(|&i| 60.0 / b.ttot[i]).collect();
        let d: Vec<f64> = (0..b.len().saturating_sub(1)).filter(|&i| cons[i] && sb[i] == st).map(|i| b.ttot[i + 1] - b.ttot[i]).collect();
        out.rows.push(StageRow {
            stage: st,
            minutes: mins,
            regularity: dsp::mean(&k.iter().map(|&i| tr.reg[i]).collect::<Vec<_>>()),
            steady_pct: 100.0 * k.iter().filter(|&&i| tr.reg2[i] >= REG_MIN).count() as f64 / k.len().max(1) as f64,
            breath_bpm: if kbo.is_empty() { f64::NAN } else { dsp::median(&kbo) },
            wearable: r.resp.iter().find(|x| x.0 == st).map(|x| (x.1, x.2)),
            usable_pct: 100.0 * kbo.len() as f64 / kb.len().max(1) as f64,
            rmssd: if d.is_empty() { f64::NAN } else { dsp::mean(&d.iter().map(|v| v * v).collect::<Vec<_>>()).sqrt() },
            moves_h: se.iter().filter(|&&s| s == st).count() as f64 / (mins / 60.0),
        });
    }
    if let Some(full) = r.resp.iter().find(|x| x.0 == "full") {
        let v: Vec<f64> = (0..b.len()).filter(|&i| b.ok[i] && sb[i] != "").map(|i| 60.0 / b.ttot[i]).collect();
        out.whole = Some((full.1, dsp::median(&v)));
    }
    let diff = |sh: usize| -> f64 {
        let (mut d, mut dn, mut r_, mut rn) = (0.0, 0, 0.0, 0);
        for (i, &t) in ti.iter().enumerate() {
            let l = lab[(t + nl - sh % nl) % nl]; // np.roll(lab, sh)[t] = lab[t - sh]
            if l == "deep" {
                d += tr.reg[i];
                dn += 1;
            } else if l == "rem" {
                r_ += tr.reg[i];
                rn += 1;
            }
        }
        if dn > 0 && rn > 0 { d / dn as f64 - r_ / rn as f64 } else { f64::NAN }
    };
    let obs = diff(0);
    if obs.is_finite() && nl > 7200 {
        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let null: Vec<f64> = (0..2000).map(|_| diff(rng.random_range(1800.0..(nl as f64 - 1800.0)) as usize)).filter(|v| v.is_finite()).collect();
        let pv = null.iter().filter(|&&v| v >= obs).count() as f64 / null.len() as f64;
        let ps = if pv > 0.0 { format!("{pv:.3}") } else { format!("< {:.4}", 1.0 / null.len() as f64) };
        out.deep_minus_rem = Some((obs, ps));
    }
    if !r.restless.is_empty() && !n.events.is_empty() {
        let e: Vec<f64> = n.events.iter().map(|x| x.0 as f64).collect();
        let mut rng = rand::rngs::StdRng::seed_from_u64(0);
        for tol in [120.0, 180.0, 300.0] {
            let near = |m: f64| e.iter().any(|v| (v - m).abs() <= tol);
            let hit = r.restless.iter().filter(|&&m| near(m)).count() as f64 / r.restless.len() as f64;
            let ch = (0..500)
                .map(|_| (0..r.restless.len()).filter(|_| near(rng.random_range(0.0..nl as f64))).count() as f64 / r.restless.len() as f64)
                .sum::<f64>()
                / 500.0;
            out.restless_hits.push(((tol / 60.0) as u32, 100.0 * hit, r.restless.len(), 100.0 * ch));
        }
    }
    out
}

// ----------------------------------------------------------------- the whole night
pub struct NightData {
    pub dir: PathBuf,
    pub t0: f64,
    pub m: usize,
    pub b: Breaths,
    pub ft: Vec<f64>,
    pub fv: Vec<f64>,
    pub q: Vec<u8>,
    pub present: Vec<bool>,
    pub x: Vec<f32>,
    pub events: Vec<(usize, usize, f64)>,
    pub motion_frac: f64,
    pub track: Track,
    pub epochs: Vec<Epoch>,
    pub epoch_min: f64,
    pub reference: Option<Reference>,
    pub comparison: Option<Comparison>,
    pub summary: String,
}

impl NightData {
    pub fn dur(&self) -> f64 {
        self.q.len() as f64 / FS
    }
}

pub struct Options {
    pub epoch_min: f64,
    pub health: bool,
    pub refresh_health: bool,
    pub stages_csv: Option<PathBuf>,
    pub restless_csv: Option<PathBuf>,
}
impl Default for Options {
    fn default() -> Self {
        Options { epoch_min: 5.0, health: true, refresh_health: false, stages_csv: None, restless_csv: None }
    }
}

pub fn analyse(dir: &Path, opt: &Options, log: &(dyn Fn(String) + Sync)) -> Result<NightData> {
    let threads = (std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2) / 2).max(1);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build()?;
    pool.install(|| analyse_inner(dir, opt, log))
}

fn analyse_inner(dir: &Path, opt: &Options, log: &(dyn Fn(String) + Sync)) -> Result<NightData> {
    let tl = timeline(dir, log)?;
    log(format!("timeline: {}, {:.2} h, {} subcarriers", clock(tl.t0, "%H:%M:%S"), tl.n as f64 / FS / 3600.0, tl.m));
    let (mot, events) = movement(&tl);
    let motion_frac = mot.iter().filter(|&&v| v).count() as f64 / mot.len() as f64;
    log(format!("movement: {} events, {:.1}% of the night", events.len(), 100.0 * motion_frac));
    let starts: Vec<f64> = (0..).map(|i| i as f64 * CHUNK_S).take_while(|&c| c < tl.n as f64 / FS).collect();
    log(format!("analysing {} chunks of {} min ...", starts.len(), CHUNK_S as usize / 60));
    let t_ = std::time::Instant::now();
    let chunks: Vec<Chunk> = starts.par_iter().map(|&c0| analyse_chunk(&tl, &mot, c0)).collect();
    log(format!("  analysed in {:.0} s", t_.elapsed().as_secs_f64()));
    let mut b = Breaths::default();
    let (mut ft, mut fv, mut q, mut present, mut x) = (vec![], vec![], vec![], vec![], vec![]);
    for c in chunks {
        macro_rules! ext { ($($f:ident),*) => { $( b.$f.extend(c.b.$f.iter().cloned()); )* } }
        ext!(tr, rise, pk, nxt, slope, ttot, ti, te, ti_frac, depth, ok, why);
        ft.extend(c.ft);
        fv.extend(c.fv);
        q.extend(c.q);
        present.extend(c.present);
        x.extend(c.x);
    }
    let t0 = tl.t0;
    let m = tl.m;
    drop(tl);
    let track = track(&x, &q);
    let mut nd = NightData {
        dir: dir.to_path_buf(),
        t0,
        m,
        b,
        ft,
        fv,
        q,
        present,
        x,
        events,
        motion_frac,
        track,
        epochs: vec![],
        epoch_min: opt.epoch_min,
        reference: None,
        comparison: None,
        summary: String::new(),
    };
    nd.epochs = epochs(&nd, opt.epoch_min * 60.0);
    let dur = nd.dur();
    nd.reference = if let Some(p) = &opt.stages_csv {
        let lab = load_stages(p, t0, dur)?;
        let restless = opt
            .restless_csv
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|s| s.lines().filter(|l| l.starts_with(|c: char| c.is_ascii_digit())).filter_map(|l| clock_s(l, t0, dur)).filter(|&v| v >= 0.0 && v < dur).collect())
            .unwrap_or_default();
        Some(Reference { lab, restless, resp: vec![], source: p.display().to_string() })
    } else if opt.health {
        health_reference(dir, t0, dur, opt.refresh_health, log)
    } else {
        None
    };
    nd.comparison = nd.reference.as_ref().map(|r| compare(&nd, r));
    write_outputs(&nd)?;
    nd.summary = summary(&nd);
    for l in nd.summary.lines() {
        log(l.to_string());
    }
    std::fs::write(dir.join("night-summary.txt"), &nd.summary)?;
    Ok(nd)
}

fn f3(v: f64) -> String {
    an::nf(v, 0, 3)
}

fn write_outputs(n: &NightData) -> Result<()> {
    let b = &n.b;
    let mut s = String::from("clock,onset_s,bpm,ttot_s,ti_s,te_s,ti_frac,depth,ok,why\r\n");
    for i in 0..b.len() {
        let _ = write!(
            s,
            "{},{:.2},{:.1},{:.2},{:.2},{:.2},{},{:.3},{},{}\r\n",
            clock(n.t0 + b.rise[i], "%H:%M:%S"),
            b.rise[i],
            60.0 / b.ttot[i],
            b.ttot[i],
            b.ti[i],
            b.te[i],
            an::nf(b.ti_frac[i], 0, 2),
            b.depth[i],
            b.ok[i] as u8,
            b.why[i]
        );
    }
    std::fs::write(n.dir.join("night-breaths.csv"), s)?;
    let mut s = String::from("start,t_h,rate_bpm,breath_bpm,cv_pct,rmssd_s,ti_frac,breaths_ok,breaths,reading_pct,present_pct,motion_pct,movements,regularity,track_bpm,good\r\n");
    for e in &n.epochs {
        let _ = write!(
            s,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\r\n",
            e.start,
            f3(e.t_h),
            f3(e.rate_bpm),
            f3(e.breath_bpm),
            f3(e.cv_pct),
            f3(e.rmssd_s),
            f3(e.ti_frac),
            e.breaths_ok,
            e.breaths,
            f3(e.reading_pct),
            f3(e.present_pct),
            f3(e.motion_pct),
            e.movements,
            f3(e.regularity),
            f3(e.track_bpm),
            e.good as u8
        );
    }
    std::fs::write(n.dir.join("night-epochs.csv"), s)?;
    let tr = &n.track;
    let mut s = String::from("clock,t_s,track_bpm,regularity,regularity_2min,steady\r\n");
    for i in 0..tr.t.len() {
        let _ = write!(
            s,
            "{},{:.1},{:.2},{:.3},{:.3},{}\r\n",
            clock(n.t0 + tr.t[i], "%H:%M:%S"),
            tr.t[i],
            tr.bpm[i],
            tr.reg[i],
            tr.reg2[i],
            (tr.reg2[i] >= REG_MIN) as u8
        );
    }
    std::fs::write(n.dir.join("night-track.csv"), s)?;
    Ok(())
}

pub fn summary(n: &NightData) -> String {
    let mut o = String::new();
    let b = &n.b;
    let okn = b.ok.iter().filter(|&&v| v).count();
    let _ = writeln!(o, "\nbreaths: {} detected, {} usable ({:.0}%)", b.len(), okn, 100.0 * okn as f64 / b.len().max(1) as f64);
    let mut why: std::collections::BTreeMap<&str, usize> = Default::default();
    for i in 0..b.len() {
        if !b.ok[i] {
            *why.entry(b.why[i]).or_default() += 1;
        }
    }
    let mut wv: Vec<(&str, usize)> = why.into_iter().collect();
    wv.sort_by(|a, b| b.1.cmp(&a.1));
    let _ = writeln!(o, "  rejected: {}", wv.iter().map(|(w, c)| format!("{w} {c}")).collect::<Vec<_>>().join(", "));
    let fin: Vec<f64> = n.fv.iter().copied().filter(|v| v.is_finite()).collect();
    if !fin.is_empty() {
        let _ = writeln!(
            o,
            "rate reading {:.0}% of the time; median {:.1} BPM (5-95%: {:.1}-{:.1})",
            100.0 * fin.len() as f64 / n.fv.len() as f64,
            dsp::median(&fin),
            dsp::percentile(&fin, 5.0),
            dsp::percentile(&fin, 95.0)
        );
    }
    let nq = n.q.len() as f64;
    let _ = writeln!(
        o,
        "breathing present {:.0}%, weak {:.0}%, motion {:.1}%",
        100.0 * n.present.iter().filter(|&&v| v).count() as f64 / nq,
        100.0 * n.q.iter().filter(|&&v| v == 1).count() as f64 / nq,
        100.0 * n.q.iter().filter(|&&v| v == 2).count() as f64 / nq
    );
    let cons = consecutive(b);
    let d: Vec<f64> = (0..b.len().saturating_sub(1)).filter(|&i| cons[i]).map(|i| b.ttot[i + 1] - b.ttot[i]).collect();
    let tto: Vec<f64> = (0..b.len()).filter(|&i| b.ok[i]).map(|i| b.ttot[i]).collect();
    let cv: Vec<f64> = n.epochs.iter().map(|e| e.cv_pct).filter(|v| !v.is_nan()).collect();
    if !tto.is_empty() && !cv.is_empty() {
        let _ = writeln!(
            o,
            "variability: whole-night CV {:.0}%, RMSSD {:.2} s; per-{}-min CV median {:.0}% (range {:.0}-{:.0}%)",
            100.0 * dsp::std(&tto) / dsp::mean(&tto),
            dsp::mean(&d.iter().map(|v| v * v).collect::<Vec<_>>()).sqrt(),
            an::pyf(n.epoch_min).trim_end_matches(".0"),
            dsp::median(&cv),
            cv.iter().cloned().fold(f64::INFINITY, f64::min),
            cv.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
        );
    }
    let g: Vec<&Epoch> = n.epochs.iter().filter(|e| e.good).collect();
    if !g.is_empty() {
        let cvs: Vec<f64> = g.iter().map(|e| e.cv_pct).collect();
        let _ = writeln!(
            o,
            "reliable epochs: {}/{} ({:.1} h): rate median {:.1} BPM, CV median {:.0}% (range {:.0}-{:.0}%), at {}",
            g.len(),
            n.epochs.len(),
            g.len() as f64 * n.epoch_min / 60.0,
            dsp::median(&g.iter().map(|e| e.breath_bpm).collect::<Vec<_>>()),
            dsp::median(&cvs),
            cvs.iter().cloned().fold(f64::INFINITY, f64::min),
            cvs.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            g.iter().map(|e| e.start.as_str()).collect::<Vec<_>>().join(" ")
        );
    }
    let tr = &n.track;
    let st: Vec<bool> = tr.reg2.iter().map(|&v| v >= REG_MIN).collect();
    let stb: Vec<f64> = (0..st.len()).filter(|&i| st[i]).map(|i| tr.bpm[i]).collect();
    let sf = st.iter().filter(|&&v| v).count() as f64 / st.len().max(1) as f64;
    if !stb.is_empty() {
        let _ = writeln!(
            o,
            "spectral track: steady rhythm {:.0}% of the night ({:.1} h), rate there median {:.1} BPM (5-95%: {:.1}-{:.1}); regularity median {:.2}",
            100.0 * sf,
            sf * n.dur() / 3600.0,
            dsp::median(&stb),
            dsp::percentile(&stb, 5.0),
            dsp::percentile(&stb, 95.0),
            dsp::median(&tr.reg)
        );
    }
    for h in 0..(n.dur() / 3600.0).ceil() as usize {
        let (a, e) = (h as f64 * 3600.0, (h + 1) as f64 * 3600.0);
        let k: Vec<usize> = (0..tr.t.len()).filter(|&i| tr.t[i] >= a && tr.t[i] < e).collect();
        let ks: Vec<f64> = k.iter().filter(|&&i| st[i]).map(|&i| tr.bpm[i]).collect();
        let _ = writeln!(
            o,
            "  {}  regularity {:.2}  steady {:3.0}%  {}  movements {}",
            clock(n.t0 + a, "%H:%M"),
            dsp::mean(&k.iter().map(|&i| tr.reg[i]).collect::<Vec<_>>()),
            100.0 * ks.len() as f64 / k.len().max(1) as f64,
            if ks.len() >= 4 { format!("rate {:.1}", dsp::median(&ks)) } else { "rate  -  ".into() },
            n.events.iter().filter(|ev| a <= ev.0 as f64 && (ev.0 as f64) < e).count()
        );
    }
    if let Some(c) = &n.comparison {
        let _ = writeln!(
            o,
            "\nWiFi metrics per reference stage:\n  {:6} {:>5} {:>10} {:>8} {:>10} {:>13} {:>8} {:>8} {:>8}",
            "stage", "min", "regularity", "steady%", "breath BPM", "wearable BPM", "usable%", "RMSSD s", "moves/h"
        );
        for r in &c.rows {
            let _ = writeln!(
                o,
                "  {:6} {:5.0} {:10.2} {:8.0} {} {:>13} {:8.0} {} {:8.1}",
                r.stage,
                r.minutes,
                r.regularity,
                r.steady_pct,
                an::nf(r.breath_bpm, 10, 1),
                r.wearable.map(|w| format!("{:.1} +-{:.1}", w.0, w.1)).unwrap_or_else(|| "-".into()),
                r.usable_pct,
                an::nf(r.rmssd, 8, 2),
                r.moves_h
            );
        }
        if let Some((wb, wf)) = c.whole {
            let _ = writeln!(o, "  whole night: wearable {wb:.1} BPM, WiFi median of usable breaths {wf:.1} BPM");
        }
        if let Some((d, p)) = &c.deep_minus_rem {
            let _ = writeln!(o, "  regularity deep - REM = {d:+.3}, circular-shift p = {p}");
        }
        for (tol, hit, nm, ch) in &c.restless_hits {
            let _ = writeln!(o, "  restless marks with a WiFi movement within +-{tol} min: {hit:.0}% of {nm} (chance {ch:.0}%)");
        }
    }
    o
}

/// Nights found in a results folder (newest first).
pub fn list_nights(results: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(results)
        .map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.is_dir() && p.file_name().unwrap().to_string_lossy().starts_with("night-")).collect())
        .unwrap_or_default();
    v.sort();
    v.reverse();
    v
}
