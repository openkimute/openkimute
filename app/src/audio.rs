//! Audio playback (rodio): looping music and one-shot sound effects from the converted WAV / FLAC files.
//! What plays is decided by `kumite_game::sound`; this only plays it. Without an audio device the game is silent.
//!
//! Or, for a replay made a video (`video.rs`), nothing plays: what would is noted with the game step it starts at
//! ([`Audio::capture`]), and mixed afterwards.

use rodio::source::{Buffered, Source};
use rodio::{Decoder, MixerDeviceSink, Player};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

type Clip = Buffered<Decoder<BufReader<File>>>;

/// What the game asked to play, in capture.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The music: a track (looped or once), or none.
    Music(Option<(PathBuf, bool)>),
    /// A one-shot sound at this volume (the effects' volume included).
    Sfx(PathBuf, f32),
    /// The music quieter (or not).
    Duck(bool),
}

/// The sounds asked for, by game step.
#[derive(Default)]
pub struct Capture {
    /// The game step now (the capturer counts them).
    pub step: u64,
    pub events: Vec<(u64, Event)>,
}

pub struct Audio {
    /// The output device (none in capture).
    sink: Option<MixerDeviceSink>,
    /// Capturing instead of playing.
    pub capture: Option<Capture>,
    music: Option<Player>,
    current: Option<PathBuf>,
    /// Decoded sound effects, kept for replay.
    cache: HashMap<PathBuf, Clip>,
    pub muted: bool,
    pub music_volume: f32,
    pub sfx_volume: f32,
}

impl Audio {
    /// Open the default output device; None (and a log line) when there is none.
    pub fn new() -> Option<Self> {
        match rodio::DeviceSinkBuilder::open_default_sink() {
            Ok(mut sink) => {
                sink.log_on_drop(false);
                Some(Self { sink: Some(sink), capture: None, music: None, current: None, cache: HashMap::new(), muted: false, music_volume: 0.55, sfx_volume: 0.9 })
            }
            Err(e) => {
                log::warn!("no audio output: {e}");
                None
            }
        }
    }

    /// No device: what the game asks to play is noted ([`Capture`]).
    pub fn capture() -> Self {
        Self { sink: None, capture: Some(Capture::default()), music: None, current: None, cache: HashMap::new(), muted: false, music_volume: 0.55, sfx_volume: 0.9 }
    }

    /// Play `path` as music, looped or once (nothing changes when it is already the current track, even when a
    /// jingle has ended); None stops the music.
    pub fn music(&mut self, track: Option<(&Path, bool)>) {
        let (path, looped) = (track.map(|t| t.0), track.is_some_and(|t| t.1));
        if self.current.as_deref() == path {
            return;
        }
        if let Some(p) = self.music.take() {
            p.stop();
        }
        self.current = path.map(Path::to_path_buf);
        if let Some(c) = self.capture.as_mut() {
            c.events.push((c.step, Event::Music(path.map(|p| (p.to_path_buf(), looped)))));
            return;
        }
        let (Some(path), Some(sink)) = (path, self.sink.as_ref()) else { return };
        match File::open(path).map_err(anyhow::Error::from).and_then(|f| Decoder::try_from(f).map_err(anyhow::Error::from)) {
            Ok(src) => {
                let player = Player::connect_new(sink.mixer());
                player.set_volume(if self.muted { 0.0 } else { self.music_volume });
                if looped {
                    player.append(src.repeat_infinite());
                } else {
                    player.append(src);
                }
                self.music = Some(player);
            }
            Err(e) => log::warn!("music {}: {e:#}", path.display()),
        }
    }

    /// Start a one-shot sound.
    pub fn sfx(&mut self, path: &Path, volume: f32) {
        if self.muted {
            return;
        }
        if let Some(c) = self.capture.as_mut() {
            c.events.push((c.step, Event::Sfx(path.to_path_buf(), volume * self.sfx_volume)));
            return;
        }
        let Some(sink) = self.sink.as_ref() else { return };
        let clip = match self.cache.get(path) {
            Some(c) => c.clone(),
            None => match File::open(path).map_err(anyhow::Error::from).and_then(|f| Decoder::try_from(f).map_err(anyhow::Error::from)) {
                Ok(d) => {
                    let c = d.buffered();
                    self.cache.insert(path.to_path_buf(), c.clone());
                    c
                }
                Err(e) => {
                    log::warn!("sound {}: {e:#}", path.display());
                    return;
                }
            },
        };
        sink.mixer().add(clip.amplify(volume * self.sfx_volume));
    }

