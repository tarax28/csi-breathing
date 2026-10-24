## What's new in v0.3.0

- **Fewer lost packets, also on macOS.** The RX firmware now sends each CSI packet as ~400 bytes (raw values in base64 plus the gain) instead of 830-1100 bytes of decimal text, so 100 packets/s fits the USB serial link with room to spare. **Flash the RX again** (Boards page) to get it; older logs still open.
- Embedded firmware set to channel 157, country IT, TX 6.75 dBm (was channel 161, US).
- Apps older than v0.3.0 cannot read recordings from the new RX firmware.

## Downloads

| System | File | How to run |
|---|---|---|
| Windows 10/11 (64-bit) | `csi-breathing-windows-x64.exe` | Double-click. SmartScreen may warn: More info > Run anyway. |
| macOS 11+ (Apple Silicon and Intel) | `csi-breathing-macos-universal.zip` | Unzip and open `CSI Breathing.app`. It is not notarised: allow it under System Settings > Privacy & Security > Open Anyway, or run `xattr -dr com.apple.quarantine "CSI Breathing.app"`. |
| Linux x86-64 (glibc 2.35+) | `csi-breathing-linux-x64.tar.gz` | `tar xzf csi-breathing-linux-x64.tar.gz`, then `./csi-breathing`. |

The TX/RX firmware for the ESP32-C5 boards is embedded in every build.
