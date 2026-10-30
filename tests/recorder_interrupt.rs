//! The recorder on a pseudo-terminal in place of the RX: frozen with SIGSTOP (what standby does to it,
//! the wall clock runs on while no thread does), then a silent board. Both must be reported, counted as
//! gaps and not as packet loss, and the recording must carry on.
#![cfg(unix)]

use std::ffi::CStr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

fn open_pty() -> (i32, String) {
    unsafe {
        let m = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_NONBLOCK);
        assert!(m >= 0 && libc::grantpt(m) == 0 && libc::unlockpt(m) == 0);
        (m, CStr::from_ptr(libc::ptsname(m)).to_str().unwrap().to_string())
    }
}

fn wait_for(out: &Path, what: &str, count: usize, secs: u64) -> String {
    let t = Instant::now();
    loop {
        let s = std::fs::read_to_string(out).unwrap_or_default();
        if s.matches(what).count() >= count {
            return s;
        }
        assert!(t.elapsed() < Duration::from_secs(secs), "no {count} x {what:?} after {secs} s in:\n{s}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn freeze_and_silence_are_reported_and_survived() {
    let dir = std::env::temp_dir().join(format!("csi-recorder-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("night.out");
    let (master, port) = open_pty();

    // the board: 200 lines/s, seq counts on whether or not anyone is listening
    let silent = Arc::new(AtomicBool::new(false));
    let quit = Arc::new(AtomicBool::new(false));
    let feeder = {
        let (silent, quit) = (silent.clone(), quit.clone());
        std::thread::spawn(move || {
            let mut seq = 0u64;
            while !quit.load(Ordering::SeqCst) {
                seq += 1;
                if !silent.load(Ordering::SeqCst) {
                    let l = format!("CSI_DATA,{seq},x\n");
                    unsafe { libc::write(master, l.as_ptr().cast(), l.len()) };
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    };

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_csi-breathing"))
        .args(["record-night", "--port", &port, "--hours", "1", "--part-min", "10", "--stall-s", "3", "--results"])
        .arg(&dir)
        .stdout(std::fs::File::create(&out).unwrap())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id() as i32;

    wait_for(&out, "DATA FLOWING", 1, 15);
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    std::thread::sleep(Duration::from_secs(12));
    unsafe { libc::kill(pid, libc::SIGCONT) };
    wait_for(&out, "DATA BACK", 1, 10);

    silent.store(true, Ordering::SeqCst);
    wait_for(&out, "DATA STOPPED", 2, 10);
    wait_for(&out, "still no data", 1, 15);
    silent.store(false, Ordering::SeqCst);
    let log = wait_for(&out, "DATA BACK", 2, 15);
    assert!(log.contains("computer slept"), "{log}");

    let night = dir.join(log.lines().next().unwrap().strip_prefix("recording to ").unwrap().split("  (").next().unwrap());
    std::fs::write(night.join("STOP"), b"").unwrap();
    assert!(child.wait().unwrap().success());
    quit.store(true, Ordering::SeqCst);
    feeder.join().unwrap();
    let log = std::fs::read_to_string(&out).unwrap();
    assert!(log.contains("2 interruptions"), "{log}");

    let j: serde_json::Value = serde_json::from_slice(&std::fs::read(night.join("night.json")).unwrap()).unwrap();
    let gaps = j["gaps"].as_array().unwrap();
    assert_eq!(gaps.len(), 2, "{j}");
    assert_eq!(gaps[0]["why"], "computer asleep");
    assert!(gaps[0]["s"].as_f64().unwrap() >= 11.0, "{j}");
    assert_eq!(gaps[1]["why"], "no data");
    assert!(gaps[1]["s"].as_f64().unwrap() >= 3.0, "{j}");
    assert_eq!(j["lost"], 0, "the frames that went past during a gap are not packet loss: {j}");
    // each interruption started a new part
    assert_eq!(j["parts"].as_array().unwrap().len(), 3, "{j}");
    assert!(j["frames"].as_u64().unwrap() > 100, "{j}");

    let _ = std::fs::remove_dir_all(&dir);
}
