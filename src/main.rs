#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod kbd;
mod transformer;
mod voice;

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use font8x8::legacy::BASIC_LEGACY;
use linked_list_allocator::LockedHeap;
use log::info;
use uefi::boot::{self, AllocateType};
use uefi::mem::memory_map::{MemoryMap, MemoryType};
use uefi::prelude::*;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};

use transformer::{ModelConfig, RunState, transformer_step};
use voice::{synthesize_vowel, VOWEL_O};

/// The dreamscape is computed at 1/SCALE resolution and blown up, so it can animate in real time.
const SCALE: usize = 4;
/// Our own heap: carved out of RAM before the firmware leaves, then managed by us.
const HEAP_BYTES: usize = 32 * 1024 * 1024;

#[global_allocator]
static HEAP: LockedHeap = LockedHeap::empty();

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    log::error!("panic: {}", info);
    arch::halt_forever()
}

/// A pixel as the display hardware wants it.
#[derive(Clone, Copy)]
pub struct Px(u32);

static mut BGR: bool = true;

impl Px {
    fn new(r: u8, g: u8, b: u8) -> Px {
        if unsafe { BGR } {
            Px((r as u32) << 16 | (g as u32) << 8 | b as u32)
        } else {
            Px((b as u32) << 16 | (g as u32) << 8 | r as u32)
        }
    }
}

/// Weights for the network. Seeded pseudo-randomly (not trained yet) so different input really
/// produces different states. Real weights loaded from the boot drive are milestone M4.
struct Weights {
    embed: Vec<f32>,
    rms_att: Vec<f32>,
    wq: Vec<f32>,
    wk: Vec<f32>,
    wv: Vec<f32>,
    wo: Vec<f32>,
    rms_ffn: Vec<f32>,
    w1: Vec<f32>,
    w2: Vec<f32>,
    w3: Vec<f32>,
    rms_final: Vec<f32>,
    w_cls: Vec<f32>,
}

