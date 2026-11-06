#!/usr/bin/env bash
set -euo pipefail

# logOS Bootable USB Creator
# Usage: ./make_bootable_usb.sh /path/to/usb/mount/point
TARGET_MOUNT="${1:-}"

if [[ -z "$TARGET_MOUNT" || ! -d "$TARGET_MOUNT" ]]; then
    echo "Usage: $0 /path/to/mounted/fat32/usb"
    echo "Example: $0 /run/media/$USER/MY_USB"
    exit 1
fi

echo "=== Building logOS Release ==="
cargo build --release --target x86_64-unknown-uefi

echo "=== Deploying to $TARGET_MOUNT/EFI/BOOT/BOOTX64.EFI ==="
mkdir -p "$TARGET_MOUNT/EFI/BOOT"
cp target/x86_64-unknown-uefi/release/logos.efi "$TARGET_MOUNT/EFI/BOOT/BOOTX64.EFI"

echo "Done! Plug the USB into any PC, press F12/Del to enter UEFI boot menu, and boot logOS directly."
