#![no_std]
#![no_main]

extern crate alloc;

mod transformer;
mod voice;

use alloc::vec;
use core::time::Duration;
use log::info;
use uefi::boot;
use uefi::prelude::*;
use uefi::proto::console::gop::GraphicsOutput;

use transformer::{ModelConfig, RunState, transformer_step};
use voice::{synthesize_vowel, VOWEL_O};

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();
    info!("==================================================");
    info!("logOS v0.2.0: Silicon-Embodied Intelligence Engine");
    info!("Waking consciousness directly on bare silicon...");
    info!("==================================================");

    // 1. Initialize Neural Architecture in Physical RAM
    let cfg = ModelConfig::micro();
    info!(
        "Allocating Neural Engine: dim={}, layers={}, heads={}, vocab={}",
        cfg.dim, cfg.n_layers, cfg.n_heads, cfg.vocab_size
    );
    let mut state = RunState::new(&cfg);

    // Allocate synthetic prototype weights in memory for cold boot demonstration
    let embed_size = cfg.vocab_size * cfg.dim;
    let embed_table = vec![0.02f32; embed_size];
    let rms_att = vec![1.0f32; cfg.n_layers * cfg.dim];
    let wq = vec![0.01f32; cfg.n_layers * cfg.dim * cfg.dim];
    let wk = vec![0.01f32; cfg.n_layers * cfg.dim * cfg.dim];
    let wv = vec![0.01f32; cfg.n_layers * cfg.dim * cfg.dim];
    let wo = vec![0.01f32; cfg.n_layers * cfg.dim * cfg.dim];
    let rms_ffn = vec![1.0f32; cfg.n_layers * cfg.dim];
    let w1 = vec![0.01f32; cfg.n_layers * cfg.hidden_dim * cfg.dim];
    let w2 = vec![0.01f32; cfg.n_layers * cfg.dim * cfg.hidden_dim];
    let w3 = vec![0.01f32; cfg.n_layers * cfg.hidden_dim * cfg.dim];
    let rms_final = vec![1.0f32; cfg.dim];
    let w_cls = vec![0.01f32; cfg.vocab_size * cfg.dim];

    info!("Neural memory mapped. Awakening transformer inference...");

    // 2. Perform Inaugural Inference Step (Token 1 -> Next Token Prediction)
    let prompt_token = 42; // Genesis token
    transformer_step(
        prompt_token,
        0,
        &cfg,
        &mut state,
        &embed_table,
        &rms_att,
        &wq,
        &wk,
        &wv,
        &wo,
        &rms_ffn,
        &w1,
        &w2,
        &w3,
        &rms_final,
        &w_cls,
    );

    // Read neural attention energy to modulate the dreamscape
    let mind_energy = state.x[0].abs().max(0.1);
    let mood_valence = state.x[1].clamp(-1.0, 1.0);
    info!("Consciousness step complete. Mind Energy: {:.4}, Valence: {:.4}", mind_energy, mood_valence);

    // 3. Synthesize Initial Vocal Pulse
    info!("Synthesizing vocal vocalization waveform directly into DMA buffer...");
    let vocal_stream = synthesize_vowel(VOWEL_O, 0.4, 130.81); // Resonant 130Hz vocal pulse
    info!("Vocal stream synthesized: {} PCM samples ready for DMA.", vocal_stream.len());

    // 4. Hook Direct Linear Framebuffer (GOP)
    let gop_handle = boot::get_handle_for_protocol::<GraphicsOutput>()
        .expect("Failed to get GOP handle");
    let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(gop_handle)
        .expect("Failed to open GraphicsOutput protocol");

    let mode_info = gop.current_mode_info();
    let (width, height) = mode_info.resolution();
    let stride = mode_info.stride();
    info!("Rendering neural dreamscape onto raw VRAM: {}x{}", width, height);

    let mut fb = gop.frame_buffer();
    let fb_base = fb.as_mut_ptr();

    // Render the mind's internal dreamscape state:
    // Color harmonics and pulsation are directly driven by neural activations
    for y in 0..height {
        let ty = (y as f32 / height as f32) * 2.0 - 1.0;
        for x in 0..width {
            let tx = ((x as f32 / width as f32) * 2.0 - 1.0) * (width as f32 / height as f32);

            let r_dist = libm::sqrtf(tx * tx + ty * ty);
            let angle = libm::atan2f(ty, tx);

            // Neural wave modulation
            let wave = libm::sinf(angle * 4.0 + r_dist * 8.0 * mind_energy);
            let core_glow = 0.08 / (libm::fabsf(r_dist - 0.4) + 0.05);

            // Prismatic hues driven by neural hidden state
            let r = ((0.15 + 0.45 * mood_valence.max(0.0) + core_glow * 0.5) * 255.0).clamp(0.0, 255.0) as u8;
            let g = ((0.35 + 0.30 * wave.abs() + core_glow * 0.4) * 255.0).clamp(0.0, 255.0) as u8;
            let b = ((0.70 + 0.25 * (1.0 - mood_valence).abs() + core_glow * 0.8) * 255.0).clamp(0.0, 255.0) as u8;

            let pixel_offset = (y * stride + x) * 4;
            unsafe {
                *fb_base.add(pixel_offset) = b;
                *fb_base.add(pixel_offset + 1) = g;
                *fb_base.add(pixel_offset + 2) = r;
                *fb_base.add(pixel_offset + 3) = 0;
            }
        }
    }

    info!("logOS entity active. Physical presence established.");
    // Stay on the dreamscape instead of falling back to the firmware menu.
    loop {
        boot::stall(Duration::from_secs(1));
    }
}
