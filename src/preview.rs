//! Playing a title inside the app: FFmpeg decodes picture and sound in one run, the picture
//! as raw frames on stdout and the sound as raw samples on stderr. Protected Blu-rays are fed
//! in unlocked through libbluray, the same way as when ripping.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const WIDTH: usize = 854;
pub const HEIGHT: usize = 480;
const FPS: f64 = 25.0;

/// What to play: a title's input, or a protected Blu-ray playlist.
#[derive(Clone)]
pub struct Source {
    pub input: String,
    pub locked: Option<(PathBuf, u32)>,
    pub duration: Option<f64>,
    pub audio_track: usize,
    pub has_audio: bool,
}

/// The newest frame waiting to be shown.
pub type Frame = Arc<Mutex<Option<Vec<u8>>>>;

/// One run of FFmpeg from a given point; dropping it stops playback.
pub struct Session {
    child: Child,
    stop: Arc<AtomicBool>,
    pub frame: Frame,
    frames: Arc<AtomicU64>,
    pub start: f64,
    pub with_audio: bool,
    _stream: Option<cpal::Stream>,
}

impl Session {
    pub fn start(ffmpeg: &Path, src: &Source, at: f64, with_audio: bool, muted: Arc<AtomicBool>, wake: impl Fn() + Send + 'static) -> Result<Session, String> {
        let at = at.max(0.0);
        // The sound device decides the sample rate.
        let audio_out = if with_audio {
            cpal::default_host().default_output_device().and_then(|d| {
                let cfg = d.default_output_config().ok()?;
                Some((d, cfg.config()))
            })
        } else {
            None
        };

        let mut reader = None;
        let mut args: Vec<String> = ["-v", "quiet", "-nostdin", "-nostats"].iter().map(|s| s.to_string()).collect();
        match &src.locked {
            Some((root, playlist)) => {
                let mut r = crate::bluray::Reader::open(root, *playlist)?;
                // Protected discs seek by position in the stream.
                if let Some(d) = src.duration.filter(|d| *d > 0.0) {
                    r.seek((r.size() as f64 * (at / d).clamp(0.0, 0.99)) as u64);
                }
                reader = Some(r);
                args.extend(["-f", "mpegts", "-i", "pipe:0"].map(String::from));
            }
            None => {
                args.extend(["-ss".into(), format!("{at:.2}"), "-i".into(), src.input.clone()]);
            }
        }
        let vf = format!(
            "bwdif=deint=interlaced,fps={FPS},scale={WIDTH}:{HEIGHT}:force_original_aspect_ratio=decrease,pad={WIDTH}:{HEIGHT}:(ow-iw)/2:(oh-ih)/2"
        );
        args.extend(["-map", "0:v:0", "-vf", &vf, "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"].map(String::from));
        if let Some((_, cfg)) = &audio_out {
            args.extend([
                "-map".into(),
                format!("0:a:{}", src.audio_track),
                "-ac".into(),
                "2".into(),
                "-ar".into(),
                cfg.sample_rate.to_string(),
                "-f".into(),
                "f32le".into(),
                "pipe:2".into(),
            ]);
        }

        let mut child = crate::disc::hidden_command(ffmpeg)
            .args(&args)
            .stdin(if reader.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(if audio_out.is_some() { Stdio::piped() } else { Stdio::null() })
            .spawn()
            .map_err(|e| format!("Couldn't start FFmpeg: {e}"))?;
        let stop = Arc::new(AtomicBool::new(false));

        if let Some(mut r) = reader {
            let mut stdin = child.stdin.take().unwrap();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 6144 * 32];
                while !stop.load(Ordering::SeqCst) {
                    match r.read(&mut buf) {
                        Ok(n) if n > 0 => {
                            if stdin.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
            });
        }

        // Sound: samples go into a short queue that the sound device drains.
        let mut stream = None;
        if let Some((device, cfg)) = audio_out {
            let queue = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(96_000 * 2)));
            let channels = cfg.channels as usize;
            let q = queue.clone();
            let muted2 = muted.clone();
            let s = device
                .build_output_stream(
                    cfg.clone(),
                    move |out: &mut [f32], _| {
                        let mut q = q.lock().unwrap();
                        let mute = muted2.load(Ordering::Relaxed);
                        for frame in out.chunks_mut(channels) {
                            let (l, r) = match (q.pop_front(), q.pop_front()) {
                                (Some(l), Some(r)) => (l, r),
                                _ => (0.0, 0.0),
                            };
                            for (c, v) in frame.iter_mut().enumerate() {
                                *v = if mute { 0.0 } else if c % 2 == 0 { l } else { r };
                            }
                        }
                    },
                    |e| eprintln!("sound: {e}"),
                    None,
                )
                .map_err(|e| e.to_string())?;
            s.play().map_err(|e| e.to_string())?;
            stream = Some(s);
            let mut stderr = child.stderr.take().unwrap();
            let stop = stop.clone();
            let limit = cfg.sample_rate as usize * 2; // one second, stereo
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 16384];
                while !stop.load(Ordering::SeqCst) {
                    let Ok(n) = stderr.read(&mut buf) else { break };
                    if n == 0 {
                        break;
                    }
                    let n = n - n % 4;
                    while queue.lock().unwrap().len() > limit && !stop.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    let mut q = queue.lock().unwrap();
                    q.extend(buf[..n].chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])));
                }
            });
        }

        // Picture: read a frame, wait until its moment, then hand it to the window.
        let frame: Frame = Arc::new(Mutex::new(None));
        let frames = Arc::new(AtomicU64::new(0));
        {
            let mut stdout = child.stdout.take().unwrap();
            let (frame, frames, stop) = (frame.clone(), frames.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut buf = vec![0u8; WIDTH * HEIGHT * 4];
                let mut t0 = None;
                while !stop.load(Ordering::SeqCst) {
                    if stdout.read_exact(&mut buf).is_err() {
                        break;
                    }
                    let t0 = *t0.get_or_insert_with(Instant::now);
                    let n = frames.load(Ordering::SeqCst);
                    let due = t0 + Duration::from_secs_f64(n as f64 / FPS);
                    if let Some(wait) = due.checked_duration_since(Instant::now()) {
                        std::thread::sleep(wait);
                    }
                    *frame.lock().unwrap() = Some(buf.clone());
                    frames.fetch_add(1, Ordering::SeqCst);
                    wake();
                }
            });
        }

        let with_audio = stream.is_some();
        Ok(Session { child, stop, frame, frames, start: at, with_audio, _stream: stream })
    }

    /// Seconds into the title of the frame on screen.
    pub fn position(&self) -> f64 {
        self.start + self.frames.load(Ordering::SeqCst) as f64 / FPS
    }

    /// FFmpeg ended without showing anything (for example no sound track to map).
    pub fn failed(&mut self) -> bool {
        self.frames.load(Ordering::SeqCst) == 0 && matches!(self.child.try_wait(), Ok(Some(_)))
    }

    pub fn ended(&mut self) -> bool {
        self.frames.load(Ordering::SeqCst) > 0 && matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn unique_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("openrip-preview-unit-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn source() -> Source {
        Source { input: "/disc/in.vob".into(), locked: None, duration: Some(60.0), audio_track: 1, has_audio: false }
    }

    fn muted() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[cfg(unix)]
    fn fake_ffmpeg(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("ffmpeg");
        fs::write(&p, format!("#!/bin/sh\n[ -n \"$OPENRIP_PROBE\" ] && exit 0\n{body}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        // Parallel tests can fork while this file is still open for writing (ETXTBSY);
        // wait until it can be executed before handing it out.
        for _ in 0..200 {
            match std::process::Command::new(&p).env("OPENRIP_PROBE", "1").output() {
                Err(e) if e.raw_os_error() == Some(26) => std::thread::sleep(std::time::Duration::from_millis(5)),
                _ => break,
            }
        }
        p
    }

    fn wait_for(mut cond: impl FnMut() -> bool) -> bool {
        for _ in 0..200 {
            if cond() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }

    #[test]
    fn frame_geometry_is_widescreen() {
        assert_eq!((WIDTH, HEIGHT), (854, 480));
        assert_eq!(WIDTH * HEIGHT * 4, 1_639_680);
        assert!(FPS > 0.0);
    }

    #[test]
    fn missing_ffmpeg_is_reported() {
        let dir = unique_dir("missing");
        let err = Session::start(&dir.join("nope"), &source(), 0.0, false, muted(), || {}).err().expect("error");
        assert!(err.starts_with("Couldn't start FFmpeg"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn protected_source_without_libbluray_or_disc_fails() {
        let dir = unique_dir("locked");
        let mut src = source();
        src.locked = Some((dir.clone(), 1));
        let result = Session::start(&dir.join("ffmpeg"), &src, 5.0, false, muted(), || {});
        assert!(result.is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn plays_frames_and_tracks_position() {
        let dir = unique_dir("play");
        // Two RGBA frames of zeros, then exit.
        let ffmpeg = fake_ffmpeg(&dir, &format!("head -c {} /dev/zero", WIDTH * HEIGHT * 4 * 2));
        let woke = Arc::new(AtomicU64::new(0));
        let w = woke.clone();
        let mut s = Session::start(&ffmpeg, &source(), 10.0, false, muted(), move || {
            w.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        assert_eq!(s.start, 10.0);
        assert!(!s.with_audio);
        assert!(wait_for(|| s.frame.lock().unwrap().is_some()), "a frame arrived");
        assert!(wait_for(|| s.position() >= 10.0 + 2.0 / FPS - 1e-9), "both frames shown");
        assert!(wait_for(|| s.ended()), "ended after the frames");
        assert!(!s.failed());
        assert_eq!(s.frame.lock().unwrap().as_ref().unwrap().len(), WIDTH * HEIGHT * 4);
        assert!((s.position() - (10.0 + 2.0 / FPS)).abs() < 1e-9, "{}", s.position());
        assert!(woke.load(Ordering::SeqCst) >= 2);
        drop(s);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn negative_start_is_clamped_and_seek_passed_to_ffmpeg() {
        let dir = unique_dir("args");
        let log = dir.join("args.txt");
        let ffmpeg = fake_ffmpeg(&dir, &format!("echo \"$@\" > '{}'", log.display()));
        let s = Session::start(&ffmpeg, &source(), -4.0, false, muted(), || {}).unwrap();
        assert_eq!(s.start, 0.0);
        assert!(wait_for(|| log.exists() && fs::metadata(&log).map(|m| m.len() > 0).unwrap_or(false)));
        let args = fs::read_to_string(&log).unwrap();
        assert!(args.contains("-ss 0.00 -i /disc/in.vob"), "{args}");
        assert!(args.contains("-f rawvideo -pix_fmt rgba pipe:1"), "{args}");
        assert!(args.contains(&format!("scale={WIDTH}:{HEIGHT}")), "{args}");
        assert!(!args.contains("f32le"), "no audio output requested: {args}");
        drop(s);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn failed_when_ffmpeg_exits_without_frames() {
        let dir = unique_dir("failed");
        let ffmpeg = fake_ffmpeg(&dir, "exit 1");
        let mut s = Session::start(&ffmpeg, &source(), 0.0, false, muted(), || {}).unwrap();
        assert!(wait_for(|| s.failed()));
        assert!(!s.ended());
        assert_eq!(s.position(), 0.0);
        drop(s);
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn dropping_session_stops_ffmpeg() {
        let dir = unique_dir("drop");
        let pidfile = dir.join("pid");
        let ffmpeg = fake_ffmpeg(&dir, &format!("echo $$ > '{}'\nexec sleep 30", pidfile.display()));
        let s = Session::start(&ffmpeg, &source(), 0.0, false, muted(), || {}).unwrap();
        assert!(wait_for(|| fs::read_to_string(&pidfile).map(|s| !s.trim().is_empty()).unwrap_or(false)));
        let pid = fs::read_to_string(&pidfile).unwrap().trim().to_string();
        drop(s);
        // Once dropped, the process has been killed and reaped.
        let alive = std::process::Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status().unwrap().success();
        assert!(!alive, "ffmpeg should be gone");
        let _ = fs::remove_dir_all(&dir);
    }
}
