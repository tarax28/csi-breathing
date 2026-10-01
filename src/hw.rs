//! Boards: find them by MAC, check the link, capture CSI, flash firmware (embedded in the binary).

use anyhow::{Result, anyhow, bail};
use std::fmt::Write as _;
use std::io::Read;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

include!(concat!(env!("OUT_DIR"), "/firmware.rs"));

pub const ESPRESSIF_VID: u16 = 0x303A;
pub const DEFAULT_TX_MAC: &str = "10:BD:A3:CF:26:FC";
pub const DEFAULT_RX_MAC: &str = "38:44:BE:BA:15:A8";

pub struct Firmware {
    pub app: &'static str,
    pub bootloader: &'static [u8],
    pub partitions: &'static [u8],
    pub image: &'static [u8],
    /// unix time the image was built (0 = not embedded)
    pub built: u64,
}
impl Firmware {
    pub fn embedded(&self) -> bool {
        !self.image.is_empty()
    }
}

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64()
}

pub fn norm_mac(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_hexdigit()).map(|c| c.to_ascii_uppercase()).collect()
}

#[derive(Clone, Debug)]
pub struct PortInfo {
    pub name: String,
    pub serial: String,
    pub product: String,
    pub vid: u16,
    pub pid: u16,
}

pub fn list_ports() -> Vec<PortInfo> {
    let mut v: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        // macOS lists each board twice (/dev/cu.* and /dev/tty.*); keep the call-out device
        .filter(|p| !p.port_name.starts_with("/dev/tty."))
        .filter_map(|p| match p.port_type {
            serialport::SerialPortType::UsbPort(u) => Some(PortInfo {
                name: p.port_name,
                serial: u.serial_number.unwrap_or_default(),
                product: u.product.unwrap_or_default(),
                vid: u.vid,
                pid: u.pid,
            }),
            _ => None,
        })
        .collect();
    v.sort_by(|a, b| ((a.vid != ESPRESSIF_VID), &a.name).cmp(&((b.vid != ESPRESSIF_VID), &b.name)));
    v
}

/// The port whose USB serial number is this MAC (Linux and Windows both report it).
pub fn port_for_mac(mac: &str) -> Option<PortInfo> {
    let m = norm_mac(mac);
    if m.is_empty() {
        return None;
    }
    list_ports().into_iter().find(|p| norm_mac(&p.serial) == m)
}

pub fn open(port: &str, timeout_ms: u64) -> Result<Box<dyn serialport::SerialPort>> {
    let p = serialport::new(port, 115_200).timeout(Duration::from_millis(timeout_ms)).open()?;
    let _ = p.clear(serialport::ClearBuffer::Input);
    Ok(p)
}

/// The flash_tx_rx.sh verify check: rate, loss, gain lock, frozen CSI buffer.
pub fn verify_link(port: &str, secs: f64) -> Result<(bool, String)> {
    let mut s = open(port, 200)?;
    let t0 = Instant::now();
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 65536];
    while t0.elapsed().as_secs_f64() < secs {
        match s.read(&mut chunk) {
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e.into()),
        }
    }
    let el = t0.elapsed().as_secs_f64();
    let (mut seqs, mut payloads) = (vec![], vec![]);
    let (mut rssi, mut noise, mut agc, mut fft, mut chs) = (vec![], vec![], vec![], vec![], vec![]);
    let (mut other, mut fwl, mut nsc) = (0, vec![], 0);
    for line in buf.split(|&b| b == b'\n') {
        let l = String::from_utf8_lossy(line);
        let l = l.trim_end();
        if l.starts_with("CSI_DATA") {
            let mut it = l.splitn(2, '"');
            let head: Vec<&str> = it.next().unwrap_or("").trim_end_matches(',').split(',').collect();
            let data = it.next().unwrap_or("");
            if head.len() >= 11 {
                let g = |i: usize| head[i].trim().parse::<i64>().ok();
                if let (Some(sq), Some(r), Some(nf), Some(ff), Some(ag), Some(ch)) = (g(1), g(3), g(5), g(6), g(7), g(8)) {
                    seqs.push(sq);
                    rssi.push(r);
                    noise.push(nf);
                    fft.push(ff);
                    agc.push(ag);
                    chs.push(ch);
                    payloads.push(data.to_string());
                    if nsc == 0 {
                        nsc = data.trim_matches(|c| c == '[' || c == ']' || c == '"').split(',').count() / 2;
                    }
                    continue;
                }
            }
            other += 1;
        } else if l.contains("gain locked") || l.contains("compensate_gain") {
            fwl.push(l.to_string());
        } else if !l.is_empty() {
            other += 1;
        }
    }
    let set = |v: &Vec<i64>| {
        let mut s = v.clone();
        s.sort();
        s.dedup();
        s
    };
    let gaps = seqs.windows(2).filter(|w| w[1] - w[0] != 1).count();
    let dupes = seqs.len() - set(&seqs).len();
    let repeats = payloads.windows(2).filter(|w| w[0] == w[1]).count();
    let rate = seqs.len() as f64 / el;
    let mut o = String::new();
    let _ = writeln!(o, "port {port}, {secs:.0} s");
    for l in &fwl {
        let _ = writeln!(o, "  fw: {l}");
    }
    let _ = writeln!(o, "  packets      : {} -> {:.1} pkt/s (target 100)", seqs.len(), rate);
    let _ = writeln!(o, "  seq gaps     : {gaps}   duplicates: {dupes}");
    let _ = writeln!(o, "  channel      : {:?}", set(&chs));
    let _ = writeln!(o, "  RSSI dBm     : {:?}   noise floor: {:?}", set(&rssi), set(&noise));
    let _ = writeln!(o, "  agc/fft gain : {:?} / {:?}", set(&agc), set(&fft));
    let _ = writeln!(o, "  subcarriers  : {nsc}   other lines: {other}");
    if !payloads.is_empty() {
        let mut p = payloads.clone();
        p.sort();
        p.dedup();
        let _ = writeln!(o, "  CSI payloads : {} distinct of {}, {} repeated", p.len(), payloads.len(), repeats);
    }
    let mut ok = true;
    if !payloads.is_empty() && repeats as f64 > 0.05 * payloads.len() as f64 {
        o += "  FAIL: CSI buffer looks frozen\n";
        ok = false;
    }
    if seqs.is_empty() {
        o += "  FAIL: no CSI data (is the TX powered and in range?)\n";
        ok = false;
    }
    if gaps > 0 || dupes > 0 {
        o += "  WARN: sequence loss/duplicates\n";
    }
    if set(&agc).len() > 1 || set(&fft).len() > 1 {
        o += "  WARN: gain changed during the check (not locked yet?)\n";
    }
    if !seqs.is_empty() && !(95.0..=105.0).contains(&rate) {
        o += "  FAIL: rate outside 95-105 Hz\n";
        ok = false;
    }
    let _ = writeln!(o, "  RESULT: {}", if ok { "PASS" } else { "FAIL" });
    Ok((ok, o))
}

