//! Radio DSP chain: bandpass, hiss bed, crackle impulses, static bursts.
//!
//! Port of the gateway `_radio_effects` (numpy) to pure Rust. Same structure,
//! same levels, same seed-key scheme — but a different RNG stream (`PCG64` via
//! `rand_pcg` instead of numpy's), so output is deterministic per input without
//! being byte-identical to the gateway. Parity is perceptual, proven by ear.

use rand::{Rng as _, SeedableRng as _};
use rand_distr::{Distribution as _, StandardNormal};
use rustfft::{FftPlanner, num_complex::Complex};
use sha2::{Digest as _, Sha256};

/// FX levels, all 0..1 fractions except the bandpass switch.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fx {
    /// Gaussian bed scaled by signal RMS.
    pub hiss: f32,
    /// Random polarity impulses scaled by signal peak.
    pub crackle: f32,
    /// Exponentially decaying noise bursts scaled by signal peak.
    pub static_: f32,
    /// 300-3400 Hz soft-edge filter.
    pub bandpass: bool,
}

/// Apply the chain. All-zero levels with no bandpass return the input untouched.
///
/// Length casts below are safe: audio buffers never approach 2^52 samples.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
#[must_use]
pub fn apply_fx(samples: &[f32], rate: u32, fx: Fx, seed_key: &str) -> Vec<f32> {
    if samples.is_empty()
        || (fx.hiss <= 0.0 && fx.crackle <= 0.0 && fx.static_ <= 0.0 && !fx.bandpass)
    {
        return samples.to_vec();
    }
    let digest = Sha256::digest(seed_key.as_bytes());
    let seed = u64::from_le_bytes(digest[0..8].try_into().unwrap_or([0; 8]));
    let mut rng = rand_pcg::Pcg64Mcg::seed_from_u64(seed);
    let mut x: Vec<f64> = samples.iter().map(|sample| f64::from(*sample)).collect();
    if fx.bandpass && rate >= 8000 {
        bandpass(&mut x, f64::from(rate));
    }
    let peak = x.iter().fold(0.0f64, |max, sample| max.max(sample.abs())) + 1e-9;
    let rms = (x.iter().map(|sample| sample * sample).sum::<f64>() / x.len() as f64).sqrt() + 1e-9;
    if fx.hiss > 0.0 {
        let level = f64::from(fx.hiss) * rms * 0.35;
        for sample in &mut x {
            let noise: f64 = StandardNormal.sample(&mut rng);
            *sample += noise * level;
        }
    }
    if fx.crackle > 0.0 {
        let duration = x.len() as f64 / f64::from(rate);
        #[allow(clippy::cast_possible_truncation)]
        let count = (f64::from(fx.crackle) * duration * 25.0) as usize;
        for _ in 0..count {
            let index = rng.r#gen_range(0..x.len());
            let sign = if rng.r#gen_bool(0.5) { 1.0 } else { -1.0 };
            let magnitude = 0.25 + 0.5 * rng.r#gen::<f64>();
            x[index] += sign * magnitude * peak;
        }
    }
    if fx.static_ > 0.0 {
        for _ in 0..=((f64::from(fx.static_) * 3.0) as usize) {
            #[allow(clippy::cast_possible_truncation)]
            let length = (f64::from(rate) * (0.03 + 0.09 * rng.r#gen::<f64>())) as usize;
            let start = rng.r#gen_range(0..x.len().saturating_sub(length).max(1));
            for (offset, sample) in x.iter_mut().enumerate().take(start + length).skip(start) {
                let local = offset - start;
                let envelope = (-3.0 * local as f64 / length.max(1) as f64).exp();
                let noise: f64 = StandardNormal.sample(&mut rng);
                *sample += noise * envelope * (0.15 + 0.45 * f64::from(fx.static_)) * peak;
            }
        }
    }
    let limit = x.iter().fold(0.0f64, |max, sample| max.max(sample.abs()));
    if limit > 0.98 {
        let gain = 0.98 / limit;
        for sample in &mut x {
            *sample *= gain;
        }
    }
    x.into_iter().map(|sample| sample as f32).collect()
}

/// In-place soft-edge 300-3400 Hz filter via FFT, mirroring the gateway mask.
///
/// Length casts below are safe: audio buffers never approach 2^52 samples.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn bandpass(x: &mut [f64], rate: f64) {
    let mut planner = FftPlanner::new();
    let forward = planner.plan_fft_forward(x.len());
    let backward = planner.plan_fft_inverse(x.len());
    let mut spectrum: Vec<Complex<f64>> =
        x.iter().map(|sample| Complex::new(*sample, 0.0)).collect();
    forward.process(&mut spectrum);
    let bins = spectrum.len();
    for (index, bin) in spectrum.iter_mut().enumerate() {
        // Mirror-aware frequency: bins past Nyquist are negative frequencies,
        // unlike numpy rfft which only stores the positive half.
        let folded = if index <= bins / 2 {
            index
        } else {
            bins - index
        };
        let freq = folded as f64 * rate / x.len() as f64;
        let low = ((freq - 300.0 + 150.0) / 300.0).clamp(0.0, 1.0);
        let high = ((3400.0 + 400.0 - freq) / 800.0).clamp(0.0, 1.0);
        *bin *= low * high / bins as f64;
    }
    backward.process(&mut spectrum);
    for (index, sample) in x.iter_mut().enumerate() {
        *sample = spectrum[index].re;
    }
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
mod tests {
    use super::*;

    fn tone(freq: f64, rate: u32, seconds: f64) -> Vec<f32> {
        let count = (f64::from(rate) * seconds) as usize;
        (0..count)
            .map(|index| {
                (2.0 * std::f64::consts::PI * freq * index as f64 / f64::from(rate)).sin() as f32
            })
            .collect()
    }

    fn energy(samples: &[f32]) -> f64 {
        samples
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum()
    }

    #[test]
    fn silence_passes_through() {
        let fx = Fx {
            hiss: 0.0,
            crackle: 0.0,
            static_: 0.0,
            bandpass: false,
        };
        let input = tone(440.0, 24_000, 0.5);
        assert_eq!(apply_fx(&input, 24_000, fx, "key"), input);
    }

    #[test]
    fn deterministic_per_seed_key() {
        let fx = Fx {
            hiss: 0.5,
            crackle: 0.5,
            static_: 0.5,
            bandpass: true,
        };
        let input = tone(440.0, 24_000, 0.5);
        let first = apply_fx(&input, 24_000, fx, "same");
        let second = apply_fx(&input, 24_000, fx, "same");
        let other = apply_fx(&input, 24_000, fx, "different");
        assert_eq!(first, second);
        assert_ne!(first, other);
    }

    #[test]
    fn bandpass_kills_rumble_and_hiss_band() {
        let fx = Fx {
            hiss: 0.0,
            crackle: 0.0,
            static_: 0.0,
            bandpass: true,
        };
        let low = tone(100.0, 24_000, 0.5);
        let high = tone(6000.0, 24_000, 0.5);
        let mid = tone(1000.0, 24_000, 0.5);
        let low_out = energy(&apply_fx(&low, 24_000, fx, "k"));
        let high_out = energy(&apply_fx(&high, 24_000, fx, "k"));
        let mid_out = energy(&apply_fx(&mid, 24_000, fx, "k"));
        assert!(low_out < energy(&low) * 0.05, "rumble survives");
        assert!(high_out < energy(&high) * 0.05, "top octave survives");
        assert!(mid_out > energy(&mid) * 0.5, "passband damaged");
    }
}
