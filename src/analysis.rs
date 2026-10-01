//! Port of breath_analyze.py: CSI log -> breathing waveform -> single breaths -> rates.
//! Same algorithms and constants; comments on *why* live in the Python original and REPORT.md.

use crate::dsp::{self, Psd};
use nalgebra::{DMatrix, SymmetricEigen};
use rustfft::num_complex::Complex64 as C64;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read};

pub const FS: f64 = 20.0;
pub const BAND: (f64, f64) = (0.1, 0.6);
pub const WIN_S: f64 = 30.0;
pub const STEP_S: f64 = 5.0;
pub const CONF_MIN: f64 = 6.0;
pub const PRESENCE_CONF: f64 = 12.0;  // was 8 before the amplitude-first resample (see breath_analyze.py)
pub const PRESENCE_MIN_S: f64 = 15.0;
pub const PRESENCE_BRIDGE_S: f64 = 15.0;
pub const HEART_BAND: (f64, f64) = (0.8, 2.0);
pub const HR_CONF_MIN: f64 = 12.0;
pub const MOTION_BIG: f64 = 6.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    Pca,
    Snr,
    Purity,
    Cluster,
}
pub const METHODS: [Method; 4] = [Method::Pca, Method::Snr, Method::Purity, Method::Cluster];
impl Method {
    pub fn name(self) -> &'static str {
        match self {
            Method::Pca => "pca",
            Method::Snr => "snr",
            Method::Purity => "purity",
            Method::Cluster => "cluster",
        }
    }
}

// ----------------------------------------------------------------- matrices
/// Column-major real matrix (n rows = samples, m cols = features).
#[derive(Clone)]
pub struct Mat {
    pub n: usize,
    pub m: usize,
    pub d: Vec<f64>,
}
impl Mat {
    pub fn zeros(n: usize, m: usize) -> Self {
        Mat { n, m, d: vec![0.0; n * m] }
    }
    pub fn col(&self, j: usize) -> &[f64] {
        &self.d[j * self.n..(j + 1) * self.n]
    }
    pub fn col_mut(&mut self, j: usize) -> &mut [f64] {
        &mut self.d[j * self.n..(j + 1) * self.n]
    }
    pub fn select_cols(&self, cols: &[usize]) -> Mat {
        let mut d = Vec::with_capacity(self.n * cols.len());
        for &j in cols {
            d.extend_from_slice(self.col(j));
        }
        Mat { n: self.n, m: cols.len(), d }
    }
}

/// Column-major complex matrix (n packets/samples x m subcarriers).
#[derive(Clone)]
pub struct CMat {
    pub n: usize,
    pub m: usize,
    pub d: Vec<C64>,
}
impl CMat {
    pub fn col(&self, j: usize) -> &[C64] {
        &self.d[j * self.n..(j + 1) * self.n]
    }
    pub fn rows(&self, a: usize, b: usize) -> CMat {
        let n = b - a;
        let mut d = Vec::with_capacity(n * self.m);
        for j in 0..self.m {
            d.extend_from_slice(&self.col(j)[a..b]);
        }
        CMat { n, m: self.m, d }
    }
}

// ----------------------------------------------------------------- parse / resample
pub struct Recording {
    pub t: Vec<f64>,
    pub seq: Vec<i64>,
    pub rssi: Vec<i32>,
    pub nsc: usize,
    /// packets x subcarriers, row-major (re, im) as f32 (the raw values are small integers)
    pub h: Vec<[f32; 2]>,
    pub bad: usize,
}

fn parse_line(line: &[u8]) -> Option<(i64, i32, i64, Vec<[f32; 2]>)> {
    let mut parts = line.split(|&b| b == b'"');
    let head = parts.next()?;
    let data = parts.next()?;
    let tail = parts.next()?;
    if parts.next().is_some() || !tail.iter().all(|b| b.is_ascii_whitespace()) {
        return None;
    }
    let head = std::str::from_utf8(head).ok()?;
    let p: Vec<&str> = head.trim_end_matches(',').split(',').collect();
    if p.len() != 14 {
        return None;
    }
    let data = std::str::from_utf8(data).ok()?;
    let body = data.trim_matches(|c| c == '[' || c == ']');
    let mut v = Vec::with_capacity(256);
    for s in body.split(',') {
        let x: i16 = s.trim().parse().ok()?;
        v.push(x);
    }
    let len: usize = p[12].trim().parse().ok()?;
    if v.len() != len || v.len() % 2 != 0 {
        return None;
    }
    let s: i64 = p[1].trim().parse().ok()?;
    let r: i32 = p[3].trim().parse().ok()?;
    let t: i64 = p[9].trim().parse().ok()?;
    // (imag, real) per ESP convention
    let row = v.chunks(2).map(|c| [c[1] as f32, c[0] as f32]).collect();
    Some((s, r, t, row))
}

/// breath_analyze.parse: t (s, unwrapped chip clock from 0), seq, rssi, H. Reads .gz too.
pub fn parse(path: &str) -> anyhow::Result<Recording> {
    let f = std::fs::File::open(path)?;
    let rd: Box<dyn Read> = if path.ends_with(".gz") { Box::new(flate2::read::MultiGzDecoder::new(f)) } else { Box::new(f) };
    parse_reader(BufReader::with_capacity(1 << 20, rd))
}

pub fn parse_bytes(b: &[u8]) -> anyhow::Result<Recording> {
    parse_reader(BufReader::new(b))
}

fn parse_reader<R: BufRead>(mut rd: R) -> anyhow::Result<Recording> {
    let mut ts = Vec::new();
    let mut seq = Vec::new();
    let mut rssi = Vec::new();
    let mut rows: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut bad = 0;
    let mut line = Vec::new();
    loop {
        line.clear();
        if rd.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if !line.starts_with(b"CSI_DATA") {
            continue;
        }
        let l = if line.ends_with(b"\n") { &line[..line.len() - 1] } else { &line[..] };
        let l = if l.ends_with(b"\r") { &l[..l.len() - 1] } else { l };
        match parse_line(l) {
            Some((s, r, t, row)) => {
                seq.push(s);
                rssi.push(r);
                ts.push(t);
                rows.push(row);
            }
            None => bad += 1,
        }
    }
    if rows.is_empty() {
        anyhow::bail!("no CSI_DATA lines parsed");
    }
    // modal row length
    let mut counts = std::collections::HashMap::new();
    for r in &rows {
        *counts.entry(r.len()).or_insert(0usize) += 1;
    }
    let nsc = *counts.iter().max_by_key(|(_, c)| **c).unwrap().0;
    let mut t2 = Vec::new();
    let mut s2 = Vec::new();
    let mut r2 = Vec::new();
    let mut h = Vec::with_capacity(rows.len() * nsc);
    for i in 0..rows.len() {
        if rows[i].len() == nsc {
            t2.push(ts[i]);
            s2.push(seq[i]);
            r2.push(rssi[i]);
            h.extend_from_slice(&rows[i]);
        } else {
            bad += 1;
        }
    }
    let period = 4294.967296;
    let ph: Vec<f64> = t2.iter().map(|&v| v as f64 / 1e6 * (2.0 * std::f64::consts::PI / period)).collect();
    let un = dsp::unwrap(&ph);
    let t: Vec<f64> = un.iter().map(|&p| p * (period / (2.0 * std::f64::consts::PI))).collect();
    let t0 = t[0];
    let t: Vec<f64> = t.iter().map(|v| v - t0).collect();
    Ok(Recording { t, seq: s2, rssi: r2, nsc, h, bad })
}

impl Recording {
    pub fn npkt(&self) -> usize {
        self.t.len()
    }
    /// Keep packets [k..] and restart the clock at 0.
    pub fn drop_head(&mut self, k: usize) {
        let t0 = self.t[k];
        self.t = self.t[k..].iter().map(|v| v - t0).collect();
        self.seq = self.seq[k..].to_vec();
        self.rssi = self.rssi[k..].to_vec();
        self.h = self.h[k * self.nsc..].to_vec();
    }
}

/// numpy.arange(0, stop, 1/FS)
pub fn arange_grid(stop: f64) -> Vec<f64> {
    let step = 1.0 / FS;
    let n = (stop / step).ceil().max(0.0) as usize;
    (0..n).map(|i| i as f64 * step).collect()
}

/// breath_analyze.RESAMPLE_AVG: offsets (s) of the 50 ms box averaged around each grid point.
pub const RESAMPLE_AVG: [f64; 5] = [-0.02, -0.01, 0.0, 0.01, 0.02];

/// breath_analyze.resample: amplitude |H| on an even FS grid, each grid point the mean over a
/// 50 ms box (numpy.interp at the RESAMPLE_AVG offsets). Amplitude first: the C5's phase is
/// random per packet, so interpolating re/im between packets shrinks |H| by a random amount.
/// The result is real: |H| in `re`, `im` = 0 (consumers only use the norm).
pub fn resample(rec: &Recording) -> (Vec<f64>, CMat) {
    let tg = arange_grid(*rec.t.last().unwrap());
    let n = tg.len();
    let m = rec.nsc;
    let np_ = rec.npkt();
    let t = &rec.t;
    let mut d = vec![C64::new(0.0, 0.0); n * m];
    let mut mag = vec![0.0; np_];
    for k in 0..m {
        for (p, v) in mag.iter_mut().enumerate() {
            let z = rec.h[p * m + k];
            *v = (z[0] as f64).hypot(z[1] as f64);
        }
        for &off in &RESAMPLE_AVG {
            for (i, &v) in tg.iter().enumerate() {
                d[k * n + i].re += dsp::interp1(v + off, t, &mag);
            }
        }
        for c in &mut d[k * n..(k + 1) * n] {
            c.re /= RESAMPLE_AVG.len() as f64;
        }
    }
    (tg, CMat { n, m, d })
}

