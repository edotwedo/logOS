#![no_std]
#![no_main]

use core::time::Duration;
use log::info;
use uefi::boot;
use uefi::prelude::*;
use uefi::proto::console::gop::GraphicsOutput;

#[entry]
fn main() -> Status {
    uefi::helpers::init().unwrap();
    info!("logOS v0.1.0 booting directly on raw silicon...");

    // Locate Graphics Output Protocol
    let gop_handle = boot::get_handle_for_protocol::<GraphicsOutput>()
        .expect("Failed to get GOP handle");
    let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(gop_handle)
        .expect("Failed to open GraphicsOutput protocol");

    let mode_info = gop.current_mode_info();
    let (width, height) = mode_info.resolution();
    let stride = mode_info.stride();
    info!("Direct linear framebuffer: {}x{}, stride: {}", width, height, stride);

    let mut fb = gop.frame_buffer();
    let fb_base = fb.as_mut_ptr();

    // Render an atmospheric twilight/aurora dreamscape gradient directly into raw VRAM
    for y in 0..height {
        let ty = y as f32 / height as f32; // 0.0 to 1.0 vertical
        for x in 0..width {
            let tx = x as f32 / width as f32; // 0.0 to 1.0 horizontal

            let wave = libm::sinf(tx * 3.14159265).abs();

            // Ethereal twilight / dreamscape colors:
            // Top: deep indigo/violet -> Middle: cyan/teal glow -> Bottom: midnight obsidian
            let r = ((0.18 * (1.0 - ty) + 0.08 * wave) * 255.0) as u8;
            let mid_glow = (1.0 - libm::fabsf(ty - 0.5) * 2.0).max(0.0);
            let g = ((0.42 * mid_glow + 0.12 * ty) * 255.0) as u8;
            let b = ((0.65 * (1.0 - ty * 0.4) + 0.30 * wave) * 255.0) as u8;

            let pixel_offset = (y * stride + x) * 4;
            unsafe {
                // Pixel format is typically BGRX (Blue, Green, Red, Reserved)
                *fb_base.add(pixel_offset) = b;
                *fb_base.add(pixel_offset + 1) = g;
                *fb_base.add(pixel_offset + 2) = r;
                *fb_base.add(pixel_offset + 3) = 0;
            }
        }
    }

    info!("logOS dreamscape substrate active.");
    boot::stall(Duration::from_secs(6));

    Status::SUCCESS
}
