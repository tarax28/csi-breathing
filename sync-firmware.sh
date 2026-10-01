#!/usr/bin/env bash
# Copy the ESP-IDF build outputs from ../firmware into prebuilt/, the copy kept in this repo
# so the app can be built where the firmware is not (CI). Run after rebuilding the firmware.
set -euo pipefail
cd "$(dirname "$0")"
for app in csi_send csi_recv; do
    src=../firmware/$app
    dst=prebuilt/$app
    mkdir -p "$dst/build/bootloader" "$dst/build/partition_table"
    cp "$src/build/bootloader/bootloader.bin" "$dst/build/bootloader/"
    cp "$src/build/partition_table/partition-table.bin" "$dst/build/partition_table/"
    cp "$src/build/$app.bin" "$dst/build/"
    stat -c %Y "$src/build/$app.bin" > "$dst/built"
    grep '^CONFIG_CSI_' "$src/sdkconfig" > "$dst/sdkconfig"
done
du -sh prebuilt
