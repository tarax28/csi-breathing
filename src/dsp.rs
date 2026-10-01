//! Signal-processing building blocks, written to reproduce numpy/scipy exactly (same
//! algorithms, same edge handling), so results match the validated Python pipeline.
//! Checked against scipy output in tests/dsp_golden.rs.

use rustfft::num_complex::Complex64 as C64;
use rustfft::FftPlanner;
use std::f64::consts::PI;

/// Second-order sections [b0, b1, b2, a0, a1, a2], like scipy's `output="sos"`.
pub type Sos = Vec<[f64; 6]>;

/// scipy.signal.butter(order, [lo, hi], btype="band", fs=fs, output="sos").
/// Pairing of poles/zeros into sections differs from scipy's zpk2sos, which changes nothing but
/// rounding (the overall filter is the same); the section count, and so sosfiltfilt's padding,
/// is identical.
pub fn butter_bandpass(order: usize, lo: f64, hi: f64, fs: f64) -> Sos {
    let n = order as i32;
    // analog prototype poles
    let p: Vec<C64> = (0..order)
        .map(|i| {
            let m = (-n + 1 + 2 * i as i32) as f64;
            -(C64::new(0.0, PI * m / (2.0 * n as f64))).exp()
        })
        .collect();
    // pre-warp (scipy normalises to fs=2)
    let wn = [2.0 * lo / fs, 2.0 * hi / fs];
    let fs2 = 2.0;
    let warped = [2.0 * fs2 * (PI * wn[0] / fs2).tan(), 2.0 * fs2 * (PI * wn[1] / fs2).tan()];
    let bw = warped[1] - warped[0];
    let wo = (warped[0] * warped[1]).sqrt();
    // lp2bp_zpk
    let mut pb = Vec::new();
    for &pl in &p {
        let pl = pl * (bw / 2.0);
        let s = (pl * pl - wo * wo).sqrt();
        pb.push(pl + s);
    }
    for &pl in &p {
        let pl = pl * (bw / 2.0);
        let s = (pl * pl - wo * wo).sqrt();
        pb.push(pl - s);
    }
    let k_bp = bw.powi(n);
    // bilinear_zpk: zeros at 0 (n of them) -> +1, n zeros at infinity -> -1
    let fs2x = 2.0 * fs2;
    let pz: Vec<C64> = pb.iter().map(|&q| (fs2x + q) / (fs2x - q)).collect();
    let mut num = C64::new(1.0, 0.0);
    for _ in 0..order {
        num *= C64::new(fs2x, 0.0);
    }
    let mut den = C64::new(1.0, 0.0);
    for &q in &pb {
        den *= fs2x - q;
    }
    let k_z = k_bp * (num / den).re;
    // sections: each conjugate pole pair, and the real poles two by two (a wide band-pass of odd
    // order has one real pair); every section gets one zero at +1 and one at -1
    let mut dens: Vec<(f64, f64, f64)> = Vec::new(); // (a1, a2, distance of poles from unit circle)
    for q in pz.iter().filter(|q| q.im > 1e-12) {
        dens.push((-2.0 * q.re, q.norm_sqr(), (1.0 - q.norm()).abs()));
    }
    let mut reals: Vec<f64> = pz.iter().filter(|q| q.im.abs() <= 1e-12).map(|q| q.re).collect();
    reals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    for pair in reals.chunks(2) {
        assert_eq!(pair.len(), 2, "odd number of real poles");
        let d = (1.0 - pair[0].abs()).abs().min((1.0 - pair[1].abs()).abs());
        dens.push((-(pair[0] + pair[1]), pair[0] * pair[1], d));
    }
    assert_eq!(dens.len(), order, "expected {order} sections");
    // order like scipy: poles closest to the unit circle last
    dens.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
    dens.iter()
        .enumerate()
        .map(|(i, &(a1, a2, _))| {
            let g = if i == 0 { k_z } else { 1.0 };
            [g, 0.0, -g, 1.0, a1, a2]
        })
        .collect()
}

