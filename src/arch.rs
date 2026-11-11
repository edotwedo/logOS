//! Owning the machine: our own GDT, interrupt table, interrupt controller (8259 PIC), timer (8253 PIT),
//! and serial port. Nothing here calls the firmware; it all talks to the hardware directly.

use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

// ---------------------------------------------------------------- port I/O

#[inline]
pub fn outb(port: u16, v: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags)) }
}

#[inline]
pub fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe { asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags)) }
    v
}

fn io_wait() {
    outb(0x80, 0);
}

// ---------------------------------------------------------------- serial (COM1) and the logger

const COM1: u16 = 0x3F8;

pub fn serial_init() {
    outb(COM1 + 1, 0x00); // no serial interrupts
    outb(COM1 + 3, 0x80); // divisor latch on
    outb(COM1, 0x01); // 115200 baud
    outb(COM1 + 1, 0x00);
    outb(COM1 + 3, 0x03); // 8 data bits, no parity, one stop bit
    outb(COM1 + 2, 0xC7); // FIFO on
    outb(COM1 + 4, 0x03);
}

pub fn serial_write(s: &str) {
    for b in s.bytes() {
        if b == b'\n' {
            serial_byte(b'\r');
        }
        serial_byte(b);
    }
}

fn serial_byte(b: u8) {
    for _ in 0..100_000 {
        if inb(COM1 + 5) & 0x20 != 0 {
            break;
        }
    }
    outb(COM1, b);
}

struct SerialLog;

impl core::fmt::Write for SerialLog {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        serial_write(s);
        Ok(())
    }
}

impl log::Log for SerialLog {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        let _ = core::fmt::write(&mut SerialLog, format_args!("[logOS] {}\n", r.args()));
    }
    fn flush(&self) {}
}

static LOGGER: SerialLog = SerialLog;

pub fn logger_init() {
    serial_init();
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);
}

// ---------------------------------------------------------------- GDT

#[repr(C, packed)]
struct Pointer {
    limit: u16,
    base: u64,
}

static GDT: [u64; 3] = [
    0,
    0x00AF_9A00_0000_FFFF, // 0x08: 64-bit kernel code
    0x00CF_9200_0000_FFFF, // 0x10: kernel data
];

unsafe fn load_gdt() {
    let ptr = Pointer { limit: (core::mem::size_of_val(&GDT) - 1) as u16, base: GDT.as_ptr() as u64 };
    unsafe {
        asm!(
            "lgdt [{p}]",
            "mov ax, 0x10",
            "mov ds, ax",
            "mov es, ax",
            "mov ss, ax",
            "mov fs, ax",
            "mov gs, ax",
            "push 0x08",
            "lea {t}, [rip + 2f]",
            "push {t}",
            "retfq",
            "2:",
            p = in(reg) &ptr,
            t = out(reg) _,
            out("ax") _,
        );
    }
}

// ---------------------------------------------------------------- IDT and interrupt entry

#[derive(Clone, Copy)]
#[repr(C)]
struct Gate {
    off_lo: u16,
    sel: u16,
    ist: u8,
    attr: u8,
    off_mid: u16,
    off_hi: u32,
    zero: u32,
}

static mut IDT: [Gate; 256] = [Gate { off_lo: 0, sel: 0, ist: 0, attr: 0, off_mid: 0, off_hi: 0, zero: 0 }; 256];

fn gate(handler: u64) -> Gate {
    Gate {
        off_lo: handler as u16,
        sel: 0x08,
        ist: 0,
        attr: 0x8E, // present, ring 0, 64-bit interrupt gate
        off_mid: (handler >> 16) as u16,
        off_hi: (handler >> 32) as u32,
        zero: 0,
    }
}

