//! PS/2 keyboard driver: the 8042 controller and scancode set 1 (what the controller translates to).

use crate::arch::{inb, outb};

fn wait_write() {
    for _ in 0..100_000 {
        if inb(0x64) & 0x02 == 0 {
            return;
        }
    }
}

fn wait_read() -> bool {
    for _ in 0..100_000 {
        if inb(0x64) & 0x01 != 0 {
            return true;
        }
    }
    false
}

/// The firmware may have left the controller polled. Turn on keyboard interrupts and translation.
pub fn controller_init() {
    while inb(0x64) & 0x01 != 0 {
        inb(0x60); // flush anything pending
    }
    wait_write();
    outb(0x64, 0x20); // read the configuration byte
    let config = if wait_read() { inb(0x60) } else { 0x45 };
    let config = (config | 0x01 | 0x40) & !0x10; // IRQ1 on, translation on, keyboard clock on
    wait_write();
    outb(0x64, 0x60);
    wait_write();
    outb(0x60, config);
    wait_write();
    outb(0x64, 0xAE); // enable the keyboard port
}

pub struct Keyboard {
    shift: bool,
}

const PLAIN: &[u8; 58] = b"\0\x1b1234567890-=\x08\tqwertyuiop[]\n\0asdfghjkl;'`\0\\zxcvbnm,./\0*\0 ";
const SHIFTED: &[u8; 58] = b"\0\x1b!@#$%^&*()_+\x08\tQWERTYUIOP{}\n\0ASDFGHJKL:\"~\0|ZXCVBNM<>?\0*\0 ";

impl Keyboard {
    pub const fn new() -> Self {
        Keyboard { shift: false }
    }

    /// Turn one scancode into a character, if it is a key press that makes one.
    pub fn feed(&mut self, sc: u8) -> Option<char> {
        match sc {
            0x2A | 0x36 => self.shift = true,
            0xAA | 0xB6 => self.shift = false,
            s if s < 58 => {
                let c = if self.shift { SHIFTED[s as usize] } else { PLAIN[s as usize] };
                if c != 0 {
                    return Some(c as char);
                }
            }
            _ => {}
        }
        None
    }
}
