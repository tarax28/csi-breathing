## What's new in v0.3.1

- **Windows: recordings no longer freeze when the screen goes off.** Turning the display off could put a laptop with Modern Standby to sleep a few seconds into the recording, and nothing was recorded until it woke. On Windows, "Turn the screen off" now shows a black full-screen window instead (any key or click brings the app back) and the display stays powered, so lower the brightness for the night. Keep the PC plugged in with the lid open: closing the lid or pressing the power button still puts it to sleep.
- **Interruptions are reported.** If the computer sleeps, the serial port fails or no data arrives, the recorder says so, reopens the port and starts a new part. The Record page shows it, the last line of the output counts it, `night.json` lists each gap, and frames missed during a gap no longer count as lost.
- Windows: the "Keep the computer awake" checkbox is now honoured.

## Downloads

| System | File | How to run |
|---|---|---|
| Windows 10/11 (64-bit) | `csi-breathing-windows-x64.exe` | Double-click. SmartScreen may warn: More info > Run anyway. |
| macOS 11+ (Apple Silicon and Intel) | `csi-breathing-macos-universal.zip` | Unzip and open `CSI Breathing.app`. It is not notarised: allow it under System Settings > Privacy & Security > Open Anyway, or run `xattr -dr com.apple.quarantine "CSI Breathing.app"`. |
| Linux x86-64 (glibc 2.35+) | `csi-breathing-linux-x64.tar.gz` | `tar xzf csi-breathing-linux-x64.tar.gz`, then `./csi-breathing`. |

The TX/RX firmware for the ESP32-C5 boards is embedded in every build.
