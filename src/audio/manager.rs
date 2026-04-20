use std::collections::HashMap;

use macroquad::audio::{load_sound, play_sound, set_sound_volume, stop_sound, PlaySoundParams, Sound};

use crate::game::config::AudioConfig;
use super::types::{MusicTrack, SoundEffect};

pub struct AudioManager {
    sfx:   HashMap<SoundEffect, Sound>,
    music: HashMap<MusicTrack,  Sound>,

    pub master_volume:      f32,
    pub music_volume:       f32,
    pub sfx_volume:         f32,
    pub crossfade_duration: f32,

    /// Per-effect gains (0..max_gain), applied on top of master × sfx_volume.
    sfx_gain: HashMap<SoundEffect, f32>,
    /// Per-track gains (0..max_gain), applied on top of master × music_volume.
    music_gain: HashMap<MusicTrack, f32>,

    /// Currently playing music track (key into `music` map).
    current_music: Option<MusicTrack>,
    /// Track retiring via fade-out: (key, elapsed_seconds).
    fade_out: Option<(MusicTrack, f32)>,
    /// Seconds elapsed since current music started (drives fade-in ramp).
    fade_in_elapsed: f32,

    /// Playback duration in seconds, parsed from WAV headers at load.
    /// Used to manually re-trigger looped tracks (quad-snd WAV loop is unreliable).
    music_duration: HashMap<MusicTrack, f32>,
    /// Seconds since current track's last (re)trigger — for manual looping.
    music_elapsed: f32,

    cooldowns: HashMap<SoundEffect, f32>,
}

impl AudioManager {
    /// Async initialiser.  Missing audio files are skipped with a log message.
    pub async fn load_all(cfg: &AudioConfig) -> Self {
        let sfx_keys = [
            SoundEffect::FootstepWalk,
            SoundEffect::FootstepRun,
            SoundEffect::QteCorrect,
            SoundEffect::QteWrong,
            SoundEffect::QteRoundComplete,
            SoundEffect::QteSessionComplete,
            SoundEffect::UrgencyWarning,
            SoundEffect::UrgencyCritical,
            SoundEffect::Victory,
            SoundEffect::GameOver,
        ];
        let music_keys = [
            MusicTrack::MainTheme,
            MusicTrack::TenseLoop,
            MusicTrack::VictoryStinger,
            MusicTrack::DefeatStinger,
        ];

        let mut sfx = HashMap::new();
        let mut sfx_gain = HashMap::new();
        for key in sfx_keys {
            if let Some(s) = try_load(key.path()).await { sfx.insert(key, s); }
            sfx_gain.insert(key, key.default_gain());
        }
        let mut music_map = HashMap::new();
        let mut music_gain = HashMap::new();
        let mut music_duration = HashMap::new();
        for key in music_keys {
            if let Some((s, dur)) = try_load_music(key.path()).await {
                music_map.insert(key, s);
                match dur {
                    Some(d) => {
                        eprintln!("[audio] {:?} duration = {:.2}s", key, d);
                        music_duration.insert(key, d);
                    }
                    None => eprintln!("[audio] {:?} duration UNKNOWN — manual loop disabled", key),
                }
            }
            music_gain.insert(key, key.default_gain());
        }

        Self {
            sfx,
            music:           music_map,
            master_volume:   cfg.master_volume,
            music_volume:    cfg.music_volume,
            sfx_volume:      cfg.sfx_volume,
            crossfade_duration: cfg.crossfade_duration,
            sfx_gain,
            music_gain,
            current_music:   None,
            fade_out:        None,
            fade_in_elapsed: 0.0,
            music_duration,
            music_elapsed:   0.0,
            cooldowns:       HashMap::new(),
        }
    }

