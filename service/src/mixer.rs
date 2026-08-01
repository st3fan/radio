//! ALSA mixer ownership: radiod sets the hardware output ceiling.
//!
//! The speaker-protection invariant lives here since milestone 10: the
//! mixer element named in `[mixer]` is set to the configured ceiling,
//! read back, and verified — and re-asserted at every playback session
//! start, so an external `alsamixer`, an `alsactl restore` at boot, or a
//! re-enumerating USB DAC cannot leave the output above the ceiling for
//! longer than the gap until the next session. The digital path runs at
//! full scale; its never-amplify clamp in `volume` is the other half of
//! the belt-and-braces.

use std::fmt;

use crate::config::MixerConfig;

#[derive(Debug)]
pub struct MixerError(pub String);

impl fmt::Display for MixerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for MixerError {}

/// Owns one mixer control. `assert_ceiling` sets the ceiling and verifies
/// it landed by reading back; it must be cheap enough to call at every
/// session start.
pub trait MixerControl: Send {
    fn assert_ceiling(&mut self) -> Result<(), MixerError>;
}

/// Derives the mixer device ("hw:N" / "hw:CARD=Name") from an ALSA PCM
/// device name like "plughw:1,0" or "hw:CARD=Device,DEV=0". The `[mixer]`
/// `device` field overrides this for PCM names it cannot parse.
pub fn mixer_device_for(audio_device: &str) -> Result<String, MixerError> {
    let (_, rest) = audio_device.split_once(':').ok_or_else(|| {
        MixerError(format!(
            "cannot derive a mixer device from audio_device {audio_device:?}; \
             set device explicitly in the [mixer] section (e.g. \"hw:0\")"
        ))
    })?;
    let card = rest.split(',').next().unwrap_or(rest);
    let card = card.strip_prefix("CARD=").unwrap_or(card);
    if card.is_empty() {
        return Err(MixerError(format!(
            "cannot derive a mixer device from audio_device {audio_device:?}; \
             set device explicitly in the [mixer] section (e.g. \"hw:0\")"
        )));
    }
    Ok(format!("hw:{card}"))
}

#[cfg(target_os = "linux")]
mod alsa_mixer {
    use super::{MixerControl, MixerError};
    use crate::config::{MixerCeiling, MixerConfig};
    use alsa::mixer::{MilliBel, Mixer, Selem, SelemChannelId, SelemId};

    /// How far below the requested dB ceiling the hardware may land: the
    /// element rounds down to its own step size (we ask for Floor), and
    /// steps are typically 0.5-1 dB. Landing above the ceiling is never
    /// accepted, whatever the step size.
    const DB_SLACK: MilliBel = MilliBel(300);

    pub struct AlsaMixer {
        device: String,
        config: MixerConfig,
        /// Logged once on the first successful assert.
        logged_mode: bool,
    }

    impl AlsaMixer {
        pub fn new(device: String, config: MixerConfig) -> AlsaMixer {
            AlsaMixer {
                device,
                config,
                logged_mode: false,
            }
        }

        fn control_names(mixer: &Mixer) -> String {
            let names: Vec<String> = mixer
                .iter()
                .filter_map(|elem| {
                    Selem::new(elem).map(|s| format!("{:?}", s.get_id().get_name().unwrap_or("?")))
                })
                .collect();
            names.join(", ")
        }
    }