// ----------------------------------------------------------------- features
/// |H_k| of the usable subcarriers, robust-scaled and clipped (n x ~114). Like
/// breath_analyze.amplitude_features: the |H_k / H_k+4| ratios were dropped on 2026-09-30.
pub fn amplitude_features(h: &CMat) -> Mat {
    let n = h.n;
    let colmean: Vec<f64> = (0..h.m).map(|j| h.col(j).iter().map(|c| c.norm()).sum::<f64>() / n as f64).collect();
    let med = dsp::median(&colmean);
    let ok: Vec<usize> = (0..h.m).filter(|&j| colmean[j] > 0.1 * med).collect();
    let mut r = Mat::zeros(n, ok.len());
    for (jj, &c) in ok.iter().enumerate() {
        let col = r.col_mut(jj);
        for i in 0..n {
            col[i] = h.d[c * n + i].norm();
        }
    }
    for j in 0..r.m {
        let col = r.col_mut(j);
        let md = dsp::median(col);
        let dev: Vec<f64> = col.iter().map(|v| (v - md).abs()).collect();
        let mad = dsp::median(&dev) * 1.4826 + 1e-9;
        for v in col.iter_mut() {
            *v = ((*v - md) / mad).clamp(-6.0, 6.0);
        }
    }
    r
}

fn filt_cols(r: &Mat, order: usize, band: (f64, f64)) -> Mat {
    let sos = dsp::butter_bandpass(order, band.0, band.1, FS);
    let mut f = Mat::zeros(r.n, r.m);
    for j in 0..r.m {
        f.col_mut(j).copy_from_slice(&dsp::sosfiltfilt(&sos, r.col(j)));
    }
    f
}

/// Per-column Welch spectra (nperseg=min(n, 30 s)), returns (f, P[col][freq]).
fn welch_cols(r: &Mat, nfft: Option<usize>) -> (Vec<f64>, Vec<Vec<f64>>) {
    let nper = r.n.min((30.0 * FS) as usize);
    let mut p = Psd::new(dsp::hann(nper), nfft.unwrap_or(nper).max(nper), FS);
    let f = p.freqs(FS);
    let ps = (0..r.m).map(|j| p.welch(r.col(j))).collect();
    (f, ps)
}

fn band_ratio(f: &[f64], ps: &[Vec<f64>], num: (f64, f64), den: (f64, f64)) -> Vec<f64> {
    ps.iter()
        .map(|p| {
            let a: Vec<f64> = f.iter().zip(p).filter(|(fv, _)| **fv >= num.0 && **fv <= num.1).map(|(_, v)| *v).collect();
            let b: Vec<f64> = f.iter().zip(p).filter(|(fv, _)| **fv >= den.0 && **fv <= den.1).map(|(_, v)| *v).collect();
            dsp::mean(&a) / (dsp::mean(&b) + 1e-12)
        })
        .collect()
}

fn normalize_cols(f: &mut Mat) {
    for j in 0..f.m {
        let c = f.col_mut(j);
        let s = dsp::std(c) + 1e-9;
        for v in c.iter_mut() {
            *v /= s;
        }
    }
}

/// Top principal directions of F (columns = features): (F v0, share of variance, F [v0 v1 v2]).
pub fn pca(f: &Mat) -> (Vec<f64>, f64, Vec<Vec<f64>>) {
    let p = f.m;
    let mut c = DMatrix::<f64>::zeros(p, p);
    for a in 0..p {
        for b in a..p {
            let v: f64 = f.col(a).iter().zip(f.col(b)).map(|(x, y)| x * y).sum();
            c[(a, b)] = v;
            c[(b, a)] = v;
        }
    }
    let eig = SymmetricEigen::new(c);
    let mut order: Vec<usize> = (0..p).collect();
    order.sort_by(|&a, &b| eig.eigenvalues[b].partial_cmp(&eig.eigenvalues[a]).unwrap());
    let total: f64 = eig.eigenvalues.iter().map(|v| v.max(0.0)).sum();
    let proj = |k: usize| -> Vec<f64> {
        let v = eig.eigenvectors.column(order[k]);
        // fix the arbitrary sign: largest-magnitude loading positive
        let s = if v.iter().fold(0.0f64, |m, x| if x.abs() > m.abs() { *x } else { m }) < 0.0 { -1.0 } else { 1.0 };
        (0..f.n).map(|i| (0..p).map(|j| f.d[j * f.n + i] * v[j]).sum::<f64>() * s).collect()
    };
    let x3: Vec<Vec<f64>> = (0..p.min(3)).map(proj).collect();
    (x3[0].clone(), eig.eigenvalues[order[0]].max(0.0) / total.max(1e-300), x3)
}

fn signed_weighted_sum(f: &Mat, reference: &[f64], wgt: &[f64]) -> Vec<f64> {
    let sgn: Vec<f64> = (0..f.m)
        .map(|j| {
            let d: f64 = f.col(j).iter().zip(reference).map(|(a, b)| a * b).sum();
            if d > 0.0 { 1.0 } else if d < 0.0 { -1.0 } else { 0.0 }
        })
        .collect();
    let ws: f64 = wgt.iter().sum();
    (0..f.n).map(|i| (0..f.m).map(|j| f.d[j * f.n + i] * sgn[j] * wgt[j]).sum::<f64>() / ws).collect()
}

/// One breathing waveform from all features: (waveform, share, 3-D trajectory).
pub fn breathing_waveform(h: &CMat, method: Method) -> (Vec<f64>, f64, Vec<Vec<f64>>) {
    let r = amplitude_features(h);
    let f = filt_cols(&r, 3, BAND);
    let (fr, ps) = welch_cols(&r, None);
    let bnr = band_ratio(&fr, &ps, BAND, (0.8, 5.0));
    match method {
        Method::Snr => {
            let thr = dsp::percentile(&bnr, 75.0);
            let sel: Vec<usize> = (0..bnr.len()).filter(|&j| bnr[j] >= thr).collect();
            let mut fs = f.select_cols(&sel);
            normalize_cols(&mut fs);
            let (reference, share, x) = pca(&fs);
            let w: Vec<f64> = sel.iter().map(|&j| bnr[j]).collect();
            (signed_weighted_sum(&fs, &reference, &w), share, x)
        }
        Method::Pca => {
            let thr = dsp::percentile(&bnr, 75.0);
            let sel: Vec<usize> = (0..bnr.len()).filter(|&j| bnr[j] >= thr).collect();
            let mut fs = f.select_cols(&sel);
            normalize_cols(&mut fs);
            pca(&fs)
        }
        Method::Purity => {
            let n = (30.0 * FS) as usize;
            let len = f.n;
            let starts: Vec<usize> = (0..=len.saturating_sub(n)).step_by(n / 2).collect();
            let nper = n.min(len);
            let mut psd = Psd::new(dsp::hann(nper), 2048.max(nper), FS);
            let fq = psd.freqs(FS);
            let bm: Vec<usize> = (0..fq.len()).filter(|&i| fq[i] >= BAND.0 && fq[i] <= BAND.1).collect();
            let mut pur = vec![0.0; f.m];
            for &s0 in &starts {
                for j in 0..f.m {
                    let seg = &f.col(j)[s0..(s0 + n).min(len)];
                    let p = psd.welch(seg);
                    let pb: Vec<f64> = bm.iter().map(|&i| p[i]).collect();
                    let pk = fq[bm[dsp::argmax(&pb)]];
                    let near: f64 = bm.iter().filter(|&&i| (fq[i] - pk).abs() <= 0.02).map(|&i| p[i]).sum();
                    pur[j] += near / (pb.iter().sum::<f64>() + 1e-12);
                }
            }
            for v in pur.iter_mut() {
                *v /= starts.len() as f64;
            }
            let bmed = dsp::median(&bnr);
            let cand: Vec<usize> = (0..f.m).filter(|&j| bnr[j] >= bmed).collect();
            let pc: Vec<f64> = cand.iter().map(|&j| pur[j]).collect();
            let thr = dsp::percentile(&pc, 75.0);
            let sel: Vec<usize> = cand.into_iter().filter(|&j| pur[j] >= thr).collect();
            let mut fs = f.select_cols(&sel);
            normalize_cols(&mut fs); // _pca normalises too
            pca(&fs)
        }
        Method::Cluster => {
            let bmed = dsp::median(&bnr);
            let sel: Vec<usize> = (0..f.m).filter(|&j| bnr[j] >= bmed).collect();
            let mut fs = f.select_cols(&sel);
            normalize_cols(&mut fs);
            let (reference, share, x) = pca(&fs);
            let g: Vec<f64> = (0..fs.m)
                .map(|j| {
                    let d: f64 = fs.col(j).iter().zip(&reference).map(|(a, b)| a * b).sum();
                    if d > 0.0 { 1.0 } else if d < 0.0 { -1.0 } else { 0.0 }
                })
                .collect();
            // corrcoef between features
            let p = fs.m;
            let means: Vec<f64> = (0..p).map(|j| dsp::mean(fs.col(j))).collect();
            let sds: Vec<f64> = (0..p)
                .map(|j| fs.col(j).iter().map(|v| (v - means[j]).powi(2)).sum::<f64>().sqrt())
                .collect();
            let mut v = vec![0.0; p];
            for a in 0..p {
                let mut s = 0.0;
                for b in 0..p {
                    let c: f64 = fs.col(a).iter().zip(fs.col(b)).map(|(x, y)| (x - means[a]) * (y - means[b])).sum::<f64>()
                        / (sds[a] * sds[b]);
                    s += c * g[b];
                }
                v[a] = s * g[a] / p as f64;
            }
            let vm = dsp::median(&v);
            let keep: Vec<usize> = (0..p).filter(|&j| v[j] >= vm).collect();
            let w: Vec<f64> = (0..fs.n)
                .map(|i| keep.iter().map(|&j| fs.d[j * fs.n + i] * g[j]).sum::<f64>() / keep.len() as f64)
                .collect();
            (w, share, x)
        }
    }
}