// Every interrupt saves the registers the System V ABI lets a call clobber, plus all SSE state
// (the dreamscape math lives in xmm registers), then calls `isr_dispatch(vector)`.
// The CPU aligns RSP to 16 before pushing its 40-byte frame; 9 pushes (72 bytes) bring it back to
// a 16-byte boundary for fxsave and the call.
global_asm!(
    r#"
.macro ISR_COMMON vec
    push rax
    push rcx
    push rdx
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    sub rsp, 512
    fxsave64 [rsp]
    mov rdi, \vec
    call {dispatch}
    fxrstor64 [rsp]
    add rsp, 512
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rax
.endm

.global logos_isr_timer
logos_isr_timer:
    ISR_COMMON 32
    iretq

.global logos_isr_keyboard
logos_isr_keyboard:
    ISR_COMMON 33
    iretq

.global logos_isr_serial
logos_isr_serial:
    ISR_COMMON 36
    iretq

.global logos_isr_ignore
logos_isr_ignore:
    iretq

.macro FAULT vec
.global logos_fault_\vec
logos_fault_\vec:
    mov rdi, \vec
    mov rsi, rsp
    and rsp, -16
    call {fault}
.endm
FAULT 0
FAULT 1
FAULT 2
FAULT 3
FAULT 4
FAULT 5
FAULT 6
FAULT 7
FAULT 8
FAULT 9
FAULT 10
FAULT 11
FAULT 12
FAULT 13
FAULT 14
FAULT 15
FAULT 16
FAULT 17
FAULT 18
FAULT 19
FAULT 20
"#,
    dispatch = sym isr_dispatch,
    fault = sym fault_dispatch,
);

unsafe extern "C" {
    fn logos_isr_timer();
    fn logos_isr_keyboard();
    fn logos_isr_ignore();
    fn logos_isr_serial();
    fn logos_fault_0();
    fn logos_fault_1();
    fn logos_fault_2();
    fn logos_fault_3();
    fn logos_fault_4();
    fn logos_fault_5();
    fn logos_fault_6();
    fn logos_fault_7();
    fn logos_fault_8();
    fn logos_fault_9();
    fn logos_fault_10();
    fn logos_fault_11();
    fn logos_fault_12();
    fn logos_fault_13();
    fn logos_fault_14();
    fn logos_fault_15();
    fn logos_fault_16();
    fn logos_fault_17();
    fn logos_fault_18();
    fn logos_fault_19();
    fn logos_fault_20();
}

const FAULT_NAMES: [&str; 21] = [
    "divide error", "debug", "NMI", "breakpoint", "overflow", "bound range", "invalid opcode",
    "device not available", "double fault", "coprocessor overrun", "invalid TSS", "segment not present",
    "stack fault", "general protection fault", "page fault", "reserved", "x87 error", "alignment check",
    "machine check", "SIMD error", "virtualization",
];

extern "sysv64" fn fault_dispatch(vec: u64, stack: *const u64) -> ! {
    let rip = unsafe { *stack.add(if matches!(vec, 8 | 10..=14 | 17) { 1 } else { 0 }) };
    let _ = core::fmt::write(
        &mut SerialLog,
        format_args!("[logOS] CPU fault {} ({}) at rip {:#x}. Halting.\n", vec, FAULT_NAMES[vec as usize], rip),
    );
    halt_forever()
}

pub fn halt_forever() -> ! {
    loop {
        unsafe { asm!("cli; hlt", options(nomem, nostack)) }
    }
}

pub static TICKS: AtomicU64 = AtomicU64::new(0);
pub const TICK_HZ: u64 = 100;

// Keyboard ring buffer: the interrupt writes scancodes, the main loop reads them.
static KEYS: [AtomicU64; 64] = [const { AtomicU64::new(0) }; 64];
static KEY_HEAD: AtomicUsize = AtomicUsize::new(0);
static KEY_TAIL: AtomicUsize = AtomicUsize::new(0);

extern "sysv64" fn isr_dispatch(vec: u64) {
    match vec {
        32 => {
            TICKS.fetch_add(1, Ordering::Relaxed);
        }
        33 => {
            let sc = inb(0x60);
            let head = KEY_HEAD.load(Ordering::Relaxed);
            let next = (head + 1) % KEYS.len();
            if next != KEY_TAIL.load(Ordering::Acquire) {
                KEYS[head].store(sc as u64, Ordering::Relaxed);
                KEY_HEAD.store(next, Ordering::Release);
            }
        }
        36 => {
            // Drain the UART: its FIFO is only 16 bytes, so every byte goes into our ring now.
            while inb(COM1 + 5) & 0x01 != 0 {
                let b = inb(COM1);
                let head = RX_HEAD.load(Ordering::Relaxed);
                let next = (head + 1) % RX_LEN;
                if next != RX_TAIL.load(Ordering::Acquire) {
                    unsafe { RX[head] = b };
                    RX_HEAD.store(next, Ordering::Release);
                }
            }
        }
        _ => {}
    }
    outb(0x20, 0x20); // end of interrupt to the master PIC
}

