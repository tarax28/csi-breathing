"""Run breath_analyze with np.convolve replaced by an exact prefix-sum box filter (what the
Rust port does), to check the port independently of OpenBLAS summation order at .5 ties."""
import sys, numpy as np
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
import breath_analyze as ba
sys.argv = ["breath_analyze.py"] + sys.argv[1:]
exec(open("/home/tarax/Work/esp-csi-sleep/breath_analyze.py").read().split('if __name__ == "__main__":')[1].replace("\n    ", "\n"), vars(ba))