// ----------------------------------------------------------------- rates
pub fn spectral_peak(f: &[f64], p: &[f64], hi: f64) -> f64 {
    let mut best = f64::NEG_INFINITY;
    let mut fp = f64::NAN;
    for i in 0..f.len() {
        if f[i] >= BAND.0 && f[i] <= hi && p[i] > best {
            best = p[i];
            fp = f[i];
        }
    }
    fp
}

fn peak_conf(f: &[f64], p: &[f64]) -> f64 {
    let pb: Vec<f64> = f.iter().zip(p).filter(|(fv, _)| **fv >= BAND.0 && **fv <= BAND.1).map(|(_, v)| *v).collect();
    pb.iter().cloned().fold(f64::NEG_INFINITY, f64::max) / (dsp::median(&pb) + 1e-12)
}

/// 30 s Welch windows every 5 s: rows (centre s, BPM, confidence).
pub fn windows(w: &[f64]) -> Vec<(f64, f64, f64)> {
    let n = (WIN_S * FS) as usize;
    let step = (STEP_S * FS) as usize;
    let mut out = Vec::new();
    if w.len() < n {
        return out;
    }
    let mut psd = Psd::new(dsp::hann(n), 4096, FS);
    let f = psd.freqs(FS);
    let mut s = 0;
    while s + n <= w.len() {
        let p = psd.welch(&w[s..s + n]);
        out.push((s as f64 / FS + WIN_S / 2.0, 60.0 * spectral_peak(&f, &p, BAND.1), peak_conf(&f, &p)));
        s += step;
    }
    out
}

/// Per-sample quality: 0 ok, 1 weak, 2 motion.
pub fn quality(x: &[f64], motion: Option<&[bool]>) -> Vec<u8> {
    let sq: Vec<f64> = x.iter().map(|v| v * v).collect();
    let n8 = (8.0 * FS) as usize;
    let n2 = (2.0 * FS) as usize;
    let env: Vec<f64> = dsp::convolve_box_same(&sq, n8, 1.0 / n8 as f64).iter().map(|v| v.max(0.0).sqrt()).collect();
    let short: Vec<f64> = dsp::convolve_box_same(&sq, n2, 1.0 / n2 as f64).iter().map(|v| v.max(0.0).sqrt()).collect();
    let med = dsp::median(&env) + 1e-12;
    let mut q: Vec<u8> = env.iter().map(|&e| if e < 0.35 * med { 1 } else { 0 }).collect();
    let mot: Vec<bool> = match motion {
        None => short.iter().map(|&s| s > 3.0 * med).collect(),
        Some(m) => short.iter().zip(m).map(|(&s, &mm)| mm || s > MOTION_BIG * med).collect(),
    };
    if mot.iter().any(|&b| b) {
        let k = (2.0 * FS) as usize;
        let mf: Vec<f64> = mot.iter().map(|&b| if b { 1.0 } else { 0.0 }).collect();
        let d = dsp::convolve_box_same(&mf, 2 * k + 1, 1.0);
        for i in 0..q.len() {
            if d[i] > 0.5 {
                q[i] = 2;
            }
        }
    }
    q
}

/// Welch rate on 20 s windows, interpolated to every sample.
pub fn rate_prior(x: &[f64]) -> Vec<f64> {
    let n = (20.0 * FS) as usize;
    let st = (2.5 * FS) as usize;
    let mut tc = Vec::new();
    let mut r = Vec::new();
    let nper = n.min(x.len());
    if nper < 2 {
        return vec![f64::NAN; x.len()];
    }
    let mut psd = Psd::new(dsp::hann(nper), 4096, FS);
    let f = psd.freqs(FS);
    let last = x.len().saturating_sub(n);
    let mut s0 = 0;
    while s0 <= last {
        let seg = &x[s0..(s0 + n).min(x.len())];
        let p = if seg.len() == nper { psd.welch(seg) } else { dsp::welch(seg, FS, n, Some(4096)).1 };
        tc.push((s0 as f64 + n as f64 / 2.0) / FS);
        r.push(60.0 * spectral_peak(&f, &p, 0.8));
        s0 += st;
    }
    let ts: Vec<f64> = (0..x.len()).map(|i| i as f64 / FS).collect();
    dsp::interp(&ts, &tc, &r)
}

fn dwell(xn: &[f64], idx: &[usize], swing: f64) -> f64 {
    let mut out = Vec::new();
    for &i in idx {
        let (mut l, mut r) = (i, i);
        while l > 0 && (xn[l - 1] - xn[i]).abs() < 0.15 * swing {
            l -= 1;
        }
        while r < xn.len() - 1 && (xn[r + 1] - xn[i]).abs() < 0.15 * swing {
            r += 1;
        }
        out.push((r - l) as f64 / FS);
    }
    if out.is_empty() { 0.0 } else { dsp::mean(&out) }
}

/// Pick the sign whose troughs look like inhale onsets.
pub fn orient(xn: &[f64], x: &[f64]) -> i32 {
    let neg: Vec<f64> = xn.iter().map(|v| -v).collect();
    let tr = dsp::find_peaks(&neg, Some(FS as usize), Some(0.5));
    let pk = dsp::find_peaks(xn, Some(FS as usize), Some(0.5));
    let d = dsp::gradient(x);
    let m3 = dsp::mean(&d.iter().map(|v| v * v * v).collect::<Vec<_>>());
    let m2 = dsp::mean(&d.iter().map(|v| v * v).collect::<Vec<_>>());
    let skew = m3 / (m2.powf(1.5) + 1e-12);
    let mut dw = 0.0;
    if tr.len() >= 3 && pk.len() >= 3 {
        let swing = dsp::median(&pk.iter().map(|&i| xn[i]).collect::<Vec<_>>())
            - dsp::median(&tr.iter().map(|&i| xn[i]).collect::<Vec<_>>());
        let dt = dwell(xn, &tr, swing);
        let dp = dwell(xn, &pk, swing);
        dw = (dt - dp) / (dt + dp + 1e-9);
    }
    if dw + 0.5 * skew >= 0.0 { 1 } else { -1 }
}

#[derive(Clone, Default)]
pub struct Breaths {
    pub x: Vec<f64>,
    pub q: Vec<u8>,
    pub sign: i32,
    pub prior: Vec<f64>,
    pub present: Vec<bool>,
    pub tr: Vec<f64>,
    pub rise: Vec<f64>,
    pub slope: Vec<f64>,
    pub pk: Vec<f64>,
    pub nxt: Vec<f64>,
    pub depth: Vec<f64>,
    pub ttot: Vec<f64>,
    pub ti: Vec<f64>,
    pub te: Vec<f64>,
    pub ti_frac: Vec<f64>,
    pub ok: Vec<bool>,
    pub why: Vec<&'static str>,
}

impl Breaths {
    pub fn len(&self) -> usize {
        self.tr.len()
    }
    pub fn is_empty(&self) -> bool {
        self.tr.is_empty()
    }
    pub fn marker(&self, name: &str) -> &Vec<f64> {
        match name {
            "tr" => &self.tr,
            "rise" => &self.rise,
            _ => &self.slope,
        }
    }
}

/// Welch rate on sliding windows with motion zeroed: (centres, BPM, confidence).
pub fn welch_series(x: &[f64], q: &[u8], win: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let n = (win * FS) as usize;
    let st = (2.5 * FS) as usize;
    let (mut tc, mut r, mut c) = (Vec::new(), Vec::new(), Vec::new());
    if x.len() < n {
        return (tc, r, c);
    }
    let mut psd = Psd::new(dsp::hann(n), 4096, FS);
    let f = psd.freqs(FS);
    let mut seg = vec![0.0; n];
    let mut s0 = 0;
    while s0 + n <= x.len() {
        let bad = q[s0..s0 + n].iter().filter(|&&v| v == 2).count();
        if bad as f64 / n as f64 <= 0.5 {
            for i in 0..n {
                seg[i] = if q[s0 + i] == 2 { 0.0 } else { x[s0 + i] };
            }
            let p = psd.welch(&seg);
            tc.push((s0 as f64 + n as f64 / 2.0) / FS);
            r.push(60.0 * spectral_peak(&f, &p, BAND.1));
            c.push(peak_conf(&f, &p));
        }
        s0 += st;
    }
    (tc, r, c)
}

