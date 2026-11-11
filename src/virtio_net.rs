//! Network driver: finds a virtio network card on the PCI bus and drives it through its legacy
//! I/O-port interface. Two queues (receive and transmit), polled; no interrupts needed.
//! Memory is identity-mapped (UEFI leaves it that way), so a pointer is a physical address.

use alloc::alloc::{alloc_zeroed, Layout};
use alloc::vec::Vec;
use core::sync::atomic::{fence, Ordering};

use crate::arch::{inb, outb};

// ---------------------------------------------------------------- PCI configuration space

pub(crate) fn outl(port: u16, v: u32) {
    unsafe { core::arch::asm!("out dx, eax", in("dx") port, in("eax") v, options(nomem, nostack, preserves_flags)) }
}
pub(crate) fn inl(port: u16) -> u32 {
    let v: u32;
    unsafe { core::arch::asm!("in eax, dx", out("eax") v, in("dx") port, options(nomem, nostack, preserves_flags)) }
    v
}
pub(crate) fn outw(port: u16, v: u16) {
    unsafe { core::arch::asm!("out dx, ax", in("dx") port, in("ax") v, options(nomem, nostack, preserves_flags)) }
}
pub(crate) fn inw(port: u16) -> u16 {
    let v: u16;
    unsafe { core::arch::asm!("in ax, dx", out("ax") v, in("dx") port, options(nomem, nostack, preserves_flags)) }
    v
}

pub(crate) fn pci_read(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    outl(0xCF8, 0x8000_0000 | (bus as u32) << 16 | (dev as u32) << 11 | (func as u32) << 8 | (off as u32 & 0xFC));
    inl(0xCFC)
}
pub(crate) fn pci_write(bus: u8, dev: u8, func: u8, off: u8, v: u32) {
    outl(0xCF8, 0x8000_0000 | (bus as u32) << 16 | (dev as u32) << 11 | (func as u32) << 8 | (off as u32 & 0xFC));
    outl(0xCFC, v);
}

/// Every device on the PCI bus as (bus, device, function, vendor, device id).
pub fn pci_devices() -> Vec<(u8, u8, u8, u16, u16)> {
    let mut out = Vec::new();
    for bus in 0..=255u8 {
        for dev in 0..32u8 {
            for func in 0..8u8 {
                let id = pci_read(bus, dev, func, 0);
                if id & 0xFFFF == 0xFFFF {
                    if func == 0 {
                        break;
                    }
                    continue;
                }
                out.push((bus, dev, func, id as u16, (id >> 16) as u16));
            }
        }
    }
    out
}

// ---------------------------------------------------------------- virtqueues

const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;
const BUF: usize = 2048;
const NET_HDR: usize = 10; // legacy virtio_net_hdr without merged buffers

#[repr(C)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

struct Queue {
    size: u16,
    desc: *mut Desc,
    avail: *mut u16, // flags, idx, ring[size]
    used: *mut u8,   // flags u16, idx u16, ring[size] of (id u32, len u32)
    bufs: *mut u8,   // size * BUF bytes
    last_used: u16,
    free: Vec<u16>,
}

impl Queue {
    fn new(size: u16) -> Queue {
        let n = size as usize;
        let avail_off = 16 * n;
        let used_off = (avail_off + 6 + 2 * n).div_ceil(4096) * 4096;
        let total = used_off + (6 + 8 * n).div_ceil(4096) * 4096;
        unsafe {
            let base = alloc_zeroed(Layout::from_size_align(total, 4096).unwrap());
            let bufs = alloc_zeroed(Layout::from_size_align(n * BUF, 4096).unwrap());
            Queue {
                size,
                desc: base as *mut Desc,
                avail: base.add(avail_off) as *mut u16,
                used: base.add(used_off),
                bufs,
                last_used: 0,
                free: (0..size).collect(),
            }
        }
    }

    fn pfn(&self) -> u32 {
        (self.desc as usize >> 12) as u32
    }

    fn buf(&self, i: u16) -> *mut u8 {
        unsafe { self.bufs.add(i as usize * BUF) }
    }

    /// Hand descriptor `i` (covering `len` bytes of its buffer) to the device.
    fn offer(&mut self, i: u16, len: usize, device_writes: bool) {
        unsafe {
            let d = &mut *self.desc.add(i as usize);
            d.addr = self.buf(i) as u64;
            d.len = len as u32;
            d.flags = if device_writes { DESC_F_WRITE } else { 0 } & !DESC_F_NEXT;
            d.next = 0;
            let idx = core::ptr::read_volatile(self.avail.add(1));
            core::ptr::write_volatile(self.avail.add(2 + (idx % self.size) as usize), i);
            fence(Ordering::SeqCst);
            core::ptr::write_volatile(self.avail.add(1), idx.wrapping_add(1));
            fence(Ordering::SeqCst);
        }
    }

