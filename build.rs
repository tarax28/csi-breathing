//! Embed the TX/RX firmware (ESP-IDF build outputs) so the executable can flash without files.
use std::path::Path;

fn main() {
    let out = std::env::var("OUT_DIR").unwrap();
    let man = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // Local ESP-IDF builds next to the app win; `prebuilt/` (see sync-firmware.sh) is the copy
    // kept in the repo for builds without them (CI).
    let local = Path::new(&man).join("..").join("firmware");
    let fw = if local.join("csi_recv/build/csi_recv.bin").exists() { local } else { Path::new(&man).join("prebuilt") };
    let mut code = String::new();
    for (name, app) in [("TX", "csi_send"), ("RX", "csi_recv")] {
        let b = fw.join(app).join("build");
        let files = [b.join("bootloader/bootloader.bin"), b.join("partition_table/partition-table.bin"), b.join(format!("{app}.bin"))];
        let ok = files.iter().all(|f| f.exists());
        let item = |f: &Path| if ok { format!("include_bytes!({:?})", f.canonicalize().unwrap()) } else { "&[]".into() };
        // git does not keep file times, so the prebuilt copy carries the build time in `built`
        let when = std::fs::read_to_string(fw.join(app).join("built")).ok().and_then(|s| s.trim().parse::<u64>().ok())
            .or_else(|| files[2].metadata().and_then(|m| m.modified()).ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()))
            .unwrap_or(0);
        code += &format!(
            "pub const FW_{name}: Firmware = Firmware {{ app: {app:?}, bootloader: {}, partitions: {}, image: {}, built: {when} }};\n",
            item(&files[0]), item(&files[1]), item(&files[2]));
        for f in &files {
            println!("cargo:rerun-if-changed={}", f.display());
        }
    }
    for (k, file) in [("CHANNEL", "csi_send/sdkconfig"), ("COUNTRY", "csi_send/sdkconfig")] {
        let v = std::fs::read_to_string(fw.join(file)).ok()
            .and_then(|s| s.lines().find(|l| l.starts_with(&format!("CONFIG_CSI_{k}="))).map(|l| l.split('=').nth(1).unwrap().trim_matches('"').to_string()))
            .unwrap_or_default();
        code += &format!("pub const FW_{k}: &str = {v:?};\n");
    }
    std::fs::write(Path::new(&out).join("firmware.rs"), code).unwrap();
}