fn searchsorted_left(a: &[f64], v: f64) -> usize {
    a.partition_point(|&x| x < v)
}

/// Single breaths from the waveform, no reference needed.
pub fn detect_breaths(w: &[f64], sign: Option<i32>, motion: Option<&[bool]>) -> Breaths {
    let n = w.len();
    let sos = dsp::butter_bandpass(2, BAND.0, 0.8, FS);
    let mut x = dsp::sosfiltfilt(&sos, w);
    let q = quality(&x, motion);
    // rate-adaptive upper cutoff (~2.2x the local breathing frequency)
    let f0: Vec<f64> = rate_prior(&x).iter().map(|v| v / 60.0).collect();
    let cuts = [0.3, 0.4, 0.5, 0.65, 0.8];
    let xs: Vec<Vec<f64>> = cuts.iter().map(|&c| dsp::sosfiltfilt(&dsp::butter_bandpass(2, BAND.0, c, FS), w)).collect();
    let pick: Vec<f64> = f0
        .iter()
        .map(|&f| {
            let v = (2.2 * f).clamp(cuts[0], cuts[4]);
            if v.is_nan() { 5.0 } else { searchsorted_left(&cuts, v) as f64 }
        })
        .collect();
    let k4 = (4.0 * FS) as usize;
    let pick = dsp::convolve_box_same(&pick, k4, 1.0 / k4 as f64);
    for i in 0..n {
        let p = (pick[i].round_ties_even() as i64).clamp(0, 4) as usize;
        x[i] = xs[p][i];
    }
    let sq: Vec<f64> = x.iter().map(|v| v * v).collect();
    let n8 = (8.0 * FS) as usize;
    let env: Vec<f64> = dsp::convolve_box_same(&sq, n8, 1.0 / n8 as f64).iter().map(|v| v.max(0.0).sqrt() + 1e-9).collect();
    let mut xn: Vec<f64> = x.iter().zip(&env).map(|(a, b)| a / b).collect();
    let sign = sign.unwrap_or_else(|| orient(&xn, &x));
    let sg = sign as f64;
    for i in 0..n {
        x[i] *= sg;
        xn[i] *= sg;
    }
    let prior = rate_prior(&x);

    let neg: Vec<f64> = xn.iter().map(|v| -v).collect();
    let tr = dsp::find_peaks(&neg, Some(FS as usize), Some(0.3));
    let pk = dsp::find_peaks(&xn, Some(FS as usize), Some(0.3));
    let mut all: Vec<(usize, i32)> = tr.iter().map(|&i| (i, -1)).chain(pk.iter().map(|&i| (i, 1))).collect();
    all.sort();
    let mut seq: Vec<(usize, i32)> = Vec::new();
    for (i, k) in all {
        if let Some(&(j, kl)) = seq.last() {
            if kl == k {
                if k as f64 * xn[i] > k as f64 * xn[j] {
                    *seq.last_mut().unwrap() = (i, k);
                }
                continue;
            }
        }
        seq.push((i, k));
    }
    while seq.len() > 4 {
        let sw: Vec<f64> = seq.windows(2).map(|p| (x[p[1].0] - x[p[0].0]).abs()).collect();
        let inner = &sw[1..sw.len() - 1];
        let j = 1 + dsp::argmin(inner);
        if sw[j] >= 0.3 * dsp::median(&sw) {
            break;
        }
        seq.drain(j..j + 2);
    }
    let mut b = Breaths { sign, ..Default::default() };
    let (mut trv, mut pkv, mut nxv, mut risev, mut slopev, mut depthv) = (vec![], vec![], vec![], vec![], vec![], vec![]);
    for j in 0..seq.len().saturating_sub(2) {
        if seq[j].1 == -1 && seq[j + 1].1 == 1 {
            let (t0, p, t1) = (seq[j].0, seq[j + 1].0, seq[j + 2].0);
            let seg = &x[t0..=p];
            let thr = x[t0] + 0.2 * (x[p] - x[t0]);
            let rise = t0 + seg.iter().position(|&v| v >= thr).unwrap_or(0);
            let slope = t0 + dsp::argmax(&dsp::gradient(seg));
            trv.push(t0);
            pkv.push(p);
            nxv.push(t1);
            risev.push(rise);
            slopev.push(slope);
            depthv.push(x[p] - (x[t0] + x[t1]) / 2.0);
        }
    }
    let s = |v: &Vec<usize>| v.iter().map(|&i| i as f64 / FS).collect::<Vec<f64>>();
    b.tr = s(&trv);
    b.pk = s(&pkv);
    b.nxt = s(&nxv);
    b.rise = s(&risev);
    b.slope = s(&slopev);
    b.depth = depthv;
    let nb = b.tr.len();
    let mut nr = vec![f64::NAN; nb];
    for i in 0..nb.saturating_sub(1) {
        if b.tr[i + 1] == b.nxt[i] {
            nr[i] = b.rise[i + 1];
        }
    }
    b.ttot = (0..nb).map(|i| if nr[i].is_nan() { b.nxt[i] - b.tr[i] } else { nr[i] - b.rise[i] }).collect();
    b.ti = (0..nb).map(|i| b.pk[i] - b.rise[i]).collect();
    b.te = (0..nb).map(|i| b.ttot[i] - b.ti[i]).collect();
    b.ti_frac = (0..nb).map(|i| if b.ttot[i] > 0.0 { b.ti[i] / b.ttot[i] } else { f64::NAN }).collect();

    // breathing present: nearest 20 s or 30 s Welch window has a clear peak
    let ts: Vec<f64> = (0..n).map(|i| i as f64 / FS).collect();
    let mut best = vec![0.0f64; n];
    for win in [20.0, 30.0] {
        let (tcw, _, cw) = welch_series(&x, &q, win);
        if !tcw.is_empty() {
            let l = tcw.len();
            for i in 0..n {
                let j = searchsorted_left(&tcw, ts[i]).min(l - 1);
                let jm = j.saturating_sub(1).min(l - 1);
                let jj = if (tcw[jm] - ts[i]).abs() < (tcw[j] - ts[i]).abs() { jm } else { j };
                best[i] = best[i].max(cw[jj]);
            }
        }
    }
    let mut present: Vec<bool> = best.iter().map(|&v| v >= PRESENCE_CONF).collect();
    let pi: Vec<u8> = present.iter().map(|&b| b as u8).collect();
    for (lo, hi, _) in dsp::runs(&pi) {
        if ((hi - lo) as f64 / FS) < PRESENCE_MIN_S && lo > 0 && hi < n {
            for v in &mut present[lo..hi] {
                *v = false;
            }
        }
    }
    let npi: Vec<u8> = present.iter().map(|&b| (!b) as u8).collect();
    for (lo, hi, _) in dsp::runs(&npi) {
        if lo > 0 && hi < n && ((hi - lo) as f64 / FS) < PRESENCE_BRIDGE_S {
            for v in &mut present[lo..hi] {
                *v = true;
            }
        }
    }

    let mut ok = vec![true; nb];
    let mut why = vec![""; nb];
    for i in 0..nb {
        let a = (b.tr[i] * FS) as usize;
        let e = ((b.nxt[i] * FS) as usize + 1).min(n);
        let len = (e - a).max(1) as f64;
        let pres = present[a..e].iter().filter(|&&v| v).count() as f64 / len;
        let mo = q[a..e].iter().any(|&v| v == 2);
        let weak = q[a..e].iter().filter(|&&v| v == 1).count() as f64 / len;
        if pres < 0.5 {
            ok[i] = false;
            why[i] = "no breathing signal";
        } else if mo {
            ok[i] = false;
            why[i] = "motion";
        } else if weak > 0.5 {
            ok[i] = false;
            why[i] = "weak";
        } else if !(60.0 / 45.0 < b.ttot[i] && b.ttot[i] < 60.0 / 4.0) {
            ok[i] = false;
            why[i] = "out of range";
        }
    }
    for i in 0..nb {
        if !ok[i] {
            continue;
        }
        let nbv: Vec<f64> = (i.saturating_sub(3)..(i + 4).min(nb)).filter(|&j| j != i && ok[j]).map(|j| b.ttot[j]).collect();
        let loc = if nbv.len() >= 2 { dsp::median(&nbv) } else { b.ttot[i] };
        let pri = 60.0 / prior[((b.tr[i] * FS) as usize).min(n - 1)];
        let t = b.ttot[i];
        let fits_loc = 0.55 * loc < t && t < 1.7 * loc;
        let fits_pri = 0.6 * pri < t && t < 1.6 * pri;
        if !(fits_loc || fits_pri) {
            ok[i] = false;
            why[i] = "implausible (split/missed?)";
        }
    }
    b.x = x;
    b.q = q;
    b.prior = prior;
    b.ok = ok;
    b.why = why;
    b.present = present;
    b
}

fn median_k(b: &Breaths, k: usize) -> (Vec<f64>, Vec<f64>) {
    let g: Vec<usize> = (0..b.len()).filter(|&i| b.ok[i]).collect();
    let t: Vec<f64> = g.iter().map(|&i| b.rise[i] + b.ttot[i] / 2.0).collect();
    let v: Vec<f64> = (0..g.len())
        .map(|n| {
            let lo = n.saturating_sub(k / 2);
            let hi = (n + k / 2 + 1).min(g.len());
            60.0 / dsp::median(&g[lo..hi].iter().map(|&i| b.ttot[i]).collect::<Vec<_>>())
        })
        .collect();
    (t, v)
}

