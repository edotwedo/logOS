#!/usr/bin/env bash
set -euo pipefail

# logOS QEMU Test Runner
echo "=== Building logOS ==="
cargo build --release --target x86_64-unknown-uefi

# Check for QEMU
if ! command -v qemu-system-x86_64 &>/dev/null; then
    echo "Error: qemu-system-x86_64 not found."
    echo "Install it via your package manager, e.g. on Arch:"
    echo "  sudo pacman -S qemu-system-x86 edk2-ovmf"
    exit 1
fi

# Locate OVMF firmware
OVMF_CODE=""
for path in \
    /usr/share/edk2/x64/OVMF_CODE.4m.fd \
    /usr/share/edk2/x64/OVMF.4m.fd \
    /usr/share/edk2-ovmf/x64/OVMF_CODE.fd \
    /usr/share/ovmf/x64/OVMF.fd \
    /usr/share/OVMF/OVMF_CODE.fd; do
    if [[ -f "$path" ]]; then
        OVMF_CODE="$path"
        break
    fi
done

if [[ -z "$OVMF_CODE" ]]; then
    echo "Error: OVMF UEFI firmware file not found."
    echo "Please install edk2-ovmf (e.g. sudo pacman -S edk2-ovmf)."
    exit 1
fi

# Create EFI System Partition layout in esp/
mkdir -p esp/EFI/BOOT
cp target/x86_64-unknown-uefi/release/logos.efi esp/EFI/BOOT/BOOTX64.EFI

echo "=== Launching logOS in QEMU (Direct UEFI GOP Framebuffer) ==="
qemu-system-x86_64 \
    -enable-kvm \
    -m 2G \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive format=raw,file=fat:rw:esp \
    -vga std \
    -serial stdio
