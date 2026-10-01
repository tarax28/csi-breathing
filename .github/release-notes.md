## Downloads

| System | File | How to run |
|---|---|---|
| Windows 10/11 (64-bit) | `csi-breathing-windows-x64.exe` | Double-click. SmartScreen may warn: More info > Run anyway. |
| macOS 11+ (Apple Silicon and Intel) | `csi-breathing-macos-universal.zip` | Unzip and open `CSI Breathing.app`. It is not notarised: allow it under System Settings > Privacy & Security > Open Anyway, or run `xattr -dr com.apple.quarantine "CSI Breathing.app"`. |
| Linux x86-64 (glibc 2.35+) | `csi-breathing-linux-x64.tar.gz` | `tar xzf csi-breathing-linux-x64.tar.gz`, then `./csi-breathing`. |

The TX/RX firmware for the ESP32-C5 boards is embedded in every build.