pub fn hybrid_rate(b: &Breaths) -> (Vec<f64>, Vec<f64>) {
    median_k(b, 3)
}

/// Final rate on a 2.5 s grid: median of Welch 20 s, Welch 30 s and median-of-3 breaths.
pub fn fused_rate(b: &Breaths) -> (Vec<f64>, Vec<f64>) {
    let (t20, r20, c20) = welch_series(&b.x, &b.q, 20.0);
    let (t30a, r30a, c30) = welch_series(&b.x, &b.q, 30.0);
    let r20: Vec<f64> = r20.iter().zip(&c20).map(|(r, c)| if *c >= CONF_MIN { *r } else { f64::NAN }).collect();
    let (t30, r30): (Vec<f64>, Vec<f64>) =
        t30a.iter().zip(&r30a).zip(&c30).filter(|(_, c)| **c >= CONF_MIN).map(|((t, r), _)| (*t, *r)).unzip();
    if t20.is_empty() {
        return (vec![], vec![]);
    }
    let (t3, v3) = median_k(b, 3);
    let e30: Vec<f64> = if t30.is_empty() { vec![f64::NAN; t20.len()] } else { dsp::interp(&t20, &t30, &r30) };
    let near: Vec<bool> = t20.iter().map(|&t| t3.iter().any(|&u| (t - u).abs() < 6.0)).collect();
    let mut m3 = vec![f64::NAN; t20.len()];
    if !t3.is_empty() {
        let iv = dsp::interp(&t20, &t3, &v3);
        for i in 0..t20.len() {
            if near[i] {
                m3[i] = iv[i];
            }
        }
    }
    let mut out: Vec<f64> = (0..t20.len())
        .map(|i| {
            let mut col = vec![r20[i], e30[i]];
            if !t3.is_empty() {
                col.push(m3[i]);
            }
            dsp::nanmedian(&col)
        })
        .collect();
    if t3.len() >= 3 {
        let g: Vec<usize> = (0..b.len()).filter(|&i| b.ok[i]).collect();
        let cv: Vec<f64> = (0..g.len())
            .map(|n| {
                if n >= 2 {
                    let v: Vec<f64> = g[n - 2..=n].iter().map(|&i| b.ttot[i]).collect();
                    dsp::std(&v) / dsp::mean(&v)
                } else {
                    1.0
                }
            })
            .collect();
        let icv = dsp::interp(&t20, &t3, &cv);
        for i in 0..t20.len() {
            let steady = (if near[i] { icv[i] } else { 1.0 }) < 0.15;
            let wel = dsp::nanmedian(&[r20[i], e30[i]]);
            if steady && m3[i].is_finite() && (!wel.is_finite() || (m3[i] - wel).abs() > 2.0) {
                out[i] = m3[i];
            }
        }
    }
    let np_ = b.present.len();
    for i in 0..t20.len() {
        let k = ((t20[i] * FS) as usize).min(np_ - 1);
        if !b.present[k] {
            out[i] = f64::NAN;
        }
    }
    (t20, out)
}

/// EXPERIMENTAL heart rate: (centres, BPM, confidence).
pub fn heart_rate(h: &CMat, b: &Breaths) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let r = amplitude_features(h);
    let f = filt_cols(&r, 3, HEART_BAND);
    let (fr, ps) = welch_cols(&r, None);
    let snr = band_ratio(&fr, &ps, HEART_BAND, (2.5, 5.0));
    let thr = dsp::percentile(&snr, 75.0);
    let sel: Vec<usize> = (0..snr.len()).filter(|&j| snr[j] >= thr).collect();
    let mut fs = f.select_cols(&sel);
    normalize_cols(&mut fs);
    let (u0, _, _) = pca(&fs);
    let wgt: Vec<f64> = sel.iter().map(|&j| snr[j]).collect();
    let hs = signed_weighted_sum(&fs, &u0, &wgt);
    let n = (20.0 * FS) as usize;
    let st = (5.0 * FS) as usize;
    let (mut tc, mut rr, mut cc) = (vec![], vec![], vec![]);
    if hs.len() < n {
        return (tc, rr, cc);
    }
    let mut psd = Psd::new(dsp::hann(n), 4096, FS);
    let fw = psd.freqs(FS);
    let mut s0 = 0;
    while s0 + n <= hs.len() {
        let mo = b.q[s0..s0 + n].iter().filter(|&&v| v == 2).count() as f64 / n as f64;
        if mo <= 0.1 {
            let pw = psd.welch(&hs[s0..s0 + n]);
            let fb = b.prior[(s0 + n / 2).min(b.prior.len() - 1)] / 60.0;
            let m: Vec<bool> = fw
                .iter()
                .map(|&f| f >= HEART_BAND.0 && f <= HEART_BAND.1 && (2..11).all(|k| (f - k as f64 * fb).abs() > 0.03))
                .collect();
            if m.iter().any(|&v| v) {
                let masked: Vec<f64> = pw.iter().zip(&m).map(|(p, &mm)| if mm { *p } else { 0.0 }).collect();
                let i = dsp::argmax(&masked);
                let pm: Vec<f64> = pw.iter().zip(&m).filter(|(_, mm)| **mm).map(|(p, _)| *p).collect();
                tc.push((s0 as f64 + n as f64 / 2.0) / FS);
                rr.push(60.0 * fw[i]);
                cc.push(pw[i] / (dsp::median(&pm) + 1e-12));
            }
        }
        s0 += st;
    }
    (tc, rr, cc)
}

// ----------------------------------------------------------------- text helpers
/// Python-style fixed formatting: NaN prints as "nan" (Rust prints "NaN").
pub fn nf(v: f64, width: usize, prec: usize) -> String {
    if v.is_nan() { format!("{:>width$}", "nan") } else { format!("{:>width$.prec$}", v) }
}

/// Python repr of a float (15.0 -> "15.0").
pub fn pyf(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    let s = format!("{v}");
    if s.contains('.') || s.contains('e') || s.contains("inf") { s } else { s + ".0" }
}

pub fn breath_summary(b: &Breaths) -> String {
    let n = b.q.len() as f64;
    let okn = b.ok.iter().filter(|&&v| v).count();
    let pres = 100.0 * b.present.iter().filter(|&&v| v).count() as f64 / n;
    let cnt = |k: u8| 100.0 * b.q.iter().filter(|&&v| v == k).count() as f64 / n;
    let mut s = format!(
        "breaths {} detected, {} usable; breathing present {:.0}% of the time; signal usable {:.0}% (weak {:.0}%, motion {:.0}%)",
        b.len(),
        okn,
        pres,
        cnt(0),
        cnt(1),
        cnt(2)
    );
    if okn >= 3 {
        let ib: Vec<f64> = (0..b.len()).filter(|&i| b.ok[i]).map(|i| b.ttot[i]).collect();
        let cons: Vec<f64> = (0..b.len().saturating_sub(1)).filter(|&i| b.ok[i] && b.ok[i + 1]).map(|i| b.ttot[i + 1] - b.ttot[i]).collect();
        let _ = write!(s, "\nper-breath: median {:.1} BPM, SDNN {:.0} ms", 60.0 / dsp::median(&ib), 1000.0 * dsp::std(&ib));
        if !cons.is_empty() {
            let _ = write!(s, ", RMSSD {:.0} ms", 1000.0 * (dsp::mean(&cons.iter().map(|v| v * v).collect::<Vec<_>>())).sqrt());
        }
        let tf: Vec<f64> = (0..b.len()).filter(|&i| b.ok[i]).map(|i| b.ti_frac[i]).collect();
        let _ = write!(s, ", inhale fraction Ti/Ttot {:.2}", dsp::nanmedian(&tf));
    }
    s
}

pub fn heart_summary(hr: &(Vec<f64>, Vec<f64>, Vec<f64>), ref_hr: Option<f64>) -> String {
    let (_, r, c) = hr;
    if r.is_empty() {
        return "heart rate (experimental): no usable window".into();
    }
    let good: Vec<f64> = r.iter().zip(c).filter(|(_, c)| **c >= HR_CONF_MIN).map(|(r, _)| *r).collect();
    let mut s = format!("heart rate (experimental, unvalidated): {}/{} windows above confidence {:.0}", good.len(), r.len(), HR_CONF_MIN);
    if !good.is_empty() {
        let _ = write!(
            s,
            ", median {:.0} BPM (range {:.0}-{:.0})",
            dsp::median(&good),
            good.iter().cloned().fold(f64::INFINITY, f64::min),
            good.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
        );
    } else {
        s += " -> no reliable reading";
    }
    if let (Some(h), false) = (ref_hr, good.is_empty()) {
        let _ = write!(s, "; vs your reference {:.0} BPM: error {:.1} BPM", h, (dsp::median(&good) - h).abs());
    }
    s
}

