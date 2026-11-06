//! logOS Voice Engine
//! Bare-metal harmonic formant synthesizer.
//! Generates raw PCM audio samples directly into DMA memory buffers without external libraries.

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;
use libm::sinf;

pub const SAMPLE_RATE: usize = 22050;

/// Acoustic Formant configuration for vowels
#[derive(Clone, Copy)]
pub struct FormantConfig {
    pub f1: f32, // First formant frequency (Hz)
    pub f2: f32, // Second formant frequency (Hz)
    pub f3: f32, // Third formant frequency (Hz)
}

pub const VOWEL_A: FormantConfig = FormantConfig { f1: 800.0, f2: 1200.0, f3: 2500.0 }; // "ah"
pub const VOWEL_E: FormantConfig = FormantConfig { f1: 400.0, f2: 2000.0, f3: 2800.0 }; // "eh"
pub const VOWEL_I: FormantConfig = FormantConfig { f1: 300.0, f2: 2300.0, f3: 3000.0 }; // "ee"
pub const VOWEL_O: FormantConfig = FormantConfig { f1: 500.0, f2: 1000.0, f3: 2500.0 }; // "oh"
pub const VOWEL_U: FormantConfig = FormantConfig { f1: 350.0, f2: 800.0,  f3: 2400.0 }; // "oo"

/// Synthesize a spoken vowel phoneme into raw 16-bit PCM audio samples
pub fn synthesize_vowel(vowel: FormantConfig, duration_secs: f32, pitch_hz: f32) -> Vec<i16> {
    let num_samples = (SAMPLE_RATE as f32 * duration_secs) as usize;
    let mut buffer = vec![0i16; num_samples];

    let mut phase_f0 = 0.0f32;
    let mut phase_f1 = 0.0f32;
    let mut phase_f2 = 0.0f32;
    let mut phase_f3 = 0.0f32;

    for i in 0..num_samples {
        let t = i as f32 / SAMPLE_RATE as f32;

        // Envelope: soft attack and gentle release
        let attack = (t / 0.04).min(1.0);
        let release = ((duration_secs - t) / 0.06).min(1.0).max(0.0);
        let envelope = attack * release;

        // Glottal pulse excitation (fundamental pitch with subtle organic vibrato)
        let vibrato = 1.0 + 0.015 * sinf(t * 31.4159); // 5Hz vibrato
        let current_pitch = pitch_hz * vibrato;
        phase_f0 += current_pitch / SAMPLE_RATE as f32;
        if phase_f0 > 1.0 { phase_f0 -= 1.0; }

        let glottal_pulse = sinf(phase_f0 * 6.2831853) + 0.4 * sinf(phase_f0 * 12.56637);

        // Resonating formants
        phase_f1 += vowel.f1 / SAMPLE_RATE as f32;
        phase_f2 += vowel.f2 / SAMPLE_RATE as f32;
        phase_f3 += vowel.f3 / SAMPLE_RATE as f32;

        let s1 = sinf(phase_f1 * 6.2831853) * 0.50;
        let s2 = sinf(phase_f2 * 6.2831853) * 0.35;
        let s3 = sinf(phase_f3 * 6.2831853) * 0.15;

        let voice_sample = glottal_pulse * (s1 + s2 + s3) * envelope;
        buffer[i] = (voice_sample * 24000.0).clamp(-32767.0, 32767.0) as i16;
    }

    buffer
}
