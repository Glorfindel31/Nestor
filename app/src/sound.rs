//! The finished-job chime: three beeps, like a microwave.
//!
//! A real nest runs for anywhere between ten seconds and ten minutes, and the
//! operator does not stand and watch it - they go and do something else. The
//! progress bar is only useful to somebody already looking at the screen, so
//! the end of a run needs to make a noise, not just change colour.
//!
//! **Synthesised, not shipped.** The waveform is generated at first use rather
//! than bundled as a `.wav` asset: three beeps is a sine, an envelope and a
//! gap, which is less code than the loader for a file would be, adds nothing
//! to the binary, and cannot go missing. It also avoids handing an antivirus
//! ML classifier one more embedded blob to weigh - see `RELEASING.md`.
//!
//! ponytail: `PlaySound` from `winmm`, which every Windows has, rather than a
//! cross-platform audio crate. `rodio` pulls in ~30 crates and a device-
//! enumeration thread to play 0.7 seconds of beep. Windows is the only
//! platform this app has ever shipped a working build for
//! (`.github/workflows/release.yml`), so the other two get the no-op below.
//! Upgrade path if mac/linux ever ship for real: `rodio::OutputStream` plus
//! `rodio::buffer::SamplesBuffer` over the same `samples()` this already
//! builds.

use std::sync::OnceLock;

/// 22.05kHz is twice over what a 2kHz beep needs and a quarter the bytes of
/// CD rate. Nothing here has content anywhere near Nyquist.
const RATE: u32 = 22_050;

/// Microwave beeps sit around 2kHz. Below ~1.5kHz it reads as an error tone,
/// above ~3kHz as a smoke alarm.
const FREQ: f64 = 2000.0;

const BEEPS: usize = 3;
const BEEP_SECS: f64 = 0.13;
const GAP_SECS: f64 = 0.09;

/// Fade in and out of every beep. Cutting a sine off mid-cycle puts a step in
/// the waveform, and a step is a click - which is what makes a synthesised
/// beep sound cheap rather than like a machine.
const FADE_SECS: f64 = 0.006;

/// The chime, as a complete PCM WAV file.
///
/// Built once and kept for the life of the process, which is also what makes
/// the asynchronous playback below sound: `PlaySound` with `SND_ASYNC` reads
/// the buffer *after* returning, so it must outlive the call.
fn wav() -> &'static [u8] {
    static WAV: OnceLock<Vec<u8>> = OnceLock::new();
    WAV.get_or_init(|| riff(&samples()))
}

/// One channel of 16-bit signed samples: `BEEPS` tones separated by silence.
fn samples() -> Vec<i16> {
    let beep = (BEEP_SECS * f64::from(RATE)) as usize;
    let gap = (GAP_SECS * f64::from(RATE)) as usize;
    let fade = (FADE_SECS * f64::from(RATE)) as usize;

    let mut out = Vec::with_capacity(BEEPS * (beep + gap));
    for _ in 0..BEEPS {
        for i in 0..beep {
            let t = i as f64 / f64::from(RATE);
            // A little third harmonic. A pure sine sounds like a hearing test;
            // the harmonic is what makes it read as a small cheap speaker in
            // an appliance, which is the sound being asked for.
            let wave = (std::f64::consts::TAU * FREQ * t).sin() + 0.18 * (std::f64::consts::TAU * FREQ * 3.0 * t).sin();
            // `beep - 1 - i`, not `beep - i`: the fade has to reach zero on
            // the *last* sample, not one past the end of the buffer, or every
            // beep stops on a small step and clicks.
            let envelope = (i as f64 / fade as f64).min((beep - 1 - i) as f64 / fade as f64).clamp(0.0, 1.0);
            // 0.35 of full scale. This interrupts somebody who has walked
            // away, and it is going to fire on every job all day.
            out.push((wave * envelope * 0.35 * f64::from(i16::MAX)) as i16);
        }
        out.extend(std::iter::repeat_n(0, gap));
    }
    out
}