pub fn write_breaths_csv(b: &Breaths, path: &str) -> std::io::Result<()> {
    let mut s = String::from("onset_s,trough_s,end_inhale_s,next_onset_s,bpm,ti_s,te_s,ti_frac,depth,ok,why\r\n");
    for i in 0..b.len() {
        let _ = writeln!(
            s,
            "{:.2},{:.2},{:.2},{:.2},{:.1},{:.2},{:.2},{:.2},{:.3},{},{}\r",
            b.rise[i],
            b.tr[i],
            b.pk[i],
            b.rise[i] + b.ttot[i],
            60.0 / b.ttot[i],
            b.ti[i],
            b.te[i],
            b.ti_frac[i],
            b.depth[i],
            b.ok[i] as u8,
            b.why[i]
        );
    }
    std::fs::write(path, s)
}

// ----------------------------------------------------------------- report
/// Everything the breath test shows: text report plus the series for the charts.
#[derive(Clone, Default)]
pub struct Report {
    pub text: String,
    pub tg_len: usize,
    pub breaths: Breaths,
    pub windows: Vec<(f64, f64, f64)>,
    pub hybrid: (Vec<f64>, Vec<f64>),
    pub fused: (Vec<f64>, Vec<f64>),
    pub heart: (Vec<f64>, Vec<f64>, Vec<f64>),
    pub methods: Vec<(Method, Vec<f64>, Vec<f64>)>,
    /// fused-rate median (BPM) or None if no breathing
    pub est: Option<f64>,
    /// MAE of the confident 30 s windows vs the reference
    pub mae: Option<f64>,
    pub hr: Option<f64>,
    pub t: Vec<f64>,
    pub seq: Vec<i64>,
}

/// breath_analyze.report on a parsed recording (text is what the Python prints).
pub fn report(rec: &Recording, reference: Option<f64>, ref_hr: Option<f64>, with_methods: bool) -> anyhow::Result<Report> {
    let mut out = String::new();
    if rec.bad > 0 {
        let _ = writeln!(out, "skipped {} malformed lines", rec.bad);
    }
    let span = (rec.seq.last().unwrap() - rec.seq[0] + 1) as f64;
    let np_ = rec.npkt() as f64;
    let tl = *rec.t.last().unwrap();
    let _ = writeln!(
        out,
        "packets {} over {:.1}s ({:.1} pkt/s), loss {:.1}%, RSSI {:.0} dBm, subcarriers {}",
        rec.npkt(),
        tl,
        np_ / tl,
        100.0 * (1.0 - np_ / span),
        rec.rssi.iter().map(|&r| r as f64).sum::<f64>() / np_,
        rec.nsc
    );
    let (tg, hr_) = resample(rec);
    let (w, ev, _) = breathing_waveform(&hr_, Method::Snr);
    let wins = windows(&w);
    if wins.is_empty() {
        anyhow::bail!("{out}need >= {:.0}s of data", WIN_S);
    }
    let good: Vec<(f64, f64, f64)> = wins.iter().copied().filter(|w| w.2 >= PRESENCE_CONF).collect();
    let _ = writeln!(out, "PC1 explains {:.0}% of band energy; {}/{} windows above confidence {}", 100.0 * ev, good.len(), wins.len(), PRESENCE_CONF);
    for &(t0, bpm, q) in &wins {
        let _ = writeln!(out, "  t={:6.1}s  {:5.1} BPM  conf {:5.1}{}", t0, bpm, q, if q >= PRESENCE_CONF { "" } else { "  (low)" });
    }
    if !good.is_empty() {
        let _ = writeln!(out, "median BPM {:.1}", dsp::median(&good.iter().map(|g| g.1).collect::<Vec<_>>()));
    }
    let b = detect_breaths(&w, None, None);
    let _ = writeln!(out, "{}", breath_summary(&b));
    let hy = hybrid_rate(&b);
    let fu = fused_rate(&b);
    let heart = heart_rate(&hr_, &b);
    let _ = writeln!(out, "{}", heart_summary(&heart, ref_hr));
    let fin: Vec<f64> = fu.1.iter().copied().filter(|v| v.is_finite()).collect();
    let est = if !fin.is_empty() {
        let _ = writeln!(
            out,
            "fused rate: median {:.1} BPM (range {:.1}-{:.1}), reading {:.0}% of the time",
            dsp::median(&fin),
            fin.iter().cloned().fold(f64::INFINITY, f64::min),
            fin.iter().cloned().fold(f64::NEG_INFINITY, f64::max),
            100.0 * fin.len() as f64 / fu.1.len() as f64
        );
        Some((dsp::median(&fin) * 10.0).round() / 10.0)
    } else {
        let _ = writeln!(out, "fused rate: no breathing detected");
        None
    };
    let mut mae = None;
    if let (Some(r), false) = (reference, good.is_empty()) {
        let err: Vec<f64> = good.iter().map(|g| (g.1 - r).abs()).collect();
        let m = dsp::mean(&err);
        mae = Some(m);
        let _ = writeln!(
            out,
            "vs reference {} BPM: MAE {:.2}, within +-1 BPM {:.0}%, within +-2 BPM {:.0}%",
            pyf(r),
            m,
            100.0 * err.iter().filter(|&&e| e <= 1.0).count() as f64 / err.len() as f64,
            100.0 * err.iter().filter(|&&e| e <= 2.0).count() as f64 / err.len() as f64
        );
    }
    let hgood: Vec<f64> = heart.1.iter().zip(&heart.2).filter(|(_, c)| **c >= HR_CONF_MIN).map(|(r, _)| *r).collect();
    let methods = if with_methods { method_rates(&hr_) } else { vec![] };
    Ok(Report {
        text: out,
        tg_len: tg.len(),
        breaths: b,
        windows: wins,
        hybrid: hy,
        fused: fu,
        heart,
        methods,
        est,
        mae,
        hr: if hgood.is_empty() { None } else { Some(dsp::median(&hgood).round()) },
        t: rec.t.clone(),
        seq: rec.seq.clone(),
    })
}

/// Fused rate from each subcarrier-combination method.
pub fn method_rates(h: &CMat) -> Vec<(Method, Vec<f64>, Vec<f64>)> {
    use rayon::prelude::*;
    METHODS
        .par_iter()
        .map(|&m| {
            let (w, _, _) = breathing_waveform(h, m);
            let (t, v) = fused_rate(&detect_breaths(&w, None, None));
            (m, t, v)
        })
        .collect()
}

// ----------------------------------------------------------------- taps
/// host_time - chip_time: the minimum over packets seen by both clocks.
pub fn chip_offset(t: &[f64], seq: &[i64], host_seq: &[i64], host_t: &[f64]) -> Option<f64> {
    let hs: std::collections::HashMap<i64, f64> = host_seq.iter().copied().zip(host_t.iter().copied()).collect();
    let d: Vec<f64> = seq.iter().zip(t).filter_map(|(s, ti)| hs.get(s).map(|h| h - ti)).collect();
    if d.is_empty() { None } else { Some(d.iter().cloned().fold(f64::INFINITY, f64::min)) }
}

/// Drop double taps, mark missed-tap gaps: (taps, usable interval mask, note).
pub fn clean_taps(taps: &[f64]) -> (Vec<f64>, Vec<bool>, String) {
    let ibi: Vec<f64> = taps.windows(2).map(|w| w[1] - w[0]).collect();
    let med = dsp::median(&ibi);
    let mut keep = vec![true; taps.len()];
    for i in 1..taps.len() {
        let prev = taps[(0..i).rev().find(|&j| keep[j]).unwrap()];
        if taps[i] - prev < 0.4 * med {
            keep[i] = false;
        }
    }
    let t2: Vec<f64> = taps.iter().zip(&keep).filter(|(_, k)| **k).map(|(t, _)| *t).collect();
    let ibi: Vec<f64> = t2.windows(2).map(|w| w[1] - w[0]).collect();
    let m2 = dsp::median(&ibi);
    let use_: Vec<bool> = ibi.iter().map(|&v| v < 1.7 * m2).collect();
    let mut note = vec![];
    let dropped = keep.iter().filter(|k| !**k).count();
    if dropped > 0 {
        note.push(format!("removed {dropped} double tap(s)"));
    }
    let gaps = use_.iter().filter(|u| !**u).count();
    if gaps > 0 {
        note.push(format!("{gaps} gap(s) look like a missed tap, not used for rate"));
    }
    (t2, use_, note.join("; "))
}

/// Taps -> (interval midpoints in chip s, BPM per interval, BPM median of 3).
pub fn tap_rates(taps_host: &[f64], off: f64, t_end: f64) -> Option<(Vec<f64>, Vec<f64>, Vec<f64>)> {
    let mut taps: Vec<f64> = taps_host.iter().map(|t| t - off).filter(|&t| t > 2.0 && t < t_end - 3.0).collect();
    taps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if taps.len() < 4 {
        return None;
    }
    let (taps, use_, _) = clean_taps(&taps);
    let ibi_all: Vec<f64> = taps.windows(2).map(|w| w[1] - w[0]).collect();
    let mid_all: Vec<f64> = taps.windows(2).map(|w| (w[0] + w[1]) / 2.0).collect();
    let ib: Vec<f64> = ibi_all.iter().zip(&use_).filter(|(_, u)| **u).map(|(v, _)| *v).collect();
    let rt: Vec<f64> = mid_all.iter().zip(&use_).filter(|(_, u)| **u).map(|(v, _)| *v).collect();
    let rr3 = (0..ib.len()).map(|i| 60.0 / dsp::median(&ib[i.saturating_sub(1)..(i + 2).min(ib.len())])).collect();
    Some((rt, ib.iter().map(|v| 60.0 / v).collect(), rr3))
}

