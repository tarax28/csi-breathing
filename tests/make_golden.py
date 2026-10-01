"""Reference outputs from numpy/scipy for the Rust DSP port (tests/dsp_golden.rs)."""
import json, numpy as np
from scipy import signal
rng = np.random.default_rng(42)
FS = 20.0
x = np.cumsum(rng.normal(size=3000)) * 0.1 + np.sin(2 * np.pi * 0.25 * np.arange(3000) / FS) + rng.normal(size=3000) * 0.3
out = {"x": x.tolist()}
out["butter"] = []
for order, lo, hi in [(2, 0.1, 0.8), (3, 0.1, 0.6), (3, 0.8, 2.0), (2, 0.1, 0.3), (2, 0.1, 0.65), (3, 1.0, 4.0)]:
    sos = signal.butter(order, [lo, hi], btype="band", fs=FS, output="sos")
    y = signal.sosfiltfilt(sos, x)
    out["butter"].append({"order": order, "lo": lo, "hi": hi, "sos": sos.tolist(), "filtfilt": y.tolist()})
f, P = signal.welch(x[:400], FS, nperseg=400, nfft=4096)
out["welch1"] = {"f": f.tolist(), "P": P.tolist()}
f, P = signal.welch(x[:2000], FS, nperseg=600)
out["welch2"] = {"f": f.tolist(), "P": P.tolist()}
f, P = signal.welch(x[:1100], FS, nperseg=600, nfft=2048)
out["welch3"] = {"f": f.tolist(), "P": P.tolist()}
f, ts, S = signal.spectrogram(x, FS, nperseg=1200, noverlap=900, nfft=8192)
out["spec"] = {"f_len": len(f), "t": ts.tolist(), "S_cols": [S[:420, j].tolist() for j in range(S.shape[1])]}
xs = signal.sosfiltfilt(signal.butter(2, [0.1, 0.8], btype="band", fs=FS, output="sos"), x)
xn = xs / np.abs(xs).std()
xn[100:105] = xn[100]   # a plateau
pk, _ = signal.find_peaks(xn, distance=20, prominence=0.3)
tr, _ = signal.find_peaks(-xn, distance=20, prominence=0.3)
pk2, _ = signal.find_peaks(xn, distance=16)
out["peaks"] = {"x": xn.tolist(), "pk": pk.tolist(), "tr": tr.tolist(), "pk2": pk2.tolist()}
a = rng.normal(size=101)
out["pct"] = {"a": a.tolist(), "p": [0, 10, 25, 50, 75, 90, 99.5, 100],
              "v": [float(np.percentile(a, p)) for p in [0, 10, 25, 50, 75, 90, 99.5, 100]],
              "med_even": float(np.median(a[:100])), "med_odd": float(np.median(a))}
out["conv"] = {"n160": np.convolve(x[:1000], np.ones(160) / 160, "same").tolist(),
               "n81": np.convolve(x[:1000], np.ones(81), "same").tolist(),
               "n8": np.convolve(x[:1000], np.ones(8) / 8, "same").tolist()}
ph = np.cumsum(rng.normal(size=500) * 2.0)
out["unwrap"] = {"in": np.angle(np.exp(1j * ph)).tolist(), "out": np.unwrap(np.angle(np.exp(1j * ph))).tolist()}
xp = np.sort(rng.uniform(0, 10, 50)); fp = rng.normal(size=50); xq = np.linspace(-1, 11, 200)
out["interp"] = {"xp": xp.tolist(), "fp": fp.tolist(), "x": xq.tolist(), "y": np.interp(xq, xp, fp).tolist()}
out["gradient"] = np.gradient(x[:50]).tolist()
json.dump(out, open("tests/golden/dsp.json", "w"))
print("ok", len(json.dumps(out)) // 1024, "KB")
