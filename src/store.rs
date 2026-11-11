//! The app shelf: logOS's own on-disk format for the apps the boss writes. No FAT, no files -
//! fixed slots on a disk that belongs to logOS.
//!
//! Sector 0: "LOGOSAPPS1" + slot count. Slot i starts at sector 1 + i * SLOT_SECTORS:
//! "APP1", source length (u32), name (48 bytes, NUL padded), then the source.
//! logOS only formats a disk that is entirely blank at sector 0; any other disk is left alone.

use alloc::string::String;
use alloc::vec::Vec;

use crate::virtio_blk::{VirtioBlk, SECTOR};

const MAGIC: &[u8; 10] = b"LOGOSAPPS1";
const SLOT_MAGIC: &[u8; 4] = b"APP1";
const SLOTS: usize = 64;
const SLOT_SECTORS: usize = 64; // 32 KiB per app
const HEAD: usize = 56;
const NAME: usize = 48;
pub const MAX_SOURCE: usize = SLOT_SECTORS * SECTOR - HEAD;

pub struct Store {
    disk: VirtioBlk,
    names: Vec<Option<String>>, // slot -> name
}

fn slot_sector(i: usize) -> u64 {
    1 + (i * SLOT_SECTORS) as u64
}

impl Store {
    /// Open the shelf on this disk, formatting it only if it is blank.
    pub fn open(mut disk: VirtioBlk) -> Result<Store, &'static str> {
        if disk.sectors < slot_sector(SLOTS) {
            return Err("disk too small for the app shelf (needs 2 MiB)");
        }
        let mut s = [0u8; SECTOR];
        disk.read(0, &mut s)?;
        if &s[..10] != MAGIC {
            if s.iter().any(|&b| b != 0) {
                return Err("this disk has something else on it; leaving it alone");
            }
            s[..10].copy_from_slice(MAGIC);
            s[10..14].copy_from_slice(&(SLOTS as u32).to_le_bytes());
            disk.write(0, &s)?;
            let blank = [0u8; SECTOR];
            for i in 0..SLOTS {
                disk.write(slot_sector(i), &blank)?;
            }
            log::info!("app shelf formatted: {} slots", SLOTS);
        }
        let mut names = Vec::with_capacity(SLOTS);
        for i in 0..SLOTS {
            disk.read(slot_sector(i), &mut s)?;
            names.push(if &s[..4] == SLOT_MAGIC { Some(name_of(&s)) } else { None });
        }
        Ok(Store { disk, names })
    }

    pub fn list(&self) -> Vec<String> {
        self.names.iter().flatten().cloned().collect()
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| n.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(name)))
    }

    pub fn load(&mut self, name: &str) -> Result<(String, String), &'static str> {
        let i = self.find(name).ok_or("no saved app by that name")?;
        let mut s = [0u8; SECTOR];
        self.disk.read(slot_sector(i), &mut s)?;
        let len = u32::from_le_bytes([s[4], s[5], s[6], s[7]]) as usize;
        if len > MAX_SOURCE {
            return Err("saved app is damaged");
        }
        let mut bytes = Vec::with_capacity(len);
        bytes.extend_from_slice(&s[HEAD..(HEAD + len).min(SECTOR)]);
        let mut sector = slot_sector(i) + 1;
        while bytes.len() < len {
            self.disk.read(sector, &mut s)?;
            let take = (len - bytes.len()).min(SECTOR);
            bytes.extend_from_slice(&s[..take]);
            sector += 1;
        }
        Ok((name_of_slot(&self.names[i]), String::from_utf8(bytes).map_err(|_| "saved app is damaged")?))
    }

    /// Save (or replace) an app. The header sector is written last, so a half-written app
    /// never looks complete.
    pub fn save(&mut self, name: &str, source: &str) -> Result<(), &'static str> {
        if source.len() > MAX_SOURCE {
            return Err("app is too big to save (32 KiB max)");
        }
        let i = match self.find(name) {
            Some(i) => i,
            None => self.names.iter().position(|n| n.is_none()).ok_or("the shelf is full (64 apps)")?,
        };
        let mut name_bytes: Vec<u8> = name.bytes().filter(|b| (b' '..=b'~').contains(b)).take(NAME).collect();
        if name_bytes.is_empty() {
            name_bytes.extend_from_slice(b"app");
        }
        let src = source.as_bytes();
        let first = src.len().min(SECTOR - HEAD);
        let mut sector = slot_sector(i) + 1;
        for chunk in src[first..].chunks(SECTOR) {
            let mut s = [0u8; SECTOR];
            s[..chunk.len()].copy_from_slice(chunk);
            self.disk.write(sector, &s)?;
            sector += 1;
        }
        let mut s = [0u8; SECTOR];
        s[..4].copy_from_slice(SLOT_MAGIC);
        s[4..8].copy_from_slice(&(src.len() as u32).to_le_bytes());
        s[8..8 + name_bytes.len()].copy_from_slice(&name_bytes);
        s[HEAD..HEAD + first].copy_from_slice(&src[..first]);
        self.disk.write(slot_sector(i), &s)?;
        self.names[i] = Some(String::from_utf8(name_bytes).unwrap_or_default());
        Ok(())
    }

    pub fn forget(&mut self, name: &str) -> Result<String, &'static str> {
        let i = self.find(name).ok_or("no saved app by that name")?;
        self.disk.write(slot_sector(i), &[0u8; SECTOR])?;
        Ok(self.names[i].take().unwrap_or_default())
    }
}

fn name_of(s: &[u8; SECTOR]) -> String {
    let raw = &s[8..8 + NAME];
    let end = raw.iter().position(|&b| b == 0).unwrap_or(NAME);
    String::from(core::str::from_utf8(&raw[..end]).unwrap_or("app"))
}

fn name_of_slot(n: &Option<String>) -> String {
    n.clone().unwrap_or_default()
}