fn match_events(taps: &[f64], ev: &[f64], tol: f64) -> Vec<(f64, f64)> {
    let mut used = std::collections::HashSet::new();
    let mut pairs = vec![];
    for &tp in taps {
        let mut j: Vec<usize> = (0..ev.len()).collect();
        j.sort_by(|&a, &b| (ev[a] - tp).abs().partial_cmp(&(ev[b] - tp).abs()).unwrap());
        for &k in j.iter().take(2) {
            if (ev[k] - tp).abs() <= tol && !used.contains(&k) {
                used.insert(k);
                pairs.push((tp, ev[k]));
                break;
            }
        }
    }
    pairs
}

fn breath_events(w: &[f64], trough: bool, min_ibi: f64) -> Vec<f64> {
    let sos = dsp::butter_bandpass(2, BAND.0, 0.8, FS);
    let x = dsp::sosfiltfilt(&sos, w);
    let sq: Vec<f64> = x.iter().map(|v| v * v).collect();
    let n8 = (8.0 * FS) as usize;
    let env = dsp::convolve_box_same(&sq, n8, 1.0 / n8 as f64);
    let xn: Vec<f64> = x.iter().zip(&env).map(|(a, e)| a / (e.max(0.0).sqrt() + 1e-9) * if trough { -1.0 } else { 1.0 }).collect();
    dsp::find_peaks(&xn, Some((min_ibi * FS) as usize), Some(0.5)).iter().map(|&i| i as f64 / FS).collect()
}

fn trajectory_turns(x3: &[Vec<f64>], min_gap: f64, h: f64) -> (Vec<f64>, Vec<f64>) {
    let n = x3[0].len();
    let d = x3.len();
    let v: Vec<Vec<f64>> = x3.iter().map(|c| dsp::gradient(c).iter().map(|g| g * FS).collect()).collect();
    let s: Vec<f64> = (0..n).map(|i| (0..d).map(|k| v[k][i] * v[k][i]).sum::<f64>().sqrt()).collect();
    let k = (h * FS) as usize;
    let mut ang = vec![0.0; n];
    for i in k..n.saturating_sub(k) {
        let (a, b) = (i - k, i + k);
        let dotp: f64 = (0..d).map(|c| v[c][a] * v[c][b]).sum();
        let na = (0..d).map(|c| v[c][a] * v[c][a]).sum::<f64>().sqrt();
        let nb = (0..d).map(|c| v[c][b] * v[c][b]).sum::<f64>().sqrt();
        ang[i] = (dotp / (na * nb + 1e-12)).clamp(-1.0, 1.0).acos().to_degrees();
    }
    let n10 = (10.0 * FS) as usize;
    let loc: Vec<f64> = dsp::convolve_box_same(&s, n10, 1.0 / n10 as f64).iter().map(|v| v + 1e-12).collect();
    let ns: Vec<f64> = s.iter().zip(&loc).map(|(a, b)| -a / b).collect();
    let mins: Vec<usize> = dsp::find_peaks(&ns, Some((min_gap * FS) as usize), None).into_iter().filter(|&i| ang[i] > 90.0).collect();
    let slow: Vec<bool> = s.iter().zip(&loc).map(|(a, b)| *a < 0.5 * b).collect();
    let dw = mins
        .iter()
        .map(|&i| {
            let (mut l, mut r) = (i, i);
            while l > 0 && slow[l - 1] {
                l -= 1;
            }
            while r < n - 1 && slow[r + 1] {
                r += 1;
            }
            if slow[i] { (r - l + 1) as f64 / FS } else { 0.0 }
        })
        .collect();
    (mins.iter().map(|&i| i as f64 / FS).collect(), dw)
}

fn long_pause_turns(tt: &[f64], dw: &[f64]) -> Vec<f64> {
    (0..tt.len())
        .filter(|&i| {
            let prev = if i > 0 { dw[i - 1] } else { 0.0 };
            let next = if i + 1 < tt.len() { dw[i + 1] } else { 0.0 };
            dw[i] >= prev && dw[i] > next
        })
        .map(|i| tt[i])
        .collect()
}

#[derive(Clone, Default)]
pub struct TapReport {
    pub text: String,
    /// ordered (column, value) for tap_sessions.csv
    pub row: Vec<(String, String)>,
    pub breaths: Breaths,
    pub taps: Vec<f64>,
    pub lag: f64,
    pub seg_t: Vec<f64>,
    pub segs: Vec<Vec<f64>>,
    pub avg: Vec<f64>,
    pub jitter_ms: Vec<f64>,
    pub rt: Vec<f64>,
    pub rr: Vec<f64>,
    pub rr3: Vec<f64>,
}

