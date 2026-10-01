//! What the Nights page draws, cut down from night::NightData and cached in the night folder
//! (night-view.json.gz) so an analysed night opens instantly.

use crate::analysis::FS;
use crate::night::{self, NightData, REG_MIN};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const FILE: &str = "night-view.json.gz";
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct EpochV {
    pub t: f64,
    pub rate: f64,
    pub cv: f64,
    pub rmssd: f64,
    pub regularity: f64,
    pub motion_pct: f64,
    pub good: bool,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct StageV {
    pub stage: String,
    pub minutes: f64,
    pub regularity: f64,
    pub steady_pct: f64,
    pub breath_bpm: f64,
    pub wearable: Option<(f64, f64)>,
    pub usable_pct: f64,
    pub rmssd: f64,
    pub moves_h: f64,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct NightView {
    pub version: u32,
    pub t0: f64,
    pub dur: f64,
    pub epoch_min: f64,
    /// usable breaths: onset (s) and rate (BPM)
    pub breath_t: Vec<f64>,
    pub breath_bpm: Vec<f64>,
    pub breaths_total: usize,
    pub track_t: Vec<f64>,
    pub track_bpm: Vec<f64>,
    pub reg2: Vec<f64>,
    /// spectrogram, column-major (time x bpm), 0..255, each column scaled to its peak
    pub spec: Vec<u8>,
    pub spec_nt: usize,
    pub spec_bpm: (f64, f64, usize),
    pub spec_dt: f64,
    pub epochs: Vec<EpochV>,
    /// movement events (start s, end s)
    pub moves: Vec<(f64, f64)>,
    pub motion_frac: f64,
    /// wearable stages as runs (start s, end s, stage)
    pub stages: Vec<(f64, f64, String)>,
    pub restless: Vec<f64>,
    pub source: String,
    pub rows: Vec<StageV>,
    pub whole: Option<(f64, f64)>,
    pub deep_minus_rem: Option<(f64, String)>,
    pub restless_hits: Vec<(u32, f64, usize, f64)>,
    pub summary: String,
}

impl NightView {
    pub fn from(n: &NightData) -> Self {
        let b = &n.b;
        let ok: Vec<usize> = (0..b.len()).filter(|&i| b.ok[i]).collect();
        let tr = &n.track;
        let nb = tr.spec_bpm.len();
        let nt = tr.spec.len();
        let mut spec = vec![0u8; nt * nb];
        for (t, col) in tr.spec.iter().enumerate() {
            let mx = col.iter().cloned().fold(0.0, f64::max) + 1e-12;
            for (i, v) in col.iter().enumerate() {
                spec[t * nb + i] = ((v / mx).powf(0.7) * 255.0).round() as u8;
            }
        }
        let dt = if tr.t.len() > 1 { tr.t[1] - tr.t[0] } else { 15.0 };
        let mut stages = vec![];
        if let Some(r) = &n.reference {
            let mut s = 0;
            for i in 1..=r.lab.len() {
                if i == r.lab.len() || r.lab[i] != r.lab[s] {
                    if !r.lab[s].is_empty() {
                        stages.push((s as f64, i as f64, r.lab[s].to_string()));
                    }
                    s = i;
                }
            }
        }
        let c = n.comparison.as_ref();
        NightView {
            version: VERSION,
            t0: n.t0,
            dur: n.q.len() as f64 / FS,
            epoch_min: n.epoch_min,
            breath_t: ok.iter().map(|&i| b.rise[i]).collect(),
            breath_bpm: ok.iter().map(|&i| 60.0 / b.ttot[i]).collect(),
            breaths_total: b.len(),
            track_t: tr.t.clone(),
            track_bpm: tr.bpm.clone(),
            reg2: tr.reg2.clone(),
            spec,
            spec_nt: nt,
            spec_bpm: (tr.spec_bpm.first().copied().unwrap_or(0.0), tr.spec_bpm.last().copied().unwrap_or(0.0), nb),
            spec_dt: dt,
            epochs: n
                .epochs
                .iter()
                .map(|e| EpochV { t: e.t_h * 3600.0, rate: e.rate_bpm, cv: e.cv_pct, rmssd: e.rmssd_s, regularity: e.regularity, motion_pct: e.motion_pct, good: e.good })
                .collect(),
            moves: n.events.iter().map(|e| (e.0 as f64, e.1 as f64)).collect(),
            motion_frac: n.motion_frac,
            stages,
            restless: n.reference.as_ref().map(|r| r.restless.clone()).unwrap_or_default(),
            source: n.reference.as_ref().map(|r| r.source.clone()).unwrap_or_default(),
            rows: c
                .map(|c| {
                    c.rows
                        .iter()
                        .map(|r| StageV {
                            stage: r.stage.into(),
                            minutes: r.minutes,
                            regularity: r.regularity,
                            steady_pct: r.steady_pct,
                            breath_bpm: r.breath_bpm,
                            wearable: r.wearable,
                            usable_pct: r.usable_pct,
                            rmssd: r.rmssd,
                            moves_h: r.moves_h,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            whole: c.and_then(|c| c.whole),
            deep_minus_rem: c.and_then(|c| c.deep_minus_rem.clone()),
            restless_hits: c.map(|c| c.restless_hits.clone()).unwrap_or_default(),
            summary: n.summary.clone(),
        }
    }

    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        let f = std::fs::File::create(dir.join(FILE))?;
        let mut gz = GzEncoder::new(std::io::BufWriter::new(f), Compression::new(6));
        serde_json::to_writer(&mut gz, self)?;
        gz.finish()?;
        Ok(())
    }

    pub fn load(dir: &Path) -> Option<Self> {
        let f = std::fs::File::open(dir.join(FILE)).ok()?;
        let v: NightView = serde_json::from_reader(std::io::BufReader::new(GzDecoder::new(f))).ok()?;
        (v.version == VERSION).then_some(v)
    }

    pub fn steady_pct(&self) -> f64 {
        100.0 * self.reg2.iter().filter(|&&r| r >= REG_MIN).count() as f64 / self.reg2.len().max(1) as f64
    }

    pub fn median_bpm(&self) -> f64 {
        if self.breath_bpm.is_empty() { f64::NAN } else { crate::dsp::median(&self.breath_bpm) }
    }

    pub fn span_text(&self) -> String {
        format!(
            "{} to {}",
            night::clock(self.t0, "%a %-d %b, %H:%M"),
            night::clock(self.t0 + self.dur, if self.dur > 20.0 * 3600.0 { "%a %H:%M" } else { "%H:%M" })
        )
    }
}
