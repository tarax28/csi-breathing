"""Run night_analyze with np.convolve's box filters replaced by exact prefix sums (what the Rust
port does), as py_exact.py does for breath_analyze: checks `csi-breathing night` independently of
numpy's summation order at .5 ties. The worker processes import this file again, so the patch
reaches them too.

  python3 tests/py_exact_night.py <night folder> [night_analyze options]

Then compare night-breaths.csv and night-epochs.csv with the ones `csi-breathing night` writes."""
import sys, numpy as np
sys.path.insert(0, "/home/tarax/Work/esp-csi-sleep/single_txrx")
sys.path.insert(0, "/home/tarax/Work/esp-csi-sleep")
_conv = np.convolve
def conv(a, v, mode="full"):
    if mode != "same" or np.iscomplexobj(v) or np.iscomplexobj(a):
        return _conv(a, v, mode)
    v = np.asarray(v, float)
    if np.all(v == v[0]) and len(a) >= len(v):
        a = np.asarray(a, float); n = len(v)
        pre = np.concatenate([[0.0], np.cumsum(a)])
        off = (n - 1) // 2
        i = np.arange(len(a)) + off
        hi = np.minimum(i, len(a) - 1); lo = np.maximum(i - (n - 1), 0)
        return (pre[hi + 1] - pre[lo]) * v[0]
    return _conv(a, v, mode)
np.convolve = conv
import night_analyze
if __name__ == "__main__":
    night_analyze.main()