/// Minimal stats from a quick look at the stream (for the live status pill).
pub struct LinkStats {
    pub rate: f64,
    pub rssi: f64,
}

/// Where CSI lines come from: the RX, or a recorded log replayed at 100 lines/s.
pub enum Source {
    Serial(String),
    Replay(String),
}

/// Capture into `path` for `secs`; calls `progress(packets, elapsed, mean RSSI)` and
/// `on_line(line, host time)` for every CSI line; `stop()` aborts.
/// Returns (seq, host time) of every line, like dashboard.capture_thread.
pub fn capture(
    src: &Source,
    secs: f64,
    path: &std::path::Path,
    started: &mut dyn FnMut(f64),
    progress: &mut dyn FnMut(usize, f64, f64),
    on_line: &mut dyn FnMut(&[u8], f64),
    stop: &dyn Fn() -> bool,
) -> Result<(Vec<i64>, Vec<f64>, f64)> {
    use std::io::Write;
    enum R {
        S(Box<dyn serialport::SerialPort>),
        P(Vec<Vec<u8>>, usize),
    }
    let mut rd = match src {
        Source::Serial(p) => R::S(open(p, 200)?),
        Source::Replay(f) => {
            let data = std::fs::read(f)?;
            let lines: Vec<Vec<u8>> = data.split(|&b| b == b'\n').filter(|l| l.starts_with(b"CSI_DATA")).map(|l| l.to_vec()).collect();
            if lines.is_empty() {
                bail!("no CSI lines in {f}");
            }
            R::P(lines, 0)
        }
    };
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let t0 = now();
    started(t0);
    let (mut seqs, mut host) = (vec![], vec![]);
    let mut buf: Vec<u8> = vec![];
    let mut chunk = vec![0u8; 65536];
    let mut last = 0.0;
    let mut rssi_sum = 0.0;
    loop {
        let el = now() - t0;
        if el >= secs || stop() {
            break;
        }
        match &mut rd {
            R::S(s) => match s.read(&mut chunk) {
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(e) => return Err(anyhow!("serial read failed: {e}")),
            },
            R::P(lines, pos) => {
                std::thread::sleep(Duration::from_millis(50));
                let k = lines.len().min((el * 100.0) as usize);
                for l in &lines[*pos..k] {
                    buf.extend_from_slice(l);
                    buf.push(b'\n');
                }
                *pos = k;
                if k >= lines.len() {
                    break;
                }
            }
        }
        let now_ = now();
        while let Some(i) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=i).collect();
            let line = &line[..line.len() - 1];
            if line.starts_with(b"CSI_DATA") {
                f.write_all(line)?;
                f.write_all(b"\n")?;
                on_line(line, now_);
                let s = String::from_utf8_lossy(line);
                let mut p = s.splitn(5, ',');
                p.next();
                if let Some(Ok(sq)) = p.next().map(|v| v.parse::<i64>()) {
                    seqs.push(sq);
                    host.push(now_);
                }
                p.next();
                if let Some(Ok(r)) = p.next().map(|v| v.parse::<f64>()) {
                    rssi_sum += r;
                }
            }
        }
        if now_ - last > 0.25 {
            last = now_;
            progress(seqs.len(), now_ - t0, if seqs.is_empty() { f64::NAN } else { rssi_sum / seqs.len() as f64 });
        }
    }
    f.flush()?;
    Ok((seqs, host, now() - t0))
}

