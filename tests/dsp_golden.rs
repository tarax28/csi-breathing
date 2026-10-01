use csi_breathing::dsp;
use serde_json::Value;

fn v(j: &Value) -> Vec<f64> {
    j.as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect()
}
fn vu(j: &Value) -> Vec<usize> {
    j.as_array().unwrap().iter().map(|x| x.as_u64().unwrap() as usize).collect()
}
fn maxrel(a: &[f64], b: &[f64]) -> f64 {
    assert_eq!(a.len(), b.len(), "length");
    let scale = b.iter().fold(0.0f64, |m, x| m.max(x.abs())).max(1e-300);
    a.iter().zip(b).fold(0.0f64, |m, (x, y)| m.max((x - y).abs())) / scale
}

#[test]
fn golden() {
    let g: Value = serde_json::from_str(&std::fs::read_to_string("tests/golden/dsp.json").unwrap()).unwrap();
    let x = v(&g["x"]);
    for b in g["butter"].as_array().unwrap() {
        let (o, lo, hi) = (b["order"].as_u64().unwrap() as usize, b["lo"].as_f64().unwrap(), b["hi"].as_f64().unwrap());
        let sos = dsp::butter_bandpass(o, lo, hi, 20.0);
        let y = dsp::sosfiltfilt(&sos, &x);
        let e = maxrel(&y, &v(&b["filtfilt"]));
        println!("butter {o} [{lo},{hi}] filtfilt rel err {e:.2e}");
        assert!(e < 1e-9, "filtfilt mismatch {e}");
    }
    for (k, nper, nfft, len) in [("welch1", 400, Some(4096), 400), ("welch2", 600, None, 2000), ("welch3", 600, Some(2048), 1100)] {
        let (f, p) = dsp::welch(&x[..len], 20.0, nper, nfft);
        let e = maxrel(&p, &v(&g[k]["P"]));
        println!("{k} rel err {e:.2e}");
        assert!(maxrel(&f, &v(&g[k]["f"])) < 1e-12 && e < 1e-10);
    }
    let (_f, t, s) = dsp::spectrogram(&x, 20.0, 1200, 900, 8192);
    assert!(maxrel(&t, &v(&g["spec"]["t"])) < 1e-12);
    for (j, col) in g["spec"]["S_cols"].as_array().unwrap().iter().enumerate() {
        let e = maxrel(&s[j][..420], &v(col));
        assert!(e < 1e-10, "spectrogram col {j}: {e}");
    }
    println!("spectrogram ok ({} cols)", s.len());
    let xp = v(&g["peaks"]["x"]);
    assert_eq!(dsp::find_peaks(&xp, Some(20), Some(0.3)), vu(&g["peaks"]["pk"]));
    let neg: Vec<f64> = xp.iter().map(|a| -a).collect();
    assert_eq!(dsp::find_peaks(&neg, Some(20), Some(0.3)), vu(&g["peaks"]["tr"]));
    assert_eq!(dsp::find_peaks(&xp, Some(16), None), vu(&g["peaks"]["pk2"]));
    println!("find_peaks ok");
    let a = v(&g["pct"]["a"]);
    for (p, want) in v(&g["pct"]["p"]).iter().zip(v(&g["pct"]["v"])) {
        assert!((dsp::percentile(&a, *p) - want).abs() < 1e-14, "percentile {p}");
    }
    assert_eq!(dsp::median(&a[..100]), g["pct"]["med_even"].as_f64().unwrap());
    assert_eq!(dsp::median(&a), g["pct"]["med_odd"].as_f64().unwrap());
    for (k, n, w) in [("n160", 160, 1.0 / 160.0), ("n81", 81, 1.0), ("n8", 8, 1.0 / 8.0)] {
        let e = maxrel(&dsp::convolve_box_same(&x[..1000], n, w), &v(&g["conv"][k]));
        println!("convolve {k} rel err {e:.2e}");
        assert!(e < 1e-12);
    }
    assert!(maxrel(&dsp::unwrap(&v(&g["unwrap"]["in"])), &v(&g["unwrap"]["out"])) < 1e-12);
    let y = dsp::interp(&v(&g["interp"]["x"]), &v(&g["interp"]["xp"]), &v(&g["interp"]["fp"]));
    assert!(maxrel(&y, &v(&g["interp"]["y"])) < 1e-14);
    assert!(maxrel(&dsp::gradient(&x[..50]), &v(&g["gradient"])) < 1e-14);
    println!("all DSP golden checks passed");
}
