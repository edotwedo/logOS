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
# The brain: if models/ has a llama2.c model, put it on the boot drive.
if [[ -f models/stories15M.bin && -f models/tokenizer.bin ]]; then
    mkdir -p esp/models && cp models/stories15M.bin models/tokenizer.bin esp/models/
else
    echo "No model in models/ - logOS will boot with its untrained brain. To give it one:"
    echo "  curl -L -o models/stories15M.bin https://huggingface.co/karpathy/tinyllamas/resolve/main/stories15M.bin"
    echo "  curl -L -o models/tokenizer.bin https://raw.githubusercontent.com/karpathy/llama2.c/master/tokenizer.bin"
fi

echo "=== Launching logOS in QEMU (Direct UEFI GOP Framebuffer) ==="
# Serial goes to the uplink bridge, which prints logOS's log and wires it to the boss brain.
qemu-system-x86_64 \
    -enable-kvm -cpu host \
    -m 2G \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive format=raw,file=fat:rw:esp \
    -vga std \
    -netdev user,id=net0 -device virtio-net-pci,netdev=net0,disable-modern=on \
    -chardev socket,id=uplink,host=127.0.0.1,port=4555,server=on,wait=off \
    -serial chardev:uplink &
QEMU_PID=$!
trap 'kill $QEMU_PID 2>/dev/null' EXIT
python3 tools/uplink.py 4555