/// Flash one board with embedded firmware through espflash (stub loader, verified writes).
pub fn flash(port: &PortInfo, fw: &Firmware, log: &mut dyn FnMut(String)) -> Result<()> {
    use espflash::connection::{Connection, ResetAfterOperation, ResetBeforeOperation};
    use espflash::flasher::Flasher;
    use espflash::image_format::Segment;
    use espflash::target::{Chip, ProgressCallbacks};
    if !fw.embedded() {
        bail!("{} firmware is not embedded in this build", fw.app);
    }
    struct P<'a> {
        log: &'a mut dyn FnMut(String),
        total: usize,
        addr: u32,
        last: usize,
    }
    impl ProgressCallbacks for P<'_> {
        fn init(&mut self, addr: u32, total: usize) {
            self.total = total;
            self.addr = addr;
            self.last = 0;
        }
        fn update(&mut self, current: usize) {
            let pct = 100 * current / self.total.max(1);
            if pct >= self.last + 25 {
                self.last = pct;
                (self.log)(format!("  0x{:05x}: {pct}%", self.addr));
            }
        }
        fn verifying(&mut self) {}
        fn finish(&mut self, skipped: bool) {
            (self.log)(format!("  0x{:05x}: {}", self.addr, if skipped { "unchanged, skipped" } else { "written and verified" }));
        }
    }
    log(format!("connecting to {} ...", port.name));
    let serial = serialport::new(&port.name, 115_200).flow_control(serialport::FlowControl::None).open_native()?;
    let info = serialport::UsbPortInfo {
        vid: port.vid,
        pid: port.pid,
        serial_number: Some(port.serial.clone()),
        manufacturer: None,
        product: Some(port.product.clone()),
    };
    let conn = Connection::new(serial, info, ResetAfterOperation::HardReset, ResetBeforeOperation::DefaultReset, 115_200);
    let mut fl = Flasher::connect(conn, true, true, false, Some(Chip::Esp32c5), None).map_err(|e| anyhow!("connect failed: {e}"))?;
    if let Ok(di) = fl.device_info() {
        log(format!("  chip {:?} rev {:?}, MAC {}", di.chip, di.revision, di.mac_address.unwrap_or_default()));
    }
    let segs = [
        Segment::new(0x2000, fw.bootloader),
        Segment::new(0xa000, fw.partitions),
        Segment::new(0x20000, fw.image),
    ];
    let mut p = P { log, total: 0, addr: 0, last: 0 };
    fl.write_bins_to_flash(&segs, &mut p).map_err(|e| anyhow!("flash failed: {e}"))?;
    fl.connection().reset_after(true, Chip::Esp32c5).map_err(|e| anyhow!("reset failed: {e}"))?;
    (p.log)("  board reset: running the new firmware".into());
    Ok(())
}

/// Free disk space (bytes) where `path` lives.
pub fn free_space(path: &std::path::Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut s) } == 0 {
            return Some(s.f_bavail as u64 * s.f_frsize as u64);
        }
        None
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let w: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut free = 0u64;
        let ok = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(w.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut())
        };
        if ok != 0 { Some(free) } else { None }
    }
}

/// Keep the PC from sleeping while this process runs (Windows; Linux uses systemd-inhibit, macOS caffeinate).
pub fn keep_awake() {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Power::{ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState};
        SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
    }
}

/// Turn the display off (any key/mouse wakes it). Returns false if no method is known.
pub fn screen_off() -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::UI::WindowsAndMessaging::{HWND_BROADCAST, PostMessageW, SC_MONITORPOWER, WM_SYSCOMMAND};
        PostMessageW(HWND_BROADCAST, WM_SYSCOMMAND, SC_MONITORPOWER as usize, 2);
        return true;
    }
    #[cfg(unix)]
    {
        let has = |c: &str| std::process::Command::new("sh").arg("-c").arg(format!("command -v {c}")).output().map(|o| o.status.success()).unwrap_or(false);
        if has("hyprctl") {
            let r = std::process::Command::new("hyprctl").args(["dispatch", "hl.dsp.dpms({ action = \"disable\" })"]).output();
            let bad = r.map(|o| !o.status.success() || String::from_utf8_lossy(&o.stdout).to_lowercase().contains("error")).unwrap_or(true);
            if bad {
                let _ = std::process::Command::new("hyprctl").args(["dispatch", "dpms", "off"]).output();
            }
            return true;
        }
        if has("xset") && std::env::var("DISPLAY").is_ok() {
            let _ = std::process::Command::new("xset").args(["dpms", "force", "off"]).output();
            return true;
        }
        if cfg!(target_os = "macos") {
            return std::process::Command::new("pmset").arg("displaysleepnow").status().map(|s| s.success()).unwrap_or(false);
        }
        false
    }
}
