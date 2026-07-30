//! The safety-critical volume module.
//!
//! `effective_volume` is the only way a requested volume becomes an effective
//! volume, and `gain` is the only way an effective volume becomes a sample
//! multiplier. All gain applied to audio must flow through these functions.

/// Maps a requested volume (0-100, a percentage of `max_volume`) onto the
/// device scale: 100 means `max_volume`, 50 means half of it. The result can
/// never exceed `max_volume` — the scale itself makes overshoot impossible.
pub fn effective_volume(requested: u8, max_volume: u8) -> u8 {
    // Callers validate requested <= 100; cap defensively anyway.
    let requested = u32::from(requested.min(100));
    let scaled = (requested * u32::from(max_volume) + 50) / 100;
    scaled as u8
}

/// Converts an effective volume into a sample multiplier. Muted always wins.
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
    fn effective_volume_is_a_percentage_of_max() {
        assert_eq!(effective_volume(100, 50), 50);
        assert_eq!(effective_volume(50, 50), 25);
        assert_eq!(effective_volume(80, 50), 40);
        assert_eq!(effective_volume(100, 30), 30);
        assert_eq!(effective_volume(50, 30), 15);
        assert_eq!(effective_volume(10, 50), 5);
    }

    #[test]
    fn effective_volume_rounds_to_nearest() {
        assert_eq!(effective_volume(25, 50), 13); // 12.5 rounds up
        assert_eq!(effective_volume(24, 50), 12);
        assert_eq!(effective_volume(1, 30), 0); // 0.3 rounds down
        assert_eq!(effective_volume(5, 30), 2); // 1.5 rounds up
    }

    #[test]
    fn effective_volume_zero() {
        assert_eq!(effective_volume(0, 50), 0);
    }

    #[test]
    fn effective_volume_with_zero_max_is_silent() {
        assert_eq!(effective_volume(100, 0), 0);
        assert_eq!(effective_volume(50, 0), 0);
    }

    #[test]
    fn effective_volume_never_exceeds_max_for_any_input() {
        for max_volume in [0u8, 1, 30, 50, 100] {
            for requested in 0..=u8::MAX {
                let effective = effective_volume(requested, max_volume);
                assert!(
                    effective <= max_volume,
                    "requested {requested} max {max_volume} -> {effective}"
                );
            }
        }
    }

    #[test]
    fn effective_volume_is_monotonic_in_request() {
        let mut previous = effective_volume(0, 50);
        for requested in 1..=100 {
            let current = effective_volume(requested, 50);
            assert!(current >= previous);
            previous = current;
        }
    }

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
    }

    #[test]
    fn gain_through_scaling_never_exceeds_max() {
        for max_volume in [0u8, 30, 50, 100] {
            for requested in 0..=u8::MAX {
                let g = gain(effective_volume(requested, max_volume), false);
                assert!(g <= f32::from(max_volume) / 100.0);
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
        let mut samples = [10000, -10000];
        apply_gain(&mut samples, 2.0); // out-of-range gain is clamped to 1.0
        assert_eq!(samples, [10000, -10000]);
    }

    #[test]
    fn apply_gain_peak_respects_max_volume() {
        // The milestone invariant: full-scale input through the clamp and
        // gain never exceeds max_volume percent of full scale.
        let max_volume = 50;
        let g = gain(effective_volume(100, max_volume), false);
        let mut samples = [i16::MAX; 64];
        apply_gain(&mut samples, g);
        let bound = (f32::from(max_volume) / 100.0 * f32::from(i16::MAX)) as i32;
        for sample in samples {
            assert!(i32::from(sample).abs() <= bound);
        }
    }
}