    /// Call once per frame.  Advances cooldowns and updates crossfade volumes.
    pub fn update(&mut self, dt: f32) {
        for v in self.cooldowns.values_mut() {
            *v = (*v - dt).max(0.0);
        }

        let eff_music = self.master_volume * self.music_volume;

        // Ramp out the retiring track.
        if let Some((fade_track, elapsed)) = &mut self.fade_out {
            *elapsed += dt;
            let t = if self.crossfade_duration > 0.0 {
                (*elapsed / self.crossfade_duration).min(1.0)
            } else {
                1.0
            };
            let fade_track_copy = *fade_track;
            let gain = self.music_gain.get(&fade_track_copy).copied().unwrap_or(1.0);
            if let Some(s) = self.music.get(&fade_track_copy) {
                set_sound_volume(s, (1.0 - t) * eff_music * gain);
            }
            if t >= 1.0 {
                if let Some(s) = self.music.get(&fade_track_copy) { stop_sound(s); }
                self.fade_out = None;
            }
        }

        // Ramp in the new track.
        if self.crossfade_duration > 0.0 && self.fade_in_elapsed < self.crossfade_duration {
            self.fade_in_elapsed += dt;
            let t = (self.fade_in_elapsed / self.crossfade_duration).min(1.0);
            if let Some(track) = self.current_music {
                let gain = self.music_gain.get(&track).copied().unwrap_or(1.0);
                if let Some(s) = self.music.get(&track) {
                    set_sound_volume(s, t * eff_music * gain);
                }
            }
        }

        // Manual loop: quad-snd's WAV loop flag is unreliable, so we re-trigger
        // looped tracks ourselves slightly before the file ends (trailing silence
        // in the WAV would otherwise leave a gap).
        if let Some(track) = self.current_music {
            if track.looped() {
                self.music_elapsed += dt;
                if let Some(&duration) = self.music_duration.get(&track) {
                    // Re-trigger 100ms before end.
                    let trigger_at = (duration - 0.10).max(0.0);
                    if duration > 0.0 && self.music_elapsed >= trigger_at {
                        eprintln!("[audio] loop re-trigger {:?} at {:.2}s", track, self.music_elapsed);
                        self.music_elapsed = 0.0;
                        if let Some(s) = self.music.get(&track) {
                            let gain = self.music_gain.get(&track).copied().unwrap_or(1.0);
                            play_sound(s, PlaySoundParams {
                                looped: false,
                                volume: eff_music * gain,
                            });
                        }
                    }
                }
            }
        }
    }

    /// Play a one-shot SFX.  Silently ignored if the file wasn't loaded or cooldown is active.
    pub fn play_sfx(&mut self, effect: SoundEffect) {
        if self.cooldowns.get(&effect).copied().unwrap_or(0.0) > 0.0 {
            return;
        }
        if let Some(sound) = self.sfx.get(&effect) {
            let gain = self.sfx_gain.get(&effect).copied().unwrap_or(1.0);
            let vol = self.master_volume * self.sfx_volume * gain;
            play_sound(sound, PlaySoundParams { looped: false, volume: vol });
            let cd = effect.cooldown();
            if cd > 0.0 { self.cooldowns.insert(effect, cd); }
        }
    }

    /// Switch to a music track, crossfading if configured.  No-op if already playing.
    pub fn play_music(&mut self, track: MusicTrack) {
        if self.current_music == Some(track) {
            return;
        }
        // If the requested track isn't loaded, keep the current one playing instead
        // of silently dropping into silence.
        if !self.music.contains_key(&track) {
            return;
        }

        let eff_music = self.master_volume * self.music_volume;
        let fading    = self.crossfade_duration > 0.0;

        // Retire current track.
        if let Some(old_track) = self.current_music.take() {
            // Cancel any prior fade-out.
            if let Some((dying_track, _)) = self.fade_out.take() {
                if let Some(s) = self.music.get(&dying_track) { stop_sound(s); }
            }
            if fading {
                self.fade_out = Some((old_track, 0.0));
            } else if let Some(s) = self.music.get(&old_track) {
                stop_sound(s);
            }
        }

        // Start new track. We pass `looped: false` because we re-trigger manually
        // in update() — quad-snd's WAV loop flag doesn't reliably seek back to 0.
        if let Some(sound) = self.music.get(&track) {
            let gain = self.music_gain.get(&track).copied().unwrap_or(1.0);
            let start_vol = if fading { 0.0 } else { eff_music * gain };
            play_sound(sound, PlaySoundParams { looped: false, volume: start_vol });
            self.current_music   = Some(track);
            self.fade_in_elapsed = 0.0;
            self.music_elapsed   = 0.0;
        }
    }

    /// Immediately stop a specific SFX (all instances).
    pub fn stop_sfx(&mut self, effect: SoundEffect) {
        if let Some(sound) = self.sfx.get(&effect) {
            stop_sound(sound);
        }
    }

    /// Immediately stop all music (no fade).
    pub fn stop_music(&mut self) {
        if let Some(track) = self.current_music.take() {
            if let Some(s) = self.music.get(&track) { stop_sound(s); }
        }
        if let Some((track, _)) = self.fade_out.take() {
            if let Some(s) = self.music.get(&track) { stop_sound(s); }
        }
    }

    pub fn current_track(&self) -> Option<MusicTrack> {
        self.current_music
    }

    pub fn set_master_volume(&mut self, v: f32) {
        self.master_volume = v.clamp(0.0, 1.0);
        self.sync_music_volume();
    }

    pub fn set_music_volume(&mut self, v: f32) {
        self.music_volume = v.clamp(0.0, 1.0);
        self.sync_music_volume();
    }

    pub fn set_sfx_volume(&mut self, v: f32) {
        self.sfx_volume = v.clamp(0.0, 1.0);
    }

    fn sync_music_volume(&self) {
        // Only sync when fade-in is complete.
        if self.fade_in_elapsed >= self.crossfade_duration {
            if let Some(track) = self.current_music {
                let gain = self.music_gain.get(&track).copied().unwrap_or(1.0);
                let eff = self.master_volume * self.music_volume * gain;
                if let Some(s) = self.music.get(&track) { set_sound_volume(s, eff); }
            }
        }
    }