/// breath_analyze.tap_report: CSI vs the inhale taps.
pub fn tap_report(rec: &Recording, taps_host: &[f64], host_seq: &[i64], host_t: &[f64]) -> anyhow::Result<TapReport> {
    let mut out = String::new();
    if rec.bad > 0 {
        let _ = writeln!(out, "skipped {} malformed lines", rec.bad);
    }
    let off = chip_offset(&rec.t, &rec.seq, host_seq, host_t).ok_or_else(|| anyhow::anyhow!("no host timing to align taps"))?;
    let (tg, hr_) = resample(rec);
    let (w0, _, x3) = breathing_waveform(&hr_, Method::Snr);
    let wm = dsp::mean(&w0);
    let ws = dsp::std(&w0) + 1e-9;
    let mut w: Vec<f64> = w0.iter().map(|v| (v - wm) / ws).collect();
    let tl = *tg.last().unwrap();
    let mut taps: Vec<f64> = taps_host.iter().map(|t| t - off).filter(|&t| t > 2.0 && t < tl - 3.0).collect();
    taps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if taps.len() < 4 {
        anyhow::bail!("need >= 4 taps inside the recording");
    }
    let (taps, tap_use, note) = clean_taps(&taps);
    let tap_ibi: Vec<f64> = taps.windows(2).map(|p| p[1] - p[0]).collect();
    let ibu: Vec<f64> = tap_ibi.iter().zip(&tap_use).filter(|(_, u)| **u).map(|(v, _)| *v).collect();
    let _ = writeln!(
        out,
        "taps {}, mean interval {:.2}s ({:.1} BPM), tap SDNN {:.0} ms{}",
        taps.len(),
        dsp::mean(&ibu),
        60.0 / dsp::mean(&ibu),
        1000.0 * dsp::std(&ibu),
        if note.is_empty() { String::new() } else { format!("  [{note}]") }
    );
    // breath-triggered average
    let pre = (1.5 * FS) as usize;
    let post = (4.0 * FS) as usize;
    let idx: Vec<usize> = taps.iter().map(|t| (t * FS) as usize).filter(|&i| i > pre && i + post < w.len()).collect();
    let mut segs: Vec<Vec<f64>> = idx.iter().map(|&i| w[i - pre..i + post].to_vec()).collect();
    let lag_t: Vec<f64> = (0..pre + post).map(|i| (i as f64 - pre as f64) / FS).collect();
    let mut avg: Vec<f64> = (0..pre + post).map(|k| dsp::mean(&segs.iter().map(|s| s[k]).collect::<Vec<_>>())).collect();
    let mean_where = |a: &[f64], lo: f64, hi: f64| dsp::mean(&lag_t.iter().zip(a).filter(|(t, _)| **t > lo && **t < hi).map(|(_, v)| *v).collect::<Vec<_>>());
    let rise = mean_where(&avg, 0.0, 1.0) - mean_where(&avg, -0.5, 0.0);
    let tap_sign = if rise >= 0.0 { 1.0 } else { -1.0 };
    for v in w.iter_mut() {
        *v *= tap_sign;
    }
    for s in segs.iter_mut() {
        for v in s.iter_mut() {
            *v *= tap_sign;
        }
    }
    for v in avg.iter_mut() {
        *v *= tap_sign;
    }
    let win: Vec<usize> = (0..lag_t.len()).filter(|&i| lag_t[i] > -1.5 && lag_t[i] < 1.5).collect();
    let tr_lag = lag_t[win[dsp::argmin(&win.iter().map(|&i| avg[i]).collect::<Vec<_>>())]];
    let after: Vec<usize> = (0..lag_t.len()).filter(|&i| lag_t[i] > tr_lag).collect();
    let pk_lag = lag_t[after[dsp::argmax(&after.iter().map(|&i| avg[i]).collect::<Vec<_>>())]];
    let sstd: Vec<f64> = (0..lag_t.len()).map(|k| dsp::std(&segs.iter().map(|s| s[k]).collect::<Vec<_>>())).collect();
    let snr = avg.iter().fold(0.0f64, |m, v| m.max(v.abs())) / (dsp::mean(&sstd) + 1e-9);
    let _ = writeln!(
        out,
        "triggered average over {} breaths: trough at {:+.2}s, peak at {:+.2}s from the tap; average/scatter {:.2} (>1 = breaths look alike)",
        segs.len(),
        tr_lag,
        pk_lag,
        snr
    );
    let mut br = detect_breaths(&w, None, None);
    let blind_ok = br.sign == 1;
    let _ = writeln!(
        out,
        "combined detector: sign chosen without taps {} with the taps (troughs = inhale onset)",
        if blind_ok { "AGREES" } else { "DISAGREES" }
    );
    if !blind_ok {
        br = detect_breaths(&w, Some(1), None);
        let _ = writeln!(out, "  -> re-run with the taps' direction for scoring and the breaths CSV");
    }
    let _ = writeln!(out, "{}", breath_summary(&br));
    let tol = (0.25 * dsp::median(&tap_ibi)).min(0.75);
    let score = |ev: &[f64]| -> (f64, f64, f64, f64) {
        let near: Vec<f64> = if ev.is_empty() {
            vec![]
        } else {
            taps.iter()
                .map(|&tp| {
                    let d: Vec<f64> = ev.iter().map(|e| (e - tp).abs()).collect();
                    ev[dsp::argmin(&d)] - tp
                })
                .filter(|d| d.abs() <= 1.5)
                .collect()
        };
        let lag = if near.is_empty() { 0.0 } else { dsp::median(&near) };
        let shifted: Vec<f64> = taps.iter().map(|t| t + lag).collect();
        let pairs = match_events(&shifted, ev, tol);
        let span = ev.iter().filter(|&&e| e > taps[0] - tol + lag && e < taps[taps.len() - 1] + tol + lag).count();
        let rec_ = pairs.len() as f64 / taps.len() as f64;
        let prec = pairs.len() as f64 / span.max(1) as f64;
        let jit = if pairs.is_empty() { f64::NAN } else { dsp::std(&pairs.iter().map(|(tp, e)| e - tp).collect::<Vec<_>>()) };
        (lag, rec_, prec, jit)
    };
    let _ = writeln!(out, "single-breath detection vs taps (constant tap lead/lag removed):");
    let (tt, dw) = trajectory_turns(&x3, 0.8, 0.4);
    let usable: Vec<usize> = (0..br.len()).filter(|&i| br.ok[i]).collect();
    let mut cands: Vec<(String, Vec<f64>)> =
        vec![("raw trough".into(), breath_events(&w, true, 1.2)), ("raw turn".into(), long_pause_turns(&tt, &dw))];
    for mk in ["tr", "rise", "slope"] {
        cands.push((format!("combined {mk}"), usable.iter().map(|&i| br.marker(mk)[i]).collect()));
    }
    let mut res = std::collections::HashMap::new();
    for (name, ev) in &cands {
        let r = score(ev);
        let _ = writeln!(
            out,
            "  {:15}: lag {:+.2}s, found {:3.0}% of taps, {:3.0}% real, jitter {} ms",
            name,
            r.0,
            100.0 * r.1,
            100.0 * r.2,
            nf(1000.0 * r.3, 4, 0)
        );
        res.insert(name.clone(), r);
    }
    let mk = *["tr", "rise", "slope"]
        .iter()
        .min_by(|a, b| {
            let ja = res[&format!("combined {a}")].3;
            let jb = res[&format!("combined {b}")].3;
            let (ja, jb) = (if ja.is_nan() { 9.0 } else { ja }, if jb.is_nan() { 9.0 } else { jb });
            ja.partial_cmp(&jb).unwrap()
        })
        .unwrap();
    let (lag, rec_, prec, jit) = res[&format!("combined {mk}")];
    let names = |k: &str| match k {
        "tr" => "trough",
        "rise" => "20% rise",
        _ => "steepest rise",
    };
    let _ = writeln!(out, "  -> best onset marker: {} (lag {:+.2}s vs your tap)", names(mk), lag);
    // rate tracking
    let mids: Vec<f64> = taps.windows(2).map(|p| (p[0] + p[1]) / 2.0).collect();
    let rt: Vec<f64> = mids.iter().zip(&tap_use).filter(|(_, u)| **u).map(|(v, _)| *v).collect();
    let rr: Vec<f64> = ibu.iter().map(|v| 60.0 / v).collect();
    let rr3: Vec<f64> = (0..ibu.len()).map(|i| 60.0 / dsp::median(&ibu[i.saturating_sub(1)..(i + 2).min(ibu.len())])).collect();
    let ct: Vec<f64> = usable.iter().map(|&i| br.rise[i] + br.ttot[i] / 2.0).collect();
    let cr: Vec<f64> = usable.iter().map(|&i| 60.0 / br.ttot[i]).collect();
    let hy = hybrid_rate(&br);
    let wins = windows(&br.x);
    let fu = fused_rate(&br);
    let err_at = |tq: &[f64], vq: &[f64], rf: &[f64]| -> Vec<f64> {
        if tq.len() < 2 {
            return vec![f64::NAN];
        }
        let mut e = vec![];
        for (t_, r_) in rt.iter().zip(rf) {
            let d: Vec<f64> = tq.iter().map(|t| (t - lag - t_).abs()).collect();
            let j = dsp::argmin(&d);
            if d[j] < 0.6 * 60.0 / r_ {
                e.push((vq[j] - r_).abs());
            }
        }
        if e.is_empty() { vec![f64::NAN] } else { e }
    };
    let welch_err = |rf: &[f64]| -> Vec<f64> {
        let mut e = vec![];
        for &(tc, bpm, _) in &wins {
            let sel: Vec<f64> = rt.iter().zip(rf).filter(|(t, _)| **t > tc - WIN_S / 2.0 && **t < tc + WIN_S / 2.0).map(|(_, r)| 60.0 / r).collect();
            if sel.len() >= 2 {
                e.push((bpm - 60.0 / dsp::mean(&sel)).abs());
            }
        }
        if e.is_empty() { vec![f64::NAN] } else { e }
    };
    let keys = ["per breath", "median of 3", "Welch 30s window", "fused"];
    let mut errs: Vec<(&str, Vec<Vec<f64>>)> = vec![];
    for (name, rf) in [("raw", &rr), ("m3", &rr3)] {
        errs.push((name, vec![err_at(&ct, &cr, rf), err_at(&hy.0, &hy.1, rf), welch_err(rf), err_at(&fu.0, &fu.1, rf)]));
    }
    // (NaN <= 2 is False in numpy, so entries without a reading count as misses)
    let fmt = |e: &[f64]| {
        let within = e.iter().filter(|v| **v <= 2.0).count() as f64 / e.len() as f64;
        format!("{} {}%", nf(dsp::nanmean(e), 5, 2), nf(100.0 * within, 4, 0))
    };
    let _ = writeln!(out, "rate tracking vs taps (MAE BPM, % within 2 BPM)");
    let _ = writeln!(out, "  {:17} {:>11}   {:>19}", "", "vs each tap", "vs taps median of 3");
    for (k, key) in keys.iter().enumerate() {
        let raw = &errs[0].1[k];
        let _ = writeln!(
            out,
            "  {:17} {:>11}   {:>19}   (n={})",
            key,
            fmt(raw),
            fmt(&errs[1].1[k]),
            raw.iter().filter(|v| !v.is_nan()).count()
        );
    }
    let pb = &errs[0].1[0];
    let _ = writeln!(
        out,
        "  taps covered by a usable CSI breath: {:.0}%",
        100.0 * pb.iter().filter(|v| !v.is_nan()).count() as f64 / rr.len().max(1) as f64
    );
    let mk_ev: Vec<f64> = usable.iter().map(|&i| br.marker(mk)[i]).collect();
    let shifted: Vec<f64> = taps.iter().map(|t| t + lag).collect();
    let jitter_ms = match_events(&shifted, &mk_ev, tol).iter().map(|(tp, e)| (e - tp) * 1000.0).collect();
    let usable_pct = 100.0 * br.q.iter().filter(|&&v| v == 0).count() as f64 / br.q.len() as f64;
    let tf: Vec<f64> = usable.iter().map(|&i| br.ti_frac[i]).collect();
    let f3 = |v: f64| nf(v, 0, 3);
    let row = vec![
        ("taps".into(), taps.len().to_string()),
        ("tap_bpm".into(), f3(60.0 / dsp::mean(&ibu))),
        ("onset_marker".into(), names(mk).to_string()),
        ("lag".into(), f3(lag)),
        ("recall".into(), f3(rec_)),
        ("precision".into(), f3(prec)),
        ("jitter_ms".into(), f3(1000.0 * jit)),
        ("sign_agrees".into(), (blind_ok as u8).to_string()),
        ("usable_pct".into(), f3(usable_pct)),
        ("per_breath_mae".into(), f3(dsp::nanmean(&errs[0].1[0]))),
        ("median3_mae".into(), f3(dsp::nanmean(&errs[0].1[1]))),
        ("welch_mae".into(), f3(dsp::nanmean(&errs[0].1[2]))),
        ("fused_mae".into(), f3(dsp::nanmean(&errs[0].1[3]))),
        ("per_mae_vs_tap3".into(), f3(dsp::nanmean(&errs[1].1[0]))),
        ("median_mae_vs_tap3".into(), f3(dsp::nanmean(&errs[1].1[1]))),
        ("welch_mae_vs_tap3".into(), f3(dsp::nanmean(&errs[1].1[2]))),
        ("fused_mae_vs_tap3".into(), f3(dsp::nanmean(&errs[1].1[3]))),
        ("ti_frac".into(), f3(if tf.is_empty() { f64::NAN } else { dsp::nanmedian(&tf) })),
        ("trough_lag".into(), f3(tr_lag)),
        ("peak_lag".into(), f3(pk_lag)),
        ("shape_snr".into(), f3(snr)),
    ];
    Ok(TapReport { text: out, row, breaths: br, taps, lag, seg_t: lag_t, segs, avg, jitter_ms, rt, rr, rr3 })
}
