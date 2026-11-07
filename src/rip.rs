//! Output formats and the background worker that runs FFmpeg for each title.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Video,
    Audio,
    Device,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Video {
    H264,
    H265,
    Mpeg4,
    Copy,
    None,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Audio {
    Aac,
    Mp3,
    Flac,
    Wav,
    Copy,
}

#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub name: &'static str,
    pub note: &'static str,
    pub group: Group,
    pub ext: &'static str,
    video: Video,
    audio: Audio,
    /// Largest height this profile makes, if it caps it.
    max_height: Option<u32>,
    /// Container that can carry the disc's subtitle tracks.
    pub keeps_subs: bool,
}

const fn p(
    name: &'static str,
    note: &'static str,
    group: Group,
    ext: &'static str,
    video: Video,
    audio: Audio,
    max_height: Option<u32>,
    keeps_subs: bool,
) -> Profile {
    Profile { name, note, group, ext, video, audio, max_height, keeps_subs }
}

pub const PROFILES: &[Profile] = &[
    p("MP4", "H.264 · plays everywhere", Group::Video, "mp4", Video::H264, Audio::Aac, None, false),
    p("MP4 HEVC", "H.265 · half the size", Group::Video, "mp4", Video::H265, Audio::Aac, None, false),
    p("MKV", "H.264 · keeps subtitles", Group::Video, "mkv", Video::H264, Audio::Aac, None, true),
    p("MKV HEVC", "H.265 · keeps subtitles", Group::Video, "mkv", Video::H265, Audio::Aac, None, true),
    p("MKV Lossless", "1:1 copy, all tracks", Group::Video, "mkv", Video::Copy, Audio::Copy, None, true),
    p("MOV", "H.264 · for Apple editors", Group::Video, "mov", Video::H264, Audio::Aac, None, false),
    p("AVI", "MPEG-4 · older players", Group::Video, "avi", Video::Mpeg4, Audio::Mp3, None, false),
    p("MPG", "MPEG-2 copy · DVD quality", Group::Video, "mpg", Video::Copy, Audio::Copy, None, false),
    p("MP3", "Audio only", Group::Audio, "mp3", Video::None, Audio::Mp3, None, false),
    p("M4A", "AAC audio only", Group::Audio, "m4a", Video::None, Audio::Aac, None, false),
    p("FLAC", "Lossless audio", Group::Audio, "flac", Video::None, Audio::Flac, None, false),
    p("WAV", "Uncompressed audio", Group::Audio, "wav", Video::None, Audio::Wav, None, false),
    p("iPhone", "MP4 · 1080p", Group::Device, "mp4", Video::H264, Audio::Aac, Some(1080), false),
    p("iPad", "MP4 · 1080p", Group::Device, "mp4", Video::H264, Audio::Aac, Some(1080), false),
    p("Apple TV", "MP4 HEVC · 4K", Group::Device, "mp4", Video::H265, Audio::Aac, Some(2160), false),
    p("Android", "MP4 · 720p", Group::Device, "mp4", Video::H264, Audio::Aac, Some(720), false),
    p("Smart TV", "MKV · 1080p", Group::Device, "mkv", Video::H264, Audio::Aac, Some(1080), true),
    p("YouTube", "MP4 · 1080p", Group::Device, "mp4", Video::H264, Audio::Aac, Some(1080), false),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Resolution {
    Source,
    P1080,
    P720,
    P480,
}

impl Resolution {
    pub const ALL: [Resolution; 4] = [Self::Source, Self::P1080, Self::P720, Self::P480];
    pub fn label(self) -> &'static str {
        match self {
            Self::Source => "Same as source",
            Self::P1080 => "1080p",
            Self::P720 => "720p",
            Self::P480 => "480p",
        }
    }
    fn height(self) -> Option<u32> {
        match self {
            Self::Source => None,
            Self::P1080 => Some(1080),
            Self::P720 => Some(720),
            Self::P480 => Some(480),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    High,
    Standard,
    Small,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Self::High, Self::Standard, Self::Small];
    pub fn label(self) -> &'static str {
        match self {
            Self::High => "High",
            Self::Standard => "Standard",
            Self::Small => "Small file",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub resolution: Resolution,
    pub quality: Quality,
    pub hardware: bool,
    pub deinterlace: bool,
}

#[derive(Clone, Debug)]
pub struct Job {
    /// Index of the title in the list, used to report progress back.
    pub slot: usize,
    pub input: String,
    pub duration: Option<f64>,
    pub audio_track: Option<usize>,
    pub subtitles: bool,
    pub output: PathBuf,
    pub profile: Profile,
    /// Disc folder and playlist of a protected Blu-ray, read through libbluray.
    pub locked: Option<(PathBuf, u32)>,
}

pub enum Msg {
    Started(usize),
    Progress { slot: usize, fraction: f32, speed: f32 },
    Finished { slot: usize, result: Result<PathBuf, String> },
    Merged(Result<PathBuf, String>),
    AllDone,
}

/// Shared with the UI so it can stop the current job.
#[derive(Default, Clone)]
pub struct Control {
    pub cancel: Arc<AtomicBool>,
    pid: Arc<AtomicU32>,
}

impl Control {
    pub fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        let pid = self.pid.load(Ordering::SeqCst);
        if pid != 0 {
            #[cfg(windows)]
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .status();
            #[cfg(unix)]
            let _ = Command::new("kill").arg(pid.to_string()).status();
        }
    }
}

fn args_for(job: &Job, s: &Settings) -> Vec<String> {
    let pr = &job.profile;
    let mut a: Vec<String> = Vec::new();
    let mut push = |items: &[&str]| a.extend(items.iter().map(|x| x.to_string()));
    push(&["-hide_banner", "-nostdin", "-y", "-analyzeduration", "100M", "-probesize", "100M"]);
    if job.locked.is_some() {
        // Unlocked data arrives on stdin as a transport stream.
        push(&["-fflags", "+genpts", "-f", "mpegts", "-i", "pipe:0"]);
    } else {
        push(&["-fflags", "+genpts", "-i", &job.input]);
    }

    if pr.video != Video::None {
        push(&["-map", "0:v:0"]);
    }
    let audio_map = match job.audio_track {
        Some(n) => format!("0:a:{n}?"),
        None => "0:a:0?".into(),
    };
    if pr.video == Video::Copy && pr.audio == Audio::Copy && pr.keeps_subs {
        push(&["-map", "0:a?"]);
    } else {
        push(&["-map", &audio_map]);
    }
    if pr.keeps_subs && job.subtitles && pr.video != Video::None {
        push(&["-map", "0:s?", "-c:s", "copy"]);
    }

    let mut filters: Vec<String> = Vec::new();
    if s.deinterlace && !matches!(pr.video, Video::Copy | Video::None) {
        filters.push("bwdif=mode=send_frame:deint=interlaced".into());
    }
    let cap = match (s.resolution.height(), pr.max_height) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    if let Some(h) = cap.filter(|_| !matches!(pr.video, Video::Copy | Video::None)) {
        filters.push(format!("scale=-2:'min({h},ih)'"));
    }
    if !filters.is_empty() {
        push(&["-vf", &filters.join(",")]);
    }

    let (crf, vt_q) = match s.quality {
        Quality::High => ("18", "70"),
        Quality::Standard => ("21", "58"),
        Quality::Small => ("25", "45"),
    };
    match pr.video {
        Video::H264 if s.hardware => push(&["-c:v", "h264_videotoolbox", "-q:v", vt_q]),
        Video::H264 => push(&["-c:v", "libx264", "-preset", "medium", "-crf", crf, "-pix_fmt", "yuv420p"]),
        Video::H265 if s.hardware => push(&["-c:v", "hevc_videotoolbox", "-q:v", vt_q, "-tag:v", "hvc1"]),
        Video::H265 => push(&["-c:v", "libx265", "-preset", "medium", "-crf", crf, "-tag:v", "hvc1", "-pix_fmt", "yuv420p"]),
        Video::Mpeg4 => push(&["-c:v", "mpeg4", "-q:v", "4", "-vtag", "xvid"]),
        Video::Copy => push(&["-c:v", "copy"]),
        Video::None => push(&["-vn"]),
    }
    match pr.audio {
        Audio::Aac => push(&["-c:a", "aac", "-b:a", "192k", "-ac", "2"]),
        Audio::Mp3 => push(&["-c:a", "libmp3lame", "-b:a", "192k"]),
        Audio::Flac => push(&["-c:a", "flac"]),
        Audio::Wav => push(&["-c:a", "pcm_s16le"]),
        Audio::Copy => push(&["-c:a", "copy"]),
    }
    if pr.ext == "mp4" || pr.ext == "mov" || pr.ext == "m4a" {
        push(&["-movflags", "+faststart"]);
    }
    push(&["-progress", "pipe:1", "-nostats"]);
    a.push(job.output.to_string_lossy().into_owned());
    a
}

/// A file name in `dir` that isn't taken yet.
pub fn free_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let clean: String = stem
        .chars()
        .map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '_' })
        .collect();
    let mut path = dir.join(format!("{clean}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{clean} ({n}).{ext}"));
        n += 1;
    }
    path
}