fn lfilter_zi2(s: &[f64; 6]) -> [f64; 2] {
    let (b0, b1, b2) = (s[0] / s[3], s[1] / s[3], s[2] / s[3]);
    let (a1, a2) = (s[4] / s[3], s[5] / s[3]);
    // (I - companion(a).T) zi = b[1:] - a[1:] * b0
    let m = [[1.0 + a1, -1.0], [a2, 1.0]];
    let r = [b1 - a1 * b0, b2 - a2 * b0];
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    [(r[0] * m[1][1] - m[0][1] * r[1]) / det, (m[0][0] * r[1] - m[1][0] * r[0]) / det]
}

fn sosfilt_zi(sos: &Sos) -> Vec<[f64; 2]> {
    let mut scale = 1.0;
    sos.iter()
        .map(|s| {
            let z = lfilter_zi2(s);
            let out = [scale * z[0], scale * z[1]];
            scale *= (s[0] + s[1] + s[2]) / (s[3] + s[4] + s[5]);
            out
        })
        .collect()
}

fn sosfilt_inplace(sos: &Sos, x: &mut [f64], zi: &mut [[f64; 2]]) {
    for (s, z) in sos.iter().zip(zi.iter_mut()) {
        let (b0, b1, b2, a1, a2) = (s[0], s[1], s[2], s[4], s[5]);
        let (mut z0, mut z1) = (z[0], z[1]);
        for v in x.iter_mut() {
            let xi = *v;
            let y = b0 * xi + z0;
            z0 = b1 * xi - a1 * y + z1;
            z1 = b2 * xi - a2 * y;
            *v = y;
        }
        z[0] = z0;
        z[1] = z1;
    }
}

/// scipy.signal.sosfiltfilt(sos, x) with the default odd padding.
pub fn sosfiltfilt(sos: &Sos, x: &[f64]) -> Vec<f64> {
    let n = x.len();
    let zeros_b = sos.iter().filter(|s| s[2] == 0.0).count();
    let zeros_a = sos.iter().filter(|s| s[5] == 0.0).count();
    let padlen = 3 * (2 * sos.len() + 1 - zeros_b.min(zeros_a));
    let edge = padlen.min(n.saturating_sub(1));
    let mut ext = Vec::with_capacity(n + 2 * edge);
    for i in (1..=edge).rev() {
        ext.push(2.0 * x[0] - x[i]);
    }
    ext.extend_from_slice(x);
    for i in 1..=edge {
        ext.push(2.0 * x[n - 1] - x[n - 1 - i]);
    }
    let zi = sosfilt_zi(sos);
    let x0 = ext[0];
    let mut z: Vec<[f64; 2]> = zi.iter().map(|z| [z[0] * x0, z[1] * x0]).collect();
    sosfilt_inplace(sos, &mut ext, &mut z);
    ext.reverse();
    let y0 = ext[0];
    let mut z: Vec<[f64; 2]> = zi.iter().map(|z| [z[0] * y0, z[1] * y0]).collect();
    sosfilt_inplace(sos, &mut ext, &mut z);
    ext.reverse();
    ext[edge..edge + n].to_vec()
}

/// Periodic Hann window (scipy get_window("hann", n)).
pub fn hann(n: usize) -> Vec<f64> {
    (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()).collect()
}

/// Periodic Tukey window (scipy get_window(("tukey", alpha), n)).
pub fn tukey(n: usize, alpha: f64) -> Vec<f64> {
    let m = n + 1; // periodic: symmetric of n+1, drop the last
    let width = (alpha * (m as f64 - 1.0) / 2.0).floor() as usize;
    let mut w = Vec::with_capacity(m);
    for i in 0..m {
        let v = if i <= width {
            0.5 * (1.0 + (PI * (-1.0 + 2.0 * i as f64 / alpha / (m as f64 - 1.0))).cos())
        } else if i < m - width - 1 {
            1.0
        } else {
            0.5 * (1.0 + (PI * (-2.0 / alpha + 1.0 + 2.0 * i as f64 / alpha / (m as f64 - 1.0))).cos())
        };
        w.push(v);
    }
    w.truncate(n);
    w
}

