//! The compact CSI line (base64 raw int8 + float32 gain bits) reads back as the old decimal line.
use base64::Engine as _;
use csi_breathing::analysis;

#[test]
fn compact_line_equals_decimal_line() {
    let head = "CSI_DATA,7,1a:00:00:00:00:00,-42,11,-98,16,31,157,8228327,47,2";
    let mut old = String::new();
    let mut new = String::new();
    for (k, g) in [1.0f32, 32.5, 0.707_945_8, 3.981_071_7].iter().enumerate() {
        for n in [234usize, 232, 230, 228] {
            let raw: Vec<i8> = (0..n).map(|i| ((i * 37 + k * 11) % 256) as u8 as i8).collect();
            let vals: Vec<String> = raw.iter().map(|&b| ((g * b as f32) as i16).to_string()).collect();
            let bytes: Vec<u8> = raw.iter().map(|&b| b as u8).collect();
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            // one length in the recordings: the modal row length would otherwise be a tie
            if n == 234 {
                old += &format!("{head},{n},0,\"[{}]\"\n", vals.join(","));
                new += &format!("{head},{n},0,\"{b64}\",{:08x}\n", g.to_bits());
            }
            assert_eq!(analysis::csi_values(&format!("[{}]", vals.join(",")), ""), analysis::csi_values(&b64, &format!(",{:08x}", g.to_bits())));
        }
    }
    // damaged: bad base64, missing gain, gain cut short
    assert!(analysis::csi_values("AAA*", ",42020000").is_none());
    assert!(analysis::csi_values("AAAA", "").is_none());
    assert!(analysis::csi_values("AAAA", ",4202").is_none());
    let (a, b) = (analysis::parse_bytes(old.as_bytes()).unwrap(), analysis::parse_bytes(new.as_bytes()).unwrap());
    assert_eq!((a.nsc, a.bad, &a.seq), (b.nsc, b.bad, &b.seq));
    assert_eq!(a.h, b.h);
}