// Serial receive ring: the uplink to the boss brain arrives here.
const RX_LEN: usize = 16384;
static mut RX: [u8; RX_LEN] = [0; RX_LEN];
static RX_HEAD: AtomicUsize = AtomicUsize::new(0);
static RX_TAIL: AtomicUsize = AtomicUsize::new(0);

pub fn serial_read() -> Option<u8> {
    let tail = RX_TAIL.load(Ordering::Relaxed);
    if tail == RX_HEAD.load(Ordering::Acquire) {
        return None;
    }
    let b = unsafe { RX[tail] };
    RX_TAIL.store((tail + 1) % RX_LEN, Ordering::Release);
    Some(b)
}

pub fn next_scancode() -> Option<u8> {
    let tail = KEY_TAIL.load(Ordering::Relaxed);
    if tail == KEY_HEAD.load(Ordering::Acquire) {
        return None;
    }
    let sc = KEYS[tail].load(Ordering::Relaxed) as u8;
    KEY_TAIL.store((tail + 1) % KEYS.len(), Ordering::Release);
    Some(sc)
}

unsafe fn load_idt() {
    unsafe {
        let idt = &mut *core::ptr::addr_of_mut!(IDT);
        for g in idt.iter_mut() {
            *g = gate(logos_isr_ignore as usize as u64);
        }
        let faults: [unsafe extern "C" fn(); 21] = [
            logos_fault_0, logos_fault_1, logos_fault_2, logos_fault_3, logos_fault_4, logos_fault_5,
            logos_fault_6, logos_fault_7, logos_fault_8, logos_fault_9, logos_fault_10, logos_fault_11,
            logos_fault_12, logos_fault_13, logos_fault_14, logos_fault_15, logos_fault_16,
            logos_fault_17, logos_fault_18, logos_fault_19, logos_fault_20,
        ];
        for (i, f) in faults.iter().enumerate() {
            idt[i] = gate(*f as usize as u64);
        }
        idt[32] = gate(logos_isr_timer as usize as u64);
        idt[33] = gate(logos_isr_keyboard as usize as u64);
        idt[36] = gate(logos_isr_serial as usize as u64);
        let ptr = Pointer { limit: (core::mem::size_of::<[Gate; 256]>() - 1) as u16, base: idt.as_ptr() as u64 };
        asm!("lidt [{}]", in(reg) &ptr, options(nostack));
    }
}

// ---------------------------------------------------------------- 8259 PIC and 8253 PIT

fn pic_init() {
    // Remap IRQ 0-15 to vectors 32-47, then unmask only the timer (IRQ0) and keyboard (IRQ1).
    outb(0x20, 0x11);
    io_wait();
    outb(0xA0, 0x11);
    io_wait();
    outb(0x21, 32);
    io_wait();
    outb(0xA1, 40);
    io_wait();
    outb(0x21, 4);
    io_wait();
    outb(0xA1, 2);
    io_wait();
    outb(0x21, 0x01);
    io_wait();
    outb(0xA1, 0x01);
    io_wait();
    outb(0x21, !0b0001_0011); // timer, keyboard, COM1
    outb(0xA1, 0xFF);
}

fn pit_init() {
    let divisor = (1_193_182 / TICK_HZ) as u16;
    outb(0x43, 0x36); // channel 0, lobyte/hibyte, square wave
    outb(0x40, divisor as u8);
    outb(0x40, (divisor >> 8) as u8);
}

/// Take the CPU: our segments, our interrupt table, our timer and keyboard interrupts.
pub unsafe fn take_over() {
    unsafe {
        asm!("cli", options(nomem, nostack));
        load_gdt();
        load_idt();
    }
    pic_init();
    pit_init();
    crate::kbd::controller_init();
    outb(COM1 + 4, 0x0B); // OUT2 routes the UART interrupt to the PIC
    outb(COM1 + 1, 0x01); // interrupt when data arrives
    unsafe { asm!("sti", options(nomem, nostack)) };
}

/// Sleep until the next interrupt.
pub fn wait() {
    unsafe { asm!("hlt", options(nomem, nostack)) }
}
