//! The software half of the volume story.
//!
//! Since milestone 10 the speaker-protecting ceiling lives in the ALSA
//! mixer (see `mixer`); the digital path runs at full scale. This module
//! is the belt to that braces: `gain` is the only way a volume becomes a
//! sample multiplier, and `apply_gain` clamps so software can attenuate
//! but never amplify.

/// Converts a volume (0-100) into a sample multiplier. Muted always wins.
pub fn gain(volume: u8, muted: bool) -> f32 {
    if muted {
        0.0
    } else {
        f32::from(volume.min(100)) / 100.0
    }
}

/// Scales samples in place. The only place gain touches audio; the pipeline
/// applies this to every buffer before it reaches a sink.
pub fn apply_gain(samples: &mut [i16], gain: f32) {
    // gain is always in 0.0..=1.0 (see gain()), but clamp defensively so a
    // bug upstream can attenuate, never amplify.
    let gain = gain.clamp(0.0, 1.0);
    for sample in samples {
        *sample = (f32::from(*sample) * gain) as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_is_linear_fraction() {
        assert_eq!(gain(0, false), 0.0);
        assert_eq!(gain(25, false), 0.25);
        assert_eq!(gain(50, false), 0.5);
        assert_eq!(gain(100, false), 1.0);
    }

    #[test]
    fn gain_muted_overrides_volume() {
        assert_eq!(gain(50, true), 0.0);
        assert_eq!(gain(100, true), 0.0);
    }

    #[test]
    fn gain_never_exceeds_one() {
        assert_eq!(gain(255, false), 1.0);
        for volume in 0..=u8::MAX {
            for muted in [false, true] {
                let g = gain(volume, muted);
                assert!((0.0..=1.0).contains(&g), "volume {volume} -> {g}");
            }
        }
    }

    #[test]
    fn gain_is_monotonic_in_volume() {
        let mut previous = gain(0, false);
        for volume in 1..=100 {
            let current = gain(volume, false);
            assert!(current >= previous);
            previous = current;
        }
    }

    #[test]
    fn apply_gain_scales_samples() {
        let mut samples = [10000, -10000, i16::MAX, i16::MIN, 0];
        apply_gain(&mut samples, 0.5);
        assert_eq!(samples, [5000, -5000, 16383, -16384, 0]);
    }

    #[test]
    fn apply_gain_zero_silences() {
        let mut samples = [i16::MAX, i16::MIN, 123];
        apply_gain(&mut samples, 0.0);
        assert_eq!(samples, [0, 0, 0]);
    }

    #[test]
    fn apply_gain_one_is_identity() {
        let mut samples = [i16::MAX, i16::MIN, 123, -456];
        apply_gain(&mut samples, 1.0);
        assert_eq!(samples, [i16::MAX, i16::MIN, 123, -456]);
    }

    #[test]
    fn apply_gain_never_amplifies() {
        // The software invariant: no gain value — in range or out — may
        // make any sample louder than it came in.
        let mut samples = [10000, -10000];
        apply_gain(&mut samples, 2.0); // out-of-range gain is clamped to 1.0
        assert_eq!(samples, [10000, -10000]);

        for g in [f32::INFINITY, 1.5, 100.0] {
            let mut samples = [i16::MAX, i16::MIN, 1234, -1234];
            let before = samples;
            apply_gain(&mut samples, g);
            for (after, before) in samples.iter().zip(before.iter()) {
                assert!(i32::from(*after).abs() <= i32::from(*before).abs());
            }
        }
    }

    #[test]
    fn apply_gain_peak_respects_the_volume() {
        // Full-scale input at volume 50 never exceeds half of full scale.
        let g = gain(50, false);
        let mut samples = [i16::MAX; 64];
        apply_gain(&mut samples, g);
        let bound = (0.5 * f32::from(i16::MAX)) as i32;
        for sample in samples {
            assert!(i32::from(sample).abs() <= bound);
        }
    }
}
