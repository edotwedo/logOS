#!/bin/bash
# Boot logOS headless in QEMU, wait for the network, type a question, and check the answer
# came back over HTTPS. Used by CI; runs the same on a dev box.
#   scripts/boot_test.sh [question]      (env: OVMF=<firmware>, OUT=<dir>, CPU="-cpu max")
set -u
Q=${1:-what is two plus two?}
OUT=${OUT:-$(mktemp -d)}
EFI=target/x86_64-unknown-uefi/release/logos.efi
OVMF=${OVMF:-$(ls /usr/share/OVMF/OVMF_CODE*.fd /usr/share/ovmf/OVMF.fd /usr/share/edk2/x64/OVMF_CODE.4m.fd 2>/dev/null | head -1)}
SOCK=${XDG_RUNTIME_DIR:-/tmp}/logos-mon-$$.sock
LOG=$OUT/boot.log
mkdir -p "$OUT/esp/EFI/BOOT"
cp "$EFI" "$OUT/esp/EFI/BOOT/BOOTX64.EFI"
truncate -s 4M "$OUT/apps.img"
echo "firmware: $OVMF  log: $LOG"
qemu-system-x86_64 -m 1024 -display none -no-reboot ${CPU:-} \
  -drive if=pflash,format=raw,readonly=on,file="$OVMF" \
  -drive format=raw,file=fat:rw:"$OUT/esp" \
  -drive if=none,format=raw,file="$OUT/apps.img",id=shelf -device virtio-blk-pci,drive=shelf,disable-modern=on \
  -netdev user,id=n0 -device virtio-net-pci,netdev=n0,disable-modern=on \
  -serial file:"$LOG" -monitor unix:"$SOCK",server,nowait &
QP=$!
trap 'kill $QP 2>/dev/null; rm -f "$SOCK"' EXIT

wait_for() { # pattern seconds
  for _ in $(seq "$2"); do grep -sqE "$1" "$LOG" && return 0; sleep 1; done
  return 1
}

if wait_for "uplink connected" 60; then
  sleep 2
  python3 - "$SOCK" "$Q" <<'PY'
import socket, sys, time
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); time.sleep(0.3)
names = {' ': 'spc', '.': 'dot', ',': 'comma', '?': 'shift-slash', "'": 'apostrophe', '-': 'minus'}
for ch in sys.argv[2]:
    key = names.get(ch, 'shift-' + ch.lower() if ch.isupper() else ch)
    s.send(f"sendkey {key}\n".encode()); time.sleep(0.08)
s.send(b"sendkey ret\n"); time.sleep(0.3)
PY
  wait_for "boss said|TLS failed" 150
fi
sleep 2
tail -30 "$LOG"
ok=0
for want in "logOS v" "logOS owns the machine" "entropy pool seeded" "wall clock from CMOS" "virtio-net up" \
            "network up: address" "app shelf open" "TLS handshake done" "HTTPS reply: [0-9]+ bytes" "boss said: "; do
  grep -qE "$want" "$LOG" || { echo "MISSING: $want"; ok=1; }
done
if grep -qE "CPU fault|panic:|TLS failed" "$LOG"; then echo "FAILED: fault, panic or TLS failure in the log"; ok=1; fi
[ $ok = 0 ] && echo "boot test passed"
exit $ok