    // ---- Per-item gain accessors ----

    pub fn sfx_gain(&self, effect: SoundEffect) -> f32 {
        self.sfx_gain.get(&effect).copied().unwrap_or(effect.default_gain())
    }

    pub fn set_sfx_gain(&mut self, effect: SoundEffect, v: f32) {
        self.sfx_gain.insert(effect, v.max(0.0));
    }

    pub fn music_gain(&self, track: MusicTrack) -> f32 {
        self.music_gain.get(&track).copied().unwrap_or(track.default_gain())
    }

    pub fn set_music_gain(&mut self, track: MusicTrack, v: f32) {
        self.music_gain.insert(track, v.max(0.0));
        // Live-update if this is the current track and fade-in is complete.
        self.sync_music_volume();
    }
}

/// Load a sound file, returning None on missing file or unsupported format.
///
/// quad-snd panics instead of returning Err on bad files, so we validate the
/// actual bytes (magic numbers + WAV format code) before calling load_sound.
async fn try_load(path: &str) -> Option<Sound> {
    if !std::path::Path::new(path).exists() {
        eprintln!("[audio] missing: {path}");
        return None;
    }

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => { eprintln!("[audio] read error {path}: {e}"); return None; }
    };

    if let Some(reason) = format_problem(&bytes) {
        eprintln!("[audio] skipping {path}: {reason}");
        eprintln!("        fix: ffmpeg -y -i {path} -c:a pcm_s16le -ar 44100 -ac 2 {path}");
        return None;
    }

    match load_sound(path).await {
        Ok(s) => Some(s),
        Err(e) => { eprintln!("[audio] load error {path}: {e}"); None }
    }
}

/// Like `try_load`, but also returns the WAV playback duration in seconds
/// (None for non-WAV containers or unparseable headers).
async fn try_load_music(path: &str) -> Option<(Sound, Option<f32>)> {
    if !std::path::Path::new(path).exists() {
        eprintln!("[audio] missing: {path}");
        return None;
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => { eprintln!("[audio] read error {path}: {e}"); return None; }
    };
    if let Some(reason) = format_problem(&bytes) {
        eprintln!("[audio] skipping {path}: {reason}");
        return None;
    }
    let dur = wav_duration_secs(&bytes);
    match load_sound(path).await {
        Ok(s) => Some((s, dur)),
        Err(e) => { eprintln!("[audio] load error {path}: {e}"); None }
    }
}

/// Parse the `fmt ` and `data` chunks to compute WAV duration in seconds.
/// Returns None for non-WAV or malformed files.
fn wav_duration_secs(bytes: &[u8]) -> Option<f32> {
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut i = 12;
    let mut byte_rate: Option<u32> = None;
    let mut data_size: Option<u32> = None;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let sz = u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        let data_off = i + 8;
        if id == b"fmt " && data_off + 16 <= bytes.len() {
            byte_rate = Some(u32::from_le_bytes([
                bytes[data_off + 8], bytes[data_off + 9],
                bytes[data_off + 10], bytes[data_off + 11],
            ]));
        } else if id == b"data" {
            data_size = Some(sz as u32);
            break;
        }
        // Chunks pad to even sizes.
        i = data_off + sz + (sz & 1);
    }
    let br = byte_rate? as f32;
    let ds = data_size? as f32;
    if br > 0.0 { Some(ds / br) } else { None }
}

/// Check raw bytes for known-bad formats.  Returns a human-readable reason or None if ok.
fn format_problem(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() < 12 { return Some("file too small"); }

    // OGG container — check for Opus vs Vorbis.
    if &bytes[0..4] == b"OggS" {
        let header = &bytes[..bytes.len().min(128)];
        if header.windows(8).any(|w| w == b"OpusHead") {
            return Some("OGG Opus codec — re-encode as OGG Vorbis");
        }
        return None; // OGG Vorbis — ok
    }

    // RIFF/WAVE container — check WAV format chunk.
    if &bytes[0..4] == b"RIFF" && bytes.len() >= 44 && &bytes[8..12] == b"WAVE" {
        let fmt = u16::from_le_bytes([bytes[20], bytes[21]]);
        match fmt {
            1 | 3 => {} // PCM or IEEE float — ok
            0xFFFE => {} // Extensible WAV — usually ok
            _ => return Some("compressed WAV — re-encode as PCM (pcm_s16le)"),
        }
        let bits = u16::from_le_bytes([bytes[34], bytes[35]]);
        if !matches!(bits, 8 | 16 | 24 | 32) {
            return Some("unusual bit depth in WAV");
        }
        return None;
    }

    // Neither OGG nor RIFF — unknown container.
    Some("unrecognised format (expected OGG Vorbis or WAV)")
}
