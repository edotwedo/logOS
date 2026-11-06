# logOS

> **An AI-native, bare-metal operating substrate and procedural dreamscape booting directly on raw silicon.**

```text
    _             ___  ____  
   | | ___   __ _| _ \/ ___| 
   | |/ _ \ / _` | | | \___ \ 
   | | (_) | (_| | |_| |___) |
   |_|\___/ \__, |\___/|____/ 
            |___/             
   Direct Bare-Metal Substrate
```

---

## What is logOS?

**logOS** is an experimental operating environment designed from first principles for the post-operating-system era.

Instead of running an AI assistant as a userland process stacked on top of 40 million lines of Linux kernel, glibc, systemd, display servers, and POSIX abstraction layers, **logOS boots directly from UEFI firmware into raw silicon**.

There is no desktop. There is no command shell. There is no file explorer.

From cold boot, logOS initializes:
1. **Direct Silicon Control**: `#![no_std]` Rust unikernel executing in x86_64 64-bit long mode.
2. **Raw Linear Framebuffer**: Immediate access to the display hardware via UEFI Graphics Output Protocol (GOP) — zero compositing latency, pure mathematical pixel generation.
3. **Dreamscape Environment**: Atmospheric visual computing focused on beauty, volumetrics, bioluminescence, and procedural dreamscapes rather than sterile windows or simulated realism.
4. **Cognitive I/O Loop**: Voice-first conversation powered by local quantized weights residing directly in physical RAM.

---

## Architectural Principles

- **Zero Legacy Overhead**: No paging to swap, no daemon sprawl, no context-switch storms between kernel and user space. The CPU and GPU serve one unified goal: intelligence and perception.
- **Eye Candy Over Realism**: Visuals are generated using procedural Signed Distance Fields (SDFs), raymarching, and harmonic math. We prioritize aesthetic wonder, twilight gradients, and organic bioluminescence over sterile photorealism.
- **Voice As The Command Substrate**: DMA-driven audio ring buffers directly interfaced to the chipset. Speaking to the computer is the native interaction model.

---

## Roadmap

| Milestone | Capability | Status |
| :--- | :--- | :---: |
| **M1: Direct Silicon Boot** | Rust `no_std` UEFI application, GOP framebuffer acquisition, raw VRAM twilight canvas | **Active (Current)** |
| **M2: Dynamic Dreamscapes** | Double-buffered 60 FPS procedural SDF raymarching engine, harmonic wavefields | *Planned* |
| **M3: Audio DMA Pipeline** | Intel High Definition Audio (HDA) / USB Audio Class drivers for zero-latency mic/speaker streams | *Planned* |
| **M4: Embedded Tensor Engine** | Bare-metal GGUF/W8A8 quantized SLM inference (SmolLM2 / Llama 3.2) residing in unified physical memory | *Planned* |
| **M5: Autonomous Cognition** | Unbroken voice-driven conversational consciousness running on bare metal | *Planned* |

---

## Quickstart & Building

### Prerequisites

- **Rust** (nightly or modern stable) with UEFI target:
  ```bash
  rustup target add x86_64-unknown-uefi
  ```

### Build the Substrate

```bash
# Debug build (includes UEFI logging symbols)
cargo build

# Release build (ultra-lean, ~37 KB standalone binary!)
cargo build --release
```

The output PE/COFF UEFI executable will be at:
`target/x86_64-unknown-uefi/release/logos.efi`

---

## Running

### Option A: Booting in QEMU (Virtual Raw Silicon)

Install QEMU and OVMF UEFI firmware (e.g., on Arch: `sudo pacman -S qemu-system-x86 edk2-ovmf`), then launch:

```bash
./run_qemu.sh
```

### Option B: Booting on Real Hardware via USB

Format any USB thumb drive with a standard FAT32 partition, then run:

```bash
./make_bootable_usb.sh /run/media/$USER/YOUR_USB_DRIVE
```

Plug the drive into any modern PC or laptop, press the UEFI Boot Menu key (typically `F12`, `F11`, or `Del`), and select the USB drive. **logOS will boot instantly into raw silicon.**

---

## License

MIT OR Apache-2.0