/// Wraps PCM samples in the 44-byte canonical WAV header. Mono, 16-bit.
fn riff(samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend(b"RIFF");
    out.extend((36 + data_len).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes()); // PCM header size
    out.extend(1u16.to_le_bytes()); // format: PCM
    out.extend(1u16.to_le_bytes()); // channels
    out.extend(RATE.to_le_bytes());
    out.extend((RATE * 2).to_le_bytes()); // bytes per second
    out.extend(2u16.to_le_bytes()); // block align
    out.extend(16u16.to_le_bytes()); // bits per sample
    out.extend(b"data");
    out.extend(data_len.to_le_bytes());
    for s in samples {
        out.extend(s.to_le_bytes());
    }
    out
}

#[cfg(target_os = "windows")]
mod sys {
    // `winmm` ships with every Windows and is already linked by half the
    // desktop; this is one import, not a dependency.
    #[link(name = "winmm")]
    extern "system" {
        fn PlaySoundW(sound: *const u16, module: *mut core::ffi::c_void, flags: u32) -> i32;
    }

    const SND_ASYNC: u32 = 0x0001;
    /// Say nothing rather than falling back to the system default beep if the
    /// buffer is ever rejected. A wrong sound is worse than no sound - the
    /// operator would learn to trust it and then be told a job finished by
    /// something that was really an error ding.
    const SND_NODEFAULT: u32 = 0x0002;
    const SND_MEMORY: u32 = 0x0004;

    /// # Safety
    /// `wav` must remain valid until playback finishes, which `SND_ASYNC`
    /// makes the caller's problem. The only caller passes a `&'static` buffer.
    pub fn play(wav: &'static [u8]) {
        // Failure is ignored on purpose: no sound card, an exclusive-mode
        // device, or a session with no audio endpoint at all are all normal on
        // a shop-floor machine, and none of them is worth a dialog.
        unsafe {
            PlaySoundW(wav.as_ptr().cast(), core::ptr::null_mut(), SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
        }
    }
}

/// Play the chime. Returns immediately - playback runs on the OS's own
/// thread, so this is safe to call straight from `update()`.
#[cfg(target_os = "windows")]
pub fn finished() {
    sys::play(wav());
}

#[cfg(not(target_os = "windows"))]
pub fn finished() {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header has to describe the payload exactly or Windows plays
    /// nothing at all, silently - there is no error path to notice.
    #[test]
    fn the_header_describes_the_payload() {
        let pcm = samples();
        let wav = riff(&pcm);

        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 44 + pcm.len() * 2, "the file must be the header plus every sample");

        let declared = u32::from_le_bytes(wav[40..44].try_into().unwrap()) as usize;
        assert_eq!(declared, pcm.len() * 2, "the data chunk lies about its own length");
        let riff_len = u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize;
        assert_eq!(riff_len, wav.len() - 8, "the RIFF length must exclude its own eight bytes");
    }

    /// Three beeps with silence between them, and no step at either end of
    /// one - a beep that starts or stops at full amplitude clicks.
    #[test]
    fn it_is_three_fading_beeps_and_not_one_long_one() {
        let pcm = samples();
        let beep = (BEEP_SECS * f64::from(RATE)) as usize;
        let gap = (GAP_SECS * f64::from(RATE)) as usize;

        assert_eq!(pcm.len(), BEEPS * (beep + gap));
        for n in 0..BEEPS {
            let start = n * (beep + gap);
            assert_eq!(pcm[start], 0, "beep {n} starts at full amplitude, which clicks");
            assert_eq!(pcm[start + beep - 1], 0, "beep {n} ends at full amplitude, which clicks");
            assert!(pcm[start..start + beep].iter().any(|s| s.abs() > 1000), "beep {n} is silent");
            assert!(pcm[start + beep..start + beep + gap].iter().all(|&s| s == 0), "the gap after beep {n} is not silent");
        }
    }
}