/// Reusable FFT power-spectrum engine for one (window, nfft) combination.
pub struct Psd {
    pub nperseg: usize,
    pub nfft: usize,
    win: Vec<f64>,
    scale: f64,
    fft: std::sync::Arc<dyn rustfft::Fft<f64>>,
    buf: Vec<C64>,
}

impl Psd {
    pub fn new(win: Vec<f64>, nfft: usize, fs: f64) -> Self {
        let nperseg = win.len();
        let scale = 1.0 / (fs * win.iter().map(|w| w * w).sum::<f64>());
        let fft = FftPlanner::new().plan_fft_forward(nfft);
        Psd { nperseg, nfft, win, scale, fft, buf: vec![C64::new(0.0, 0.0); nfft] }
    }

    pub fn freqs(&self, fs: f64) -> Vec<f64> {
        (0..self.nfft / 2 + 1).map(|i| i as f64 * fs / self.nfft as f64).collect()
    }

    /// One-sided density periodogram of one segment (mean removed), added into `acc`.
    pub fn add_segment(&mut self, seg: &[f64], acc: &mut [f64]) {
        let mean = seg.iter().sum::<f64>() / seg.len() as f64;
        for c in self.buf.iter_mut() {
            *c = C64::new(0.0, 0.0);
        }
        for (i, &v) in seg.iter().enumerate() {
            self.buf[i] = C64::new((v - mean) * self.win[i], 0.0);
        }
        self.fft.process(&mut self.buf);
        let nb = self.nfft / 2 + 1;
        for k in 0..nb {
            let mut p = self.buf[k].norm_sqr() * self.scale;
            let last_nyq = self.nfft % 2 == 0 && k == nb - 1;
            if k != 0 && !last_nyq {
                p *= 2.0;
            }
            acc[k] += p;
        }
    }

    /// scipy.signal.welch(x, fs, nperseg, nfft) (hann, 50% overlap, constant detrend, mean).
    pub fn welch(&mut self, x: &[f64]) -> Vec<f64> {
        let nb = self.nfft / 2 + 1;
        let mut acc = vec![0.0; nb];
        let step = self.nperseg - self.nperseg / 2;
        let nseg = if x.len() >= self.nperseg { (x.len() - self.nperseg) / step + 1 } else { 0 };
        for s in 0..nseg {
            let seg = &x[s * step..s * step + self.nperseg];
            self.add_segment(seg, &mut acc);
        }
        for v in acc.iter_mut() {
            *v /= nseg.max(1) as f64;
        }
        acc
    }
}

/// scipy.signal.welch(x, fs, nperseg=min(nperseg, len), nfft=nfft or nperseg). Returns (f, P).
pub fn welch(x: &[f64], fs: f64, nperseg: usize, nfft: Option<usize>) -> (Vec<f64>, Vec<f64>) {
    let nper = nperseg.min(x.len());
    let mut p = Psd::new(hann(nper), nfft.unwrap_or(nper).max(nper), fs);
    let f = p.freqs(fs);
    let pw = p.welch(x);
    (f, pw)
}

/// scipy.signal.spectrogram(x, fs, nperseg, noverlap, nfft) (tukey 0.25 window, density).
/// Returns (f, t, S[column][freq]).
pub fn spectrogram(x: &[f64], fs: f64, nperseg: usize, noverlap: usize, nfft: usize) -> (Vec<f64>, Vec<f64>, Vec<Vec<f64>>) {
    let mut p = Psd::new(tukey(nperseg, 0.25), nfft, fs);
    let f = p.freqs(fs);
    let step = nperseg - noverlap;
    let ncol = if x.len() >= nperseg { (x.len() - nperseg) / step + 1 } else { 0 };
    let mut t = Vec::with_capacity(ncol);
    let mut cols = Vec::with_capacity(ncol);
    for c in 0..ncol {
        let mut acc = vec![0.0; nfft / 2 + 1];
        p.add_segment(&x[c * step..c * step + nperseg], &mut acc);
        cols.push(acc);
        t.push((nperseg as f64 / 2.0 + (c * step) as f64) / fs);
    }
    (f, t, cols)
}