    /// The next buffer the device has finished with: (descriptor, bytes written).
    fn take(&mut self) -> Option<(u16, usize)> {
        unsafe {
            fence(Ordering::SeqCst);
            let idx = core::ptr::read_volatile(self.used.add(2) as *const u16);
            if idx == self.last_used {
                return None;
            }
            let e = self.used.add(4 + 8 * (self.last_used % self.size) as usize) as *const u32;
            let id = core::ptr::read_volatile(e) as u16;
            let len = core::ptr::read_volatile(e.add(1)) as usize;
            self.last_used = self.last_used.wrapping_add(1);
            Some((id, len))
        }
    }
}

// ---------------------------------------------------------------- the device

pub struct VirtioNet {
    io: u16,
    rx: Queue,
    tx: Queue,
    pub mac: [u8; 6],
}

// Legacy register offsets from BAR0.
const R_DEV_FEATURES: u16 = 0x00;
const R_DRV_FEATURES: u16 = 0x04;
const R_QUEUE_PFN: u16 = 0x08;
const R_QUEUE_SIZE: u16 = 0x0C;
const R_QUEUE_SELECT: u16 = 0x0E;
const R_QUEUE_NOTIFY: u16 = 0x10;
const R_STATUS: u16 = 0x12;
const R_MAC: u16 = 0x14;
const F_MAC: u32 = 1 << 5;

impl VirtioNet {
    /// Find and start the first virtio network card (vendor 1AF4, legacy device 1000).
    pub fn probe() -> Option<VirtioNet> {
        let (bus, dev, func, _, _) = pci_devices().into_iter().find(|d| d.3 == 0x1AF4 && d.4 == 0x1000)?;
        let bar0 = pci_read(bus, dev, func, 0x10);
        if bar0 & 1 == 0 {
            log::info!("virtio-net has no I/O-port BAR (modern-only device); run QEMU with disable-modern=on");
            return None;
        }
        let io = (bar0 & 0xFFFC) as u16;
        // Turn on I/O decoding and bus mastering so the card can reach our memory.
        let cmd = pci_read(bus, dev, func, 0x04);
        pci_write(bus, dev, func, 0x04, cmd | 0x05);

        outb(io + R_STATUS, 0); // reset
        outb(io + R_STATUS, 1); // acknowledge
        outb(io + R_STATUS, 1 | 2); // driver
        let features = inl(io + R_DEV_FEATURES);
        outl(io + R_DRV_FEATURES, features & F_MAC);

        let setup = |q: u16| -> Option<Queue> {
            outw(io + R_QUEUE_SELECT, q);
            let size = inw(io + R_QUEUE_SIZE);
            if size == 0 {
                return None;
            }
            let queue = Queue::new(size);
            outl(io + R_QUEUE_PFN, queue.pfn());
            Some(queue)
        };
        let mut rx = setup(0)?;
        let tx = setup(1)?;

        let mut mac = [0u8; 6];
        for (i, m) in mac.iter_mut().enumerate() {
            *m = inb(io + R_MAC + i as u16);
        }
        outb(io + R_STATUS, 1 | 2 | 4); // driver ready

        // Give the card every receive buffer.
        for i in 0..rx.size {
            rx.offer(i, BUF, true);
        }
        rx.free.clear();
        outw(io + R_QUEUE_NOTIFY, 0);
        log::info!(
            "virtio-net up at PCI {:02x}:{:02x}.{} io {:#x}, queues rx {} tx {}, MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            bus, dev, func, io, rx.size, tx.size, mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        );
        Some(VirtioNet { io, rx, tx, mac })
    }

    /// A received Ethernet frame, if one is waiting; `f` sees it, then the buffer goes back to the card.
    pub fn recv<R>(&mut self, f: impl FnOnce(&[u8]) -> R) -> Option<R> {
        let (id, len) = self.rx.take()?;
        let frame = unsafe { core::slice::from_raw_parts(self.rx.buf(id).add(NET_HDR), len.saturating_sub(NET_HDR)) };
        let r = f(frame);
        self.rx.offer(id, BUF, true);
        outw(self.io + R_QUEUE_NOTIFY, 0);
        Some(r)
    }

    pub fn can_send(&mut self) -> bool {
        while let Some((id, _)) = self.tx.take() {
            self.tx.free.push(id);
        }
        !self.tx.free.is_empty()
    }

    /// Send one Ethernet frame of `len` bytes that `f` writes.
    pub fn send<R>(&mut self, len: usize, f: impl FnOnce(&mut [u8]) -> R) -> R {
        while !self.can_send() {
            core::hint::spin_loop();
        }
        let id = self.tx.free.pop().unwrap();
        let len = len.min(BUF - NET_HDR);
        let buf = self.tx.buf(id);
        let r = unsafe {
            core::ptr::write_bytes(buf, 0, NET_HDR);
            f(core::slice::from_raw_parts_mut(buf.add(NET_HDR), len))
        };
        self.tx.offer(id, NET_HDR + len, false);
        outw(self.io + R_QUEUE_NOTIFY, 1);
        r
    }
}
