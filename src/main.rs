#![no_std]
#![no_main]

extern crate alloc;

mod arch;
mod brain;
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

use brain::Brain;
use transformer::RunState;
use voice::{synthesize_vowel, VOWEL_O};

/// The dreamscape is computed at 1/SCALE resolution and blown up, so it can animate in real time.
const SCALE: usize = 4;
/// Our own heap: carved out of RAM before the firmware leaves, then managed by us.
const HEAP_BYTES: usize = 192 * 1024 * 1024;

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

/// What the mind's state looks like: these four numbers drive the dreamscape.
#[derive(Clone, Copy)]
struct Mood {
    energy: f32,  // ring speed and wave density
    valence: f32, // warm (positive) or cool (negative)
    swirl: f32,   // how hard the spiral twists
    pulse: f32,   // flash on each keystroke or word, decays every frame
}

fn mood_of(st: &RunState) -> Mood {
    let squash = |v: f32| v / (1.0 + libm::fabsf(v));
    Mood {
        energy: 0.6 + 0.8 * libm::fabsf(squash(st.x[0])),
        valence: squash(st.x[1]),
        swirl: 2.0 + 3.0 * libm::fabsf(squash(st.x[2])),
        pulse: 1.0,
    }
}

/// Wrap text into lines of at most `width` characters.
fn wrap(s: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in s.split(' ') {
        if !cur.is_empty() && cur.len() + 1 + word.len() > width {
            lines.push(core::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
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
    info!("logOS v0.5.0: Silicon-Embodied Intelligence Engine");
    info!("Waking consciousness directly on bare silicon...");
    info!("==================================================");

    // Read the brain off the boot drive while the firmware can still read files for us.
    let brain = match (brain::read_file(cstr16!("\\models\\stories15M.bin")), brain::read_file(cstr16!("\\models\\tokenizer.bin"))) {
        (Some(model), Some(tok)) => match Brain::from_files(brain::words(model), &tok) {
            Some(b) => b,
            None => {
                info!("model files found but unreadable; using the untrained brain");
                Brain::micro()
            }
        },
        _ => {
            info!("no model on the boot drive (models/stories15M.bin); using the untrained brain");
            Brain::micro()
        }
    };
    let cfg = brain.cfg;
    info!(
        "Neural Engine: dim={}, layers={}, heads={}, vocab={}, context={} ({})",
        cfg.dim, cfg.n_layers, cfg.n_heads, cfg.vocab_size, cfg.seq_len,
        if brain.trained { "trained: TinyStories 15M" } else { "untrained" }
    );
    let mut state = RunState::new(&cfg);
    brain.step(1, 0, &mut state);
    let mut target = mood_of(&state);
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
    let mut reply = String::from(if brain.trained {
        "I'm awake. Start a story and press Enter."
    } else {
        "No model on the boot drive yet. Type, and watch me react."
    });
    // Generation in progress: prompt tokens still to feed, position, last token, tokens produced.
    let mut feed: Vec<usize> = Vec::new();
    let mut fed = 0usize;
    let mut pos = 0usize;
    let mut prev = 1usize;
    let mut generating = false;
    let mut gen_start = 0u64;
    let mut produced = 0usize;
    let mut rng = 0x2545_F491_4F6C_DD1Du64;
    let mut frames: u64 = 0;
    let cols = (w.saturating_sub(48)) / 16;

    loop {
        while let Some(sc) = arch::next_scancode() {
            let Some(c) = keyboard.feed(sc) else { continue };
            match c {
                '\n' => {
                    if !line.is_empty() {
                        info!("heard: {}", line);
                        if brain.trained {
                            feed = brain.encode(&line);
                            fed = 0;
                            pos = 0;
                            state = RunState::new(&cfg);
                            produced = 0;
                            generating = true;
                            gen_start = arch::TICKS.load(Ordering::Relaxed);
                            rng ^= arch::TICKS.load(Ordering::Relaxed).wrapping_mul(0x9E37_79B9);
                            reply = line.clone();
                        } else {
                            brain.step(line.len() % cfg.vocab_size, pos % cfg.seq_len, &mut state);
                            pos += 1;
                            target = mood_of(&state);
                            reply = core::mem::take(&mut line);
                        }
                        line.clear();
                    }
                }
                '\u{8}' => {
                    line.pop();
                }
                c if (' '..='~').contains(&c) && line.len() < 60 => line.push(c),
                _ => {}
            }
            mood.pulse = 1.0;
        }

        // Think a little every frame so the screen never freezes.
        if generating {
            let start = arch::TICKS.load(Ordering::Relaxed);
            while generating && arch::TICKS.load(Ordering::Relaxed) < start + 4 {
                if fed < feed.len() {
                    brain.step(feed[fed], pos, &mut state);
                    prev = feed[fed];
                    fed += 1;
                    pos += 1;
                    if fed < feed.len() {
                        continue;
                    }
                }
                let next = brain::sample(&mut state.logits, 0.8, 0.9, &mut rng);
                if Brain::is_end(next) || pos >= cfg.seq_len || produced >= 160 {
                    generating = false;
                    let secs = (arch::TICKS.load(Ordering::Relaxed) - gen_start) as f32 / arch::TICK_HZ as f32;
                    info!("said ({} tokens in {:.1}s, {:.1} tok/s): {}", produced, secs, produced as f32 / secs.max(0.01), reply);
                    break;
                }
                reply.push_str(&brain.decode(prev, next));
                brain.step(next, pos, &mut state);
                prev = next;
                pos += 1;
                produced += 1;
                target = mood_of(&state);
                mood.pulse = mood.pulse.max(0.5);
            }
        }

        let ticks = arch::TICKS.load(Ordering::Relaxed);
        let time = if timer_ok { ticks as f32 / arch::TICK_HZ as f32 } else { frames as f32 * 0.016 };
        let p = mood.pulse;
        mood = Mood { energy: mood.energy + (target.energy - mood.energy) * 0.05, valence: mood.valence + (target.valence - mood.valence) * 0.05, swirl: mood.swirl + (target.swirl - mood.swirl) * 0.05, pulse: p * 0.92 };
        render(&mut low, lw, lh, time, mood);
        upscale(&low, lw, lh, &mut frame, w);
        text(&mut frame, w, h, 24, 20, 2, "logOS");
        let mut status = String::new();
        let _ = core::fmt::write(
            &mut status,
            format_args!(
                "no firmware  ram {} MiB  up {}s  brain {}  thoughts {}{}",
                usable / (1024 * 1024), ticks / arch::TICK_HZ,
                if brain.trained { "TinyStories-15M" } else { "untrained" }, pos,
                if generating { "  thinking..." } else { "" }
            ),
        );
        text(&mut frame, w, h, 24, 44, 1, &status);
        let lines = wrap(&reply, cols.max(10));
        let shown = &lines[lines.len().saturating_sub(10)..];
        for (i, l) in shown.iter().enumerate() {
            text(&mut frame, w, h, 24, h - 72 - (shown.len() - 1 - i) * 22, 2, l);
        }
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

        if timer_ok {
            let next = arch::TICKS.load(Ordering::Relaxed).max(ticks + 2);
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