/// scipy.signal.find_peaks(x, distance=distance, prominence=prominence) (either optional).
pub fn find_peaks(x: &[f64], distance: Option<usize>, prominence: Option<f64>) -> Vec<usize> {
    let n = x.len();
    // local maxima, flat tops -> middle (scipy _local_maxima_1d)
    let mut peaks = Vec::new();
    if n >= 3 {
        let mut i = 1;
        let i_max = n - 1;
        while i < i_max {
            if x[i - 1] < x[i] {
                let mut ahead = i + 1;
                while ahead < i_max && x[ahead] == x[i] {
                    ahead += 1;
                }
                if x[ahead] < x[i] {
                    peaks.push((i + ahead - 1) / 2);
                    i = ahead;
                }
            }
            i += 1;
        }
    }
    if let Some(d) = distance {
        if d > 1 && peaks.len() > 1 {
            let np_ = peaks.len();
            let mut keep = vec![true; np_];
            // argsort by height (scipy uses argsort on priority; ties are rare on real data)
            let mut order: Vec<usize> = (0..np_).collect();
            order.sort_by(|&a, &b| x[peaks[a]].partial_cmp(&x[peaks[b]]).unwrap());
            for &j in order.iter().rev() {
                if !keep[j] {
                    continue;
                }
                let mut k = j as isize - 1;
                while k >= 0 && peaks[j] - peaks[k as usize] < d {
                    keep[k as usize] = false;
                    k -= 1;
                }
                let mut k = j + 1;
                while k < np_ && peaks[k] - peaks[j] < d {
                    keep[k] = false;
                    k += 1;
                }
            }
            peaks = peaks.into_iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| p).collect();
        }
    }
    if let Some(pmin) = prominence {
        peaks.retain(|&p| {
            let h = x[p];
            let mut left_min = h;
            let mut i = p as isize;
            while i >= 0 && x[i as usize] <= h {
                left_min = left_min.min(x[i as usize]);
                i -= 1;
            }
            let mut right_min = h;
            let mut i = p;
            while i < n && x[i] <= h {
                right_min = right_min.min(x[i]);
                i += 1;
            }
            h - left_min.max(right_min) >= pmin
        });
    }
    peaks
}

/// numpy.percentile (linear), on a copy.
pub fn percentile(a: &[f64], p: f64) -> f64 {
    let mut v: Vec<f64> = a.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    percentile_sorted(&v, p)
}

pub fn percentile_sorted(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let idx = p / 100.0 * (v.len() - 1) as f64;
    let lo = idx.floor() as usize;
    let hi = (lo + 1).min(v.len() - 1);
    let t = idx - lo as f64;
    let (a, b) = (v[lo], v[hi]);
    let d = b - a;
    if t >= 0.5 { b - d * (1.0 - t) } else { a + d * t } // numpy _lerp
}