    pub fn toggle_mute(&mut self) -> bool {
        self.muted = !self.muted;
        if let Some(p) = &self.music {
            p.set_volume(if self.muted { 0.0 } else { self.music_volume });
        }
        self.muted
    }

    /// The music held (Android: the game in the background) or going on.
    #[cfg(target_os = "android")]
    pub fn pause(&self, paused: bool) {
        match &self.music {
            Some(p) if paused => p.pause(),
            Some(p) => p.play(),
            None => {}
        }
    }

    /// Music quieter while paused.
    pub fn duck(&mut self, ducked: bool) {
        // (the game says it every step: noted when it changes)
        if let Some(c) = self.capture.as_mut() {
            let was = c.events.iter().rev().find_map(|e| if let Event::Duck(d) = e.1 { Some(d) } else { None }).unwrap_or(false);
            if was != ducked {
                c.events.push((c.step, Event::Duck(ducked)));
            }
        }
        if let Some(p) = &self.music {
            p.set_volume(if self.muted {
                0.0
            } else if ducked {
                self.music_volume * 0.3
            } else {
                self.music_volume
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn assets() -> Option<PathBuf> {
        let a = std::env::var_os("OPENKUMITE_ASSETS").map_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets"), PathBuf::from);
        kumite_fs::exists(&a.join("audio/index.json")).then_some(a)
    }

    #[test]
    fn every_converted_sound_and_track_decodes() {
        let Some(a) = assets() else { return };
        let index: serde_json::Value = serde_json::from_slice(&kumite_fs::read(&a.join("audio/index.json")).unwrap()).unwrap();
        let mut checked = 0;
        for bank in index["sfx"].as_array().unwrap() {
            for s in bank["samples"].as_array().unwrap() {
                let path = a.join("audio/sfx").join(s["file"].as_str().unwrap());
                let d = Decoder::try_from(File::open(&path).unwrap()).unwrap();
                let rate = d.sample_rate().get() as f32;
                let n = d.count() as f32;
                let secs = s["seconds"].as_f64().unwrap() as f32;
                assert!((n / rate - secs).abs() < 0.05, "{}: {} s decoded, index says {secs}", path.display(), n / rate);
                checked += 1;
            }
        }
        // every music track and movie soundtrack, to its whole length
        let music = index["music"].as_array().unwrap().iter().map(|m| ("audio/music", &m["file"], &m["seconds"]));
        // (a conversion from before the FLAC soundtracks names none: its movies are not checked here)
        let movies = index["video"].as_array().into_iter().flatten().filter(|v| v["audio_file"].is_string());
        let movies = movies.map(|v| ("video", &v["audio_file"], &v["seconds"]));
        for (dir, file, secs) in music.chain(movies) {
            let path = a.join(dir).join(file.as_str().unwrap());
            let d = Decoder::try_from(File::open(&path).unwrap()).unwrap();
            let rate = (d.sample_rate().get() * d.channels().get() as u32) as f64;
            let (n, secs) = (d.count() as f64, secs.as_f64().unwrap());
            assert!((n / rate - secs).abs() < 0.1, "{}: {} s decoded, index says {secs}", path.display(), n / rate);
            checked += 1;
        }
        eprintln!("decoded {checked} files");
        assert!(checked > 800);
    }

    #[test]
    fn the_output_device_plays_silently() {
        let Some(a) = assets() else { return };
        let Some(mut audio) = Audio::new() else { return }; // no device on this machine: nothing to test
        audio.music_volume = 0.0;
        audio.sfx_volume = 0.0;
        let index: serde_json::Value = serde_json::from_slice(&kumite_fs::read(&a.join("audio/index.json")).unwrap()).unwrap();
        audio.music(Some((&a.join("audio/music").join(index["music"][3]["file"].as_str().unwrap()), true)));
        audio.sfx(&a.join("audio/sfx/sys/04.wav"), 1.0);
        audio.sfx(&a.join("audio/sfx/sys/04.wav"), 1.0); // second play comes from the cache
        assert_eq!(audio.cache.len(), 1);
        std::thread::sleep(Duration::from_millis(200));
        assert!(audio.music.as_ref().is_some_and(|p| !p.empty()), "music should be queued");
        audio.music(None);
        assert!(audio.music.is_none());
    }
}