fn run_one(
    ffmpeg: &Path,
    job: &Job,
    settings: &Settings,
    control: &Control,
    tx: &Sender<Msg>,
    wake: &dyn Fn(),
) -> Result<PathBuf, String> {
    // A protected Blu-ray is opened first, so a missing key is reported straight away.
    let reader = match &job.locked {
        Some((root, playlist)) => Some(crate::bluray::Reader::open(root, *playlist)?),
        None => None,
    };
    let mut child = Command::new(ffmpeg)
        .args(args_for(job, settings))
        .stdin(if reader.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Couldn't start FFmpeg: {e}"))?;
    control.pid.store(child.id(), Ordering::SeqCst);

    // Keep the end of FFmpeg's log for the error message.
    let tail = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut stderr = child.stderr.take().unwrap();
    let tail2 = tail.clone();
    let err_thread = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        let mut t = tail2.lock().unwrap();
        *t = s.lines().rev().take(4).map(str::to_string).collect();
        t.reverse();
    });

    // Feed the unlocked stream to FFmpeg from its own thread.
    let read_error = Arc::new(Mutex::new(None::<String>));
    let feeder = reader.map(|mut reader| {
        let mut stdin = child.stdin.take().unwrap();
        let read_error = read_error.clone();
        let cancel = control.cancel.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 6144 * 64];
            while !cancel.load(Ordering::SeqCst) {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if stdin.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        *read_error.lock().unwrap() = Some(e);
                        break;
                    }
                }
            }
        })
    });

    let stdout = child.stdout.take().unwrap();
    let mut speed = 0.0f32;
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        if let Some(v) = line.strip_prefix("speed=") {
            speed = v.trim().trim_end_matches('x').parse().unwrap_or(speed);
        } else if let Some(v) = line.strip_prefix("out_time_us=") {
            if let (Ok(us), Some(d)) = (v.trim().parse::<f64>(), job.duration) {
                let fraction = (us / 1e6 / d).clamp(0.0, 1.0) as f32;
                let _ = tx.send(Msg::Progress { slot: job.slot, fraction, speed });
                wake();
            }
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    control.pid.store(0, Ordering::SeqCst);
    let _ = err_thread.join();
    if let Some(f) = feeder {
        let _ = f.join();
    }
    if let Some(e) = read_error.lock().unwrap().take() {
        let _ = std::fs::remove_file(&job.output);
        return Err(e);
    }

    if control.cancel.load(Ordering::SeqCst) {
        let _ = std::fs::remove_file(&job.output);
        return Err("Stopped".into());
    }
    if status.success() {
        Ok(job.output.clone())
    } else {
        let _ = std::fs::remove_file(&job.output);
        let msg = tail.lock().unwrap().join(" ");
        let hint = if msg.contains("Invalid data") || msg.contains("could not find codec") {
            " (the disc may be copy-protected)"
        } else {
            ""
        };
        Err(format!("FFmpeg failed{hint}: {msg}"))
    }
}

