//! Disk driver: finds a virtio block device on the PCI bus and drives it through its legacy
//! I/O-port interface. One request queue, polled, one request at a time. Every request is a
//! three-descriptor chain: header (device reads), data, status byte (device writes).

use alloc::alloc::{alloc_zeroed, Layout};
use core::sync::atomic::{fence, Ordering};

use crate::arch::{inb, outb};
use crate::virtio_net::{inl, inw, outl, outw, pci_devices, pci_read, pci_write};

pub const SECTOR: usize = 512;
const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;
const T_IN: u32 = 0; // read
const T_OUT: u32 = 1; // write

const R_DEV_FEATURES: u16 = 0x00;
const R_DRV_FEATURES: u16 = 0x04;
const R_QUEUE_PFN: u16 = 0x08;
const R_QUEUE_SIZE: u16 = 0x0C;
const R_QUEUE_SELECT: u16 = 0x0E;
const R_QUEUE_NOTIFY: u16 = 0x10;
const R_STATUS: u16 = 0x12;
const R_CAPACITY: u16 = 0x14; // u64, in 512-byte sectors
const F_RO: u32 = 1 << 5;

#[repr(C)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

pub struct VirtioBlk {
    io: u16,
    size: u16,
    desc: *mut Desc,
    avail: *mut u16,
    used: *mut u8,
    last_used: u16,
    hdr: *mut u8,    // 16-byte request header
    status: *mut u8, // 1 byte
    data: *mut u8,   // one sector bounce buffer
    pub sectors: u64,
    pub read_only: bool,
}

impl VirtioBlk {
    /// Find and start the first virtio disk (vendor 1AF4, legacy device 1001).
    pub fn probe() -> Option<VirtioBlk> {
        let (bus, dev, func, _, _) = pci_devices().into_iter().find(|d| d.3 == 0x1AF4 && d.4 == 0x1001)?;
        let bar0 = pci_read(bus, dev, func, 0x10);
        if bar0 & 1 == 0 {
            log::info!("virtio-blk has no I/O-port BAR (modern-only device); run QEMU with disable-modern=on");
            return None;
        }
        let io = (bar0 & 0xFFFC) as u16;
        let cmd = pci_read(bus, dev, func, 0x04);
        pci_write(bus, dev, func, 0x04, cmd | 0x05);

        outb(io + R_STATUS, 0);
        outb(io + R_STATUS, 1);
        outb(io + R_STATUS, 1 | 2);
        let features = inl(io + R_DEV_FEATURES);
        outl(io + R_DRV_FEATURES, features & F_RO);

        outw(io + R_QUEUE_SELECT, 0);
        let size = inw(io + R_QUEUE_SIZE);
        if size < 3 {
            log::info!("virtio-blk queue too small ({})", size);
            return None;
        }
        let n = size as usize;
        let avail_off = 16 * n;
        let used_off = (avail_off + 6 + 2 * n).div_ceil(4096) * 4096;
        let total = used_off + (6 + 8 * n).div_ceil(4096) * 4096;
        let (base, scratch) = unsafe {
            (
                alloc_zeroed(Layout::from_size_align(total, 4096).unwrap()),
                alloc_zeroed(Layout::from_size_align(4096, 4096).unwrap()),
            )
        };
        outl(io + R_QUEUE_PFN, (base as usize >> 12) as u32);
        let sectors = inl(io + R_CAPACITY) as u64 | ((inl(io + R_CAPACITY + 4) as u64) << 32);
        outb(io + R_STATUS, 1 | 2 | 4);
        let read_only = features & F_RO != 0;
        log::info!(
            "virtio-blk up at PCI {:02x}:{:02x}.{} io {:#x}, queue {}, {} KiB{}",
            bus, dev, func, io, size, sectors * SECTOR as u64 / 1024, if read_only { ", read-only" } else { "" }
        );
        let _ = inb(io + R_STATUS);
        unsafe {
            Some(VirtioBlk {
                io,
                size,
                desc: base as *mut Desc,
                avail: base.add(avail_off) as *mut u16,
                used: base.add(used_off),
                last_used: 0,
                hdr: scratch,
                status: scratch.add(16),
                data: scratch.add(SECTOR),
                sectors,
                read_only,
            })
        }
    }

    fn request(&mut self, kind: u32, sector: u64) -> Result<(), &'static str> {
        if sector >= self.sectors {
            return Err("sector past the end of the disk");
        }
        if kind == T_OUT && self.read_only {
            return Err("disk is read-only");
        }
        unsafe {
            core::ptr::write_unaligned(self.hdr as *mut u32, kind);
            core::ptr::write_unaligned(self.hdr.add(4) as *mut u32, 0);
            core::ptr::write_unaligned(self.hdr.add(8) as *mut u64, sector);
            *self.status = 0xFF;
            let d = |i: usize| &mut *self.desc.add(i);
            *d(0) = Desc { addr: self.hdr as u64, len: 16, flags: DESC_F_NEXT, next: 1 };
            let data_flags = if kind == T_IN { DESC_F_WRITE } else { 0 };
            *d(1) = Desc { addr: self.data as u64, len: SECTOR as u32, flags: data_flags | DESC_F_NEXT, next: 2 };
            *d(2) = Desc { addr: self.status as u64, len: 1, flags: DESC_F_WRITE, next: 0 };
            let idx = core::ptr::read_volatile(self.avail.add(1));
            core::ptr::write_volatile(self.avail.add(2 + (idx % self.size) as usize), 0);
            fence(Ordering::SeqCst);
            core::ptr::write_volatile(self.avail.add(1), idx.wrapping_add(1));
            fence(Ordering::SeqCst);
            outw(self.io + R_QUEUE_NOTIFY, 0);
            let mut spins = 0u64;
            loop {
                fence(Ordering::SeqCst);
                if core::ptr::read_volatile(self.used.add(2) as *const u16) != self.last_used {
                    break;
                }
                spins += 1;
                if spins > 2_000_000_000 {
                    return Err("disk did not answer");
                }
                core::hint::spin_loop();
            }
            self.last_used = self.last_used.wrapping_add(1);
            match core::ptr::read_volatile(self.status) {
                0 => Ok(()),
                1 => Err("disk I/O error"),
                _ => Err("disk refused the request"),
            }
        }
    }

    pub fn read(&mut self, sector: u64, out: &mut [u8; SECTOR]) -> Result<(), &'static str> {
        self.request(T_IN, sector)?;
        unsafe { core::ptr::copy_nonoverlapping(self.data, out.as_mut_ptr(), SECTOR) };
        Ok(())
    }

    pub fn write(&mut self, sector: u64, buf: &[u8; SECTOR]) -> Result<(), &'static str> {
        unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), self.data, SECTOR) };
        self.request(T_OUT, sector)
    }
}