fn seeded(len: usize, seed: &mut u64, scale: f32) -> Vec<f32> {
    let mut v = vec![0.0f32; len];
    for x in v.iter_mut() {
        // xorshift64
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *x = ((*seed >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0) * scale;
    }
    v
}

impl Weights {
    fn new(cfg: &ModelConfig) -> Self {
        let mut s = 0x9E37_79B9_7F4A_7C15u64;
        let (d, h, l) = (cfg.dim, cfg.hidden_dim, cfg.n_layers);
        let k = 1.0 / libm::sqrtf(d as f32);
        Weights {
            embed: seeded(cfg.vocab_size * d, &mut s, 1.0),
            rms_att: vec![1.0; l * d],
            wq: seeded(l * d * d, &mut s, k),
            wk: seeded(l * d * d, &mut s, k),
            wv: seeded(l * d * d, &mut s, k),
            wo: seeded(l * d * d, &mut s, k),
            rms_ffn: vec![1.0; l * d],
            w1: seeded(l * h * d, &mut s, k),
            w2: seeded(l * d * h, &mut s, 1.0 / libm::sqrtf(h as f32)),
            w3: seeded(l * h * d, &mut s, k),
            rms_final: vec![1.0; d],
            w_cls: seeded(cfg.vocab_size * d, &mut s, k),
        }
    }
}

/// What the mind's state looks like: these four numbers drive the dreamscape.
#[derive(Clone, Copy)]
struct Mood {
    energy: f32,  // ring speed and wave density
    valence: f32, // warm (positive) or cool (negative)
    swirl: f32,   // how hard the spiral twists
    pulse: f32,   // flash on each keystroke, decays every frame
}

fn think(token: usize, pos: &mut usize, cfg: &ModelConfig, st: &mut RunState, w: &Weights) -> Mood {
    if *pos >= cfg.seq_len {
        *st = RunState::new(cfg);
        *pos = 0;
    }
    transformer_step(
        token % cfg.vocab_size, *pos, cfg, st, &w.embed, &w.rms_att, &w.wq, &w.wk, &w.wv, &w.wo,
        &w.rms_ffn, &w.w1, &w.w2, &w.w3, &w.rms_final, &w.w_cls,
    );
    *pos += 1;
    let squash = |v: f32| v / (1.0 + libm::fabsf(v));
    Mood {
        energy: 0.6 + 0.8 * libm::fabsf(squash(st.x[0])),
        valence: squash(st.x[1]),
        swirl: 2.0 + 3.0 * libm::fabsf(squash(st.x[2])),
        pulse: 1.0,
    }
}

fn mix(a: Mood, b: Mood, t: f32) -> Mood {
    let l = |x: f32, y: f32| x + (y - x) * t;
    Mood { energy: l(a.energy, b.energy), valence: l(a.valence, b.valence), swirl: l(a.swirl, b.swirl), pulse: a.pulse }
}

fn render(low: &mut [Px], lw: usize, lh: usize, time: f32, m: Mood) {
    let aspect = lw as f32 / lh as f32;
    for y in 0..lh {
        let ty = (y as f32 / lh as f32) * 2.0 - 1.0;
        for x in 0..lw {
            let tx = ((x as f32 / lw as f32) * 2.0 - 1.0) * aspect;
            let r = libm::sqrtf(tx * tx + ty * ty);
            let a = libm::atan2f(ty, tx);
            let wave = libm::sinf(a * 4.0 + r * 8.0 * m.swirl * 0.4 - time * 1.5 * m.energy);
            let ring = 0.4 + 0.04 * libm::sinf(time * 2.0 * m.energy) + 0.05 * m.pulse;
            let glow = (0.08 + 0.12 * m.pulse) / (libm::fabsf(r - ring) + 0.05);
            let warm = m.valence.max(0.0);
            let cool = (-m.valence).max(0.0);
            let red = 0.15 + 0.45 * warm + 0.25 * wave.abs() * warm + glow * 0.5;
            let green = 0.25 + 0.30 * wave.abs() + glow * 0.4;
            let blue = 0.55 + 0.30 * cool + 0.15 * wave + glow * 0.8;
            let c = |v: f32| (v * 255.0).clamp(0.0, 255.0) as u8;
            low[y * lw + x] = Px::new(c(red), c(green), c(blue));
        }
    }
}

fn upscale(low: &[Px], lw: usize, lh: usize, out: &mut [Px], w: usize) {
    for y in 0..lh * SCALE {
        let row = &low[(y / SCALE) * lw..(y / SCALE) * lw + lw];
        let dst = &mut out[y * w..y * w + lw * SCALE];
        for (x, p) in dst.iter_mut().enumerate() {
            *p = row[x / SCALE];
        }
    }
}

/// Draw text with the 8x8 font, scaled `s` times, with a dark shadow so it reads on any colour.
fn text(out: &mut [Px], w: usize, h: usize, x0: usize, y0: usize, s: usize, msg: &str) {
    let white = Px::new(255, 255, 255);
    let shadow = Px::new(20, 10, 40);
    for (i, ch) in msg.chars().enumerate() {
        let glyph = BASIC_LEGACY[(ch as usize).min(127)];
        for (gy, bits) in glyph.iter().enumerate() {
            for gx in 0..8 {
                if bits & (1 << gx) == 0 {
                    continue;
                }
                for (dx, dy, color) in [(1, 1, shadow), (0, 0, white)] {
                    for sy in 0..s {
                        for sx in 0..s {
                            let px = x0 + (i * 8 + gx) * s + sx + dx;
                            let py = y0 + gy * s + sy + dy;
                            if px < w && py < h {
                                out[py * w + px] = color;
                            }
                        }
                    }
                }
            }
        }
    }
}

#[entry]
fn main() -> Status {
    arch::logger_init();
    // Carve out our heap before anything allocates.
    let heap = boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, HEAP_BYTES / 4096)
        .expect("no memory for the heap");
    unsafe { HEAP.lock().init(heap.as_ptr(), HEAP_BYTES) };

    info!("==================================================");
    info!("logOS v0.4.0: Silicon-Embodied Intelligence Engine");
    info!("Waking consciousness directly on bare silicon...");
    info!("==================================================");

    let cfg = ModelConfig::micro();
    info!("Allocating Neural Engine: dim={}, layers={}, heads={}, vocab={}", cfg.dim, cfg.n_layers, cfg.n_heads, cfg.vocab_size);
    let mut state = RunState::new(&cfg);
    let weights = Weights::new(&cfg);
    let mut pos = 0usize;
    let mut target = think(42, &mut pos, &cfg, &mut state, &weights);
    let mut mood = target;
    let vocal_stream = synthesize_vowel(VOWEL_O, 0.4, 130.81);
    info!("Vocal stream synthesized: {} PCM samples (no audio driver yet: M3).", vocal_stream.len());

    // Learn where the screen lives, then let go of the firmware's graphics driver.
    let (w, h, stride, fb) = {
        let handle = boot::get_handle_for_protocol::<GraphicsOutput>().expect("no GOP");
        let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(handle).expect("GOP open failed");
        let mode = gop.current_mode_info();
        unsafe { BGR = !matches!(mode.pixel_format(), PixelFormat::Rgb) };
        let (w, h) = mode.resolution();
        (w, h, mode.stride(), gop.frame_buffer().as_mut_ptr() as *mut u32)
    };

    // The firmware leaves. From here on every byte of this machine is ours.
    info!("Exiting firmware boot services...");
    let mmap = unsafe { boot::exit_boot_services(Some(MemoryType::LOADER_DATA)) };
    let usable: u64 = mmap
        .entries()
        .filter(|d| matches!(d.ty, MemoryType::CONVENTIONAL | MemoryType::BOOT_SERVICES_CODE | MemoryType::BOOT_SERVICES_DATA))
        .map(|d| d.page_count * 4096)
        .sum();
    unsafe { arch::take_over() };
    info!("logOS owns the machine: firmware gone, own GDT/IDT, PIT timer at {} Hz, PS/2 keyboard. RAM: {} MiB usable.", arch::TICK_HZ, usable / (1024 * 1024));

    // Is the timer really ticking? If not, fall back to counting frames ourselves.
    let mut spin = 0u64;
    while arch::TICKS.load(Ordering::Relaxed) == 0 && spin < 200_000_000 {
        core::hint::spin_loop();
        spin += 1;
    }
    let timer_ok = arch::TICKS.load(Ordering::Relaxed) > 0;
    if !timer_ok {
        info!("warning: no timer interrupts arrived; pacing frames by spinning");
    }

    let (lw, lh) = (w / SCALE, h / SCALE);
    let mut low = vec![Px(0); lw * lh];
    let mut frame = vec![Px(0); w * h];
    info!("Dreamscape live at {}x{} (rendered at {}x{}). Type to it.", w, h, lw, lh);

    let mut keyboard = kbd::Keyboard::new();
    let mut line = String::new();
    let mut last = String::from("type something and press Enter");
    let mut frames: u64 = 0;

    loop {
        while let Some(sc) = arch::next_scancode() {
            let Some(c) = keyboard.feed(sc) else { continue };
            match c {
                '\n' => {
                    if !line.is_empty() {
                        info!("heard: {}", line);
                        last = core::mem::take(&mut line);
                    }
                }
                '\u{8}' => {
                    line.pop();
                }
                c if (' '..='~').contains(&c) && line.len() < 60 => line.push(c),
                _ => {}
            }
            target = think(c as usize, &mut pos, &cfg, &mut state, &weights);
            mood.pulse = 1.0;
        }

        let ticks = arch::TICKS.load(Ordering::Relaxed);
        let time = if timer_ok { ticks as f32 / arch::TICK_HZ as f32 } else { frames as f32 * 0.016 };
        mood = mix(mood, target, 0.05);
        mood.pulse *= 0.92;
        render(&mut low, lw, lh, time, mood);
        upscale(&low, lw, lh, &mut frame, w);
        text(&mut frame, w, h, 24, 20, 2, "logOS");
        let mut status = String::new();
        let _ = core::fmt::write(
            &mut status,
            format_args!(
                "no firmware  ram {} MiB  up {}s  energy {:.2}  valence {:+.2}  thoughts {}",
                usable / (1024 * 1024), ticks / arch::TICK_HZ, mood.energy, mood.valence, pos
            ),
        );
        text(&mut frame, w, h, 24, 44, 1, &status);
        text(&mut frame, w, h, 24, h - 72, 2, &last);
        let mut prompt = String::from("> ");
        prompt.push_str(&line);
        if (frames / 30) % 2 == 0 {
            prompt.push('_');
        }
        text(&mut frame, w, h, 24, h - 40, 2, &prompt);

        // Straight into video memory, row by row.
        for y in 0..h {
            unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr().add(y * w) as *const u32, fb.add(y * stride), w) };
        }
        if frames == 0 {
            info!("logOS entity active. Physical presence established.");
        }
        frames += 1;

        // Sleep until the next tick (~60 fps at 100 Hz is two ticks per frame at worst).
        if timer_ok {
            let next = ticks + 2;
            while arch::TICKS.load(Ordering::Relaxed) < next {
                arch::wait();
            }
        } else {
            for _ in 0..2_000_000 {
                core::hint::spin_loop();
            }
        }
    }
}