fn merge(ffmpeg: &Path, parts: &[PathBuf], out: &Path) -> Result<PathBuf, String> {
    let list = std::env::temp_dir().join(format!("disc-ripper-merge-{}.txt", std::process::id()));
    let body: String = parts
        .iter()
        .map(|p| format!("file '{}'\n", crate::disc::path_for_ffmpeg(p).replace('\'', "'\\''")))
        .collect();
    std::fs::write(&list, body).map_err(|e| e.to_string())?;
    let out_status = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-y", "-v", "error", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list)
        .args(["-map", "0", "-c", "copy"])
        .arg(out)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&list);
    if out_status.status.success() {
        for p in parts {
            let _ = std::fs::remove_file(p);
        }
        Ok(out.to_path_buf())
    } else {
        Err(String::from_utf8_lossy(&out_status.stderr).trim().to_string())
    }
}

/// Rip every job in turn on a background thread.
pub fn start(
    ffmpeg: PathBuf,
    jobs: Vec<Job>,
    settings: Settings,
    merge_to: Option<PathBuf>,
    control: Control,
    tx: Sender<Msg>,
    wake: impl Fn() + Send + 'static,
) {
    std::thread::spawn(move || {
        let mut done = Vec::new();
        for job in &jobs {
            if control.cancel.load(Ordering::SeqCst) {
                break;
            }
            let _ = tx.send(Msg::Started(job.slot));
            wake();
            let result = run_one(&ffmpeg, job, &settings, &control, &tx, &wake);
            if let Ok(p) = &result {
                done.push(p.clone());
            }
            let _ = tx.send(Msg::Finished { slot: job.slot, result });
            wake();
        }
        if let Some(out) = merge_to.filter(|_| done.len() > 1 && !control.cancel.load(Ordering::SeqCst)) {
            let _ = tx.send(Msg::Merged(merge(&ffmpeg, &done, &out)));
        }
        let _ = tx.send(Msg::AllDone);
        wake();
    });
}