    impl MixerControl for AlsaMixer {
        fn assert_ceiling(&mut self) -> Result<(), MixerError> {
            let device = &self.device;
            let control = &self.config.control;
            let mixer = Mixer::new(device, false)
                .map_err(|err| MixerError(format!("cannot open mixer {device}: {err}")))?;
            let id = SelemId::new(control, 0);
            let Some(selem) = mixer.find_selem(&id) else {
                return Err(MixerError(format!(
                    "mixer {device} has no control {control:?}; available: {}; \
                     discover with `amixer -D {device} scontrols`",
                    AlsaMixer::control_names(&mixer)
                )));
            };
            if !selem.has_playback_volume() {
                return Err(MixerError(format!(
                    "mixer control {control:?} on {device} has no playback volume"
                )));
            }

            match self.config.ceiling {
                MixerCeiling::Db(db) => {
                    let requested = MilliBel((f64::from(db) * 100.0).round() as i64);
                    let (min, max) = selem.get_playback_db_range();
                    if min >= max {
                        return Err(MixerError(format!(
                            "mixer control {control:?} on {device} reports no usable dB range; \
                             use ceiling_percent instead of ceiling_db"
                        )));
                    }
                    // Round::Floor: when the ceiling falls between hardware
                    // steps, land below it, never above.
                    selem
                        .set_playback_db_all(requested, alsa::Round::Floor)
                        .map_err(|err| {
                            MixerError(format!(
                                "cannot set {control:?} on {device} to {db} dB: {err}"
                            ))
                        })?;
                    let landed =
                        selem
                            .get_playback_vol_db(SelemChannelId::mono())
                            .map_err(|err| {
                                MixerError(format!(
                                    "cannot read back {control:?} on {device}: {err}"
                                ))
                            })?;
                    if landed > requested || landed < requested - DB_SLACK {
                        return Err(MixerError(format!(
                            "readback mismatch on {control:?} ({device}): asked for {db} dB, \
                             device reports {} dB",
                            landed.to_db()
                        )));
                    }
                    if !self.logged_mode {
                        println!(
                            "radiod: mixer ceiling {control:?} on {device}: {} dB (dB mode, \
                             device range {}..{} dB)",
                            landed.to_db(),
                            min.to_db(),
                            max.to_db()
                        );
                        self.logged_mode = true;
                    }
                }
                MixerCeiling::Percent(percent) => {
                    let (min, max) = selem.get_playback_volume_range();
                    // Integer floor keeps the target at or below the exact
                    // percentage point of the raw range.
                    let target = min + (max - min) * i64::from(percent) / 100;
                    selem.set_playback_volume_all(target).map_err(|err| {
                        MixerError(format!(
                            "cannot set {control:?} on {device} to {percent}% (raw {target}): {err}"
                        ))
                    })?;
                    let landed =
                        selem
                            .get_playback_volume(SelemChannelId::mono())
                            .map_err(|err| {
                                MixerError(format!(
                                    "cannot read back {control:?} on {device}: {err}"
                                ))
                            })?;
                    if landed != target {
                        return Err(MixerError(format!(
                            "readback mismatch on {control:?} ({device}): set raw {target}, \
                             device reports {landed}"
                        )));
                    }
                    if !self.logged_mode {
                        println!(
                            "radiod: mixer ceiling {control:?} on {device}: {percent}% raw \
                             ({target} of {min}..{max}; raw mode — the scale may be nonlinear, \
                             prefer ceiling_db if the control supports it)"
                        );
                        self.logged_mode = true;
                    }
                }
            }

            // A muted switch is fail-quiet, not fail-loud, but correct it so
            // "why is there no sound" has one less answer.
            if selem.has_playback_switch() {
                selem.set_playback_switch_all(1).map_err(|err| {
                    MixerError(format!("cannot unmute {control:?} on {device}: {err}"))
                })?;
            }
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
pub use alsa_mixer::AlsaMixer;

/// Builds the mixer for an alsa-sink radiod: device from `[mixer].device`
/// or derived from `audio_device`.
#[cfg(target_os = "linux")]
pub fn make_alsa_mixer(
    audio_device: &str,
    config: MixerConfig,
) -> Result<Box<dyn MixerControl>, MixerError> {
    let device = match &config.device {
        Some(device) => device.clone(),
        None => mixer_device_for(audio_device)?,
    };
    Ok(Box::new(AlsaMixer::new(device, config)))
}

#[cfg(not(target_os = "linux"))]
pub fn make_alsa_mixer(
    _audio_device: &str,
    _config: MixerConfig,
) -> Result<Box<dyn MixerControl>, MixerError> {
    Err(MixerError(
        "the alsa mixer is only available on Linux".to_string(),
    ))
}

#[cfg(test)]
pub mod testing {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Scriptable fake: counts asserts, fails on demand.
    #[derive(Clone, Default)]
    pub struct TestMixer {
        pub asserts: Arc<Mutex<u32>>,
        pub fail_with: Arc<Mutex<Option<String>>>,
    }

    impl MixerControl for TestMixer {
        fn assert_ceiling(&mut self) -> Result<(), MixerError> {
            *self.asserts.lock().unwrap() += 1;
            match &*self.fail_with.lock().unwrap() {
                Some(message) => Err(MixerError(message.clone())),
                None => Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixer_device_derives_from_common_pcm_names() {
        assert_eq!(mixer_device_for("plughw:1,0").unwrap(), "hw:1");
        assert_eq!(mixer_device_for("hw:0,0").unwrap(), "hw:0");
        assert_eq!(mixer_device_for("plughw:0").unwrap(), "hw:0");
        assert_eq!(
            mixer_device_for("plughw:CARD=Device,DEV=0").unwrap(),
            "hw:Device"
        );
    }

    #[test]
    fn mixer_device_underivable_names_are_errors() {
        assert!(mixer_device_for("default").is_err());
        assert!(mixer_device_for("plughw:").is_err());
    }

    #[test]
    fn test_mixer_counts_asserts_and_scripts_failures() {
        let mut mixer = testing::TestMixer::default();
        assert!(mixer.assert_ceiling().is_ok());
        *mixer.fail_with.lock().unwrap() = Some("ceiling gone".to_string());
        let err = mixer.assert_ceiling().unwrap_err();
        assert_eq!(err.to_string(), "ceiling gone");
        assert_eq!(*mixer.asserts.lock().unwrap(), 2);
    }
}