pub fn median(a: &[f64]) -> f64 {
    if a.is_empty() {
        return f64::NAN;
    }
    let mut v: Vec<f64> = a.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

/// numpy.nanmedian
pub fn nanmedian(a: &[f64]) -> f64 {
    let v: Vec<f64> = a.iter().copied().filter(|x| !x.is_nan()).collect();
    median(&v)
}

pub fn mean(a: &[f64]) -> f64 {
    if a.is_empty() { f64::NAN } else { a.iter().sum::<f64>() / a.len() as f64 }
}

pub fn nanmean(a: &[f64]) -> f64 {
    let v: Vec<f64> = a.iter().copied().filter(|x| !x.is_nan()).collect();
    mean(&v)
}

/// Population standard deviation (numpy std, ddof=0).
pub fn std(a: &[f64]) -> f64 {
    let m = mean(a);
    (a.iter().map(|v| (v - m) * (v - m)).sum::<f64>() / a.len() as f64).sqrt()
}

/// numpy.convolve(x, ones(n) * w, "same") for len(x) >= n.
pub fn convolve_box_same(x: &[f64], n: usize, w: f64) -> Vec<f64> {
    let len = x.len();
    let off = (n - 1) / 2;
    // full[i] = sum_{j=0}^{n-1} x[i-j] ; same = full[off .. off+len]
    let mut pre = vec![0.0; len + 1];
    for i in 0..len {
        pre[i + 1] = pre[i] + x[i];
    }
    (0..len)
        .map(|k| {
            let i = k + off; // index into full
            let hi = i.min(len - 1);
            let lo = i.saturating_sub(n - 1);
            if lo > hi { 0.0 } else { (pre[hi + 1] - pre[lo]) * w }
        })
        .collect()
}

/// numpy.interp(x, xp, fp) for sorted xp.
pub fn interp(x: &[f64], xp: &[f64], fp: &[f64]) -> Vec<f64> {
    x.iter().map(|&v| interp1(v, xp, fp)).collect()
}

pub fn interp1(v: f64, xp: &[f64], fp: &[f64]) -> f64 {
    let n = xp.len();
    if n == 0 {
        return f64::NAN;
    }
    if v.is_nan() {
        return f64::NAN;
    }
    if v <= xp[0] {
        return fp[0];
    }
    if v >= xp[n - 1] {
        return fp[n - 1];
    }
    // last j with xp[j] <= v
    let j = match xp.binary_search_by(|a| a.partial_cmp(&v).unwrap()) {
        Ok(j) => {
            return fp[j];
        }
        Err(i) => i - 1,
    };
    let slope = (fp[j + 1] - fp[j]) / (xp[j + 1] - xp[j]);
    slope * (v - xp[j]) + fp[j]
}

/// numpy.unwrap
pub fn unwrap(p: &[f64]) -> Vec<f64> {
    let mut out = Vec::with_capacity(p.len());
    if p.is_empty() {
        return out;
    }
    out.push(p[0]);
    let mut corr = 0.0;
    for i in 1..p.len() {
        let dd = p[i] - p[i - 1];
        let mut ddmod = (dd + PI).rem_euclid(2.0 * PI) - PI;
        if ddmod == -PI && dd > 0.0 {
            ddmod = PI;
        }
        let mut pc = ddmod - dd;
        if dd.abs() < PI {
            pc = 0.0;
        }
        corr += pc;
        out.push(p[i] + corr);
    }
    out
}

/// numpy.gradient (unit spacing).
pub fn gradient(x: &[f64]) -> Vec<f64> {
    let n = x.len();
    if n < 2 {
        return vec![0.0; n];
    }
    let mut g = vec![0.0; n];
    g[0] = x[1] - x[0];
    g[n - 1] = x[n - 1] - x[n - 2];
    for i in 1..n - 1 {
        g[i] = (x[i + 1] - x[i - 1]) / 2.0;
    }
    g
}

/// Runs of equal non-zero values: (start, end_exclusive, value), like breath_analyze._runs.
pub fn runs<T: Copy + PartialEq + Default>(q: &[T]) -> Vec<(usize, usize, T)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < q.len() {
        if q[i] != T::default() {
            let mut j = i;
            while j < q.len() && q[j] == q[i] {
                j += 1;
            }
            out.push((i, j, q[i]));
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Index of the maximum (first on ties), like numpy.argmax.
pub fn argmax(a: &[f64]) -> usize {
    let mut best = 0;
    for i in 1..a.len() {
        if a[i] > a[best] {
            best = i;
        }
    }
    best
}

pub fn argmin(a: &[f64]) -> usize {
    let mut best = 0;
    for i in 1..a.len() {
        if a[i] < a[best] {
            best = i;
        }
    }
    best
}
