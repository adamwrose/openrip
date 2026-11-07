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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn profile(name: &str) -> Profile {
        *PROFILES.iter().find(|p| p.name == name).unwrap_or_else(|| panic!("no profile {name}"))
    }

    fn job(profile: Profile) -> Job {
        Job {
            slot: 3,
            input: "/disc/in.vob".into(),
            duration: Some(100.0),
            audio_track: None,
            subtitles: false,
            output: PathBuf::from("/out/movie.mp4"),
            profile,
            locked: None,
        }
    }

    /// merge() names its list file after the process id, so tests that merge take turns.
    static MERGE_LOCK: Mutex<()> = Mutex::new(());

    fn settings() -> Settings {
        Settings { resolution: Resolution::Source, quality: Quality::Standard, hardware: false, deinterlace: false }
    }

    fn has_pair(a: &[String], k: &str, v: &str) -> bool {
        a.windows(2).any(|w| w[0] == k && w[1] == v)
    }

    fn unique_dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("openrip-rip-unit-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn profiles_are_unique_and_sane() {
        let mut names: Vec<_> = PROFILES.iter().map(|p| p.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), PROFILES.len());
        for p in PROFILES {
            assert!(!p.ext.is_empty() && !p.note.is_empty());
            if p.group == Group::Audio {
                assert!(!p.keeps_subs, "{}", p.name);
            }
            if p.keeps_subs {
                assert_eq!(p.ext, "mkv", "only mkv carries subtitles: {}", p.name);
            }
        }
    }

    #[test]
    fn resolution_and_quality_labels() {
        assert_eq!(Resolution::ALL.len(), 4);
        assert_eq!(Resolution::Source.label(), "Same as source");
        assert_eq!(Resolution::P1080.label(), "1080p");
        assert_eq!(Resolution::P720.label(), "720p");
        assert_eq!(Resolution::P480.label(), "480p");
        assert_eq!(Resolution::Source.height(), None);
        assert_eq!(Resolution::P480.height(), Some(480));
        assert_eq!(Quality::ALL.len(), 3);
        assert_eq!(Quality::High.label(), "High");
        assert_eq!(Quality::Standard.label(), "Standard");
        assert_eq!(Quality::Small.label(), "Small file");
    }

    #[test]
    fn args_mp4_software_defaults() {
        let a = args_for(&job(profile("MP4")), &settings());
        assert!(has_pair(&a, "-i", "/disc/in.vob"));
        assert!(has_pair(&a, "-map", "0:v:0"));
        assert!(has_pair(&a, "-map", "0:a:0?"));
        assert!(has_pair(&a, "-c:v", "libx264"));
        assert!(has_pair(&a, "-crf", "21"));
        assert!(has_pair(&a, "-c:a", "aac"));
        assert!(has_pair(&a, "-movflags", "+faststart"));
        assert!(!a.contains(&"-vf".to_string()));
        assert!(!a.contains(&"0:s?".to_string()));
        assert_eq!(a.last().unwrap(), "/out/movie.mp4");
    }

    #[test]
    fn args_quality_maps_to_crf() {
        for (q, crf) in [(Quality::High, "18"), (Quality::Standard, "21"), (Quality::Small, "25")] {
            let s = Settings { quality: q, ..settings() };
            assert!(has_pair(&args_for(&job(profile("MP4")), &s), "-crf", crf));
            assert!(has_pair(&args_for(&job(profile("MP4 HEVC")), &s), "-crf", crf));
        }
    }

    #[test]
    fn args_quality_maps_to_hardware_q() {
        for (q, v) in [(Quality::High, "70"), (Quality::Standard, "58"), (Quality::Small, "45")] {
            let s = Settings { quality: q, hardware: true, ..settings() };
            let a = args_for(&job(profile("MP4")), &s);
            assert!(has_pair(&a, "-c:v", "h264_videotoolbox"));
            assert!(has_pair(&a, "-q:v", v));
        }
    }

    #[test]
    fn args_hevc_software_and_hardware() {
        let a = args_for(&job(profile("MP4 HEVC")), &settings());
        assert!(has_pair(&a, "-c:v", "libx265"));
        assert!(has_pair(&a, "-tag:v", "hvc1"));
        let a = args_for(&job(profile("MP4 HEVC")), &Settings { hardware: true, ..settings() });
        assert!(has_pair(&a, "-c:v", "hevc_videotoolbox"));
    }

    #[test]
    fn args_audio_track_selection() {
        let mut j = job(profile("MP4"));
        j.audio_track = Some(2);
        assert!(has_pair(&args_for(&j, &settings()), "-map", "0:a:2?"));
    }

    #[test]
    fn args_subtitles_only_when_container_keeps_them() {
        let mut j = job(profile("MKV"));
        j.subtitles = true;
        let a = args_for(&j, &settings());
        assert!(has_pair(&a, "-map", "0:s?"));
        assert!(has_pair(&a, "-c:s", "copy"));

        j.subtitles = false;
        assert!(!args_for(&j, &settings()).contains(&"0:s?".to_string()));

        let mut j = job(profile("MP4"));
        j.subtitles = true;
        assert!(!args_for(&j, &settings()).contains(&"0:s?".to_string()));

        let mut j = job(profile("FLAC"));
        j.subtitles = true;
        assert!(!args_for(&j, &settings()).contains(&"0:s?".to_string()));
    }

    #[test]
    fn args_lossless_copies_all_audio_without_filters() {
        let s = Settings { resolution: Resolution::P480, deinterlace: true, ..settings() };
        let a = args_for(&job(profile("MKV Lossless")), &s);
        assert!(has_pair(&a, "-map", "0:a?"));
        assert!(has_pair(&a, "-c:v", "copy"));
        assert!(has_pair(&a, "-c:a", "copy"));
        assert!(!a.contains(&"-vf".to_string()));
        assert!(!a.contains(&"-movflags".to_string()));
    }

    #[test]
    fn args_mpg_copy_maps_first_audio_only() {
        let a = args_for(&job(profile("MPG")), &settings());
        assert!(has_pair(&a, "-map", "0:a:0?"));
        assert!(!a.contains(&"0:a?".to_string()));
    }

    #[test]
    fn args_audio_only_profiles() {
        for (name, codec) in [("MP3", "libmp3lame"), ("M4A", "aac"), ("FLAC", "flac"), ("WAV", "pcm_s16le")] {
            let a = args_for(&job(profile(name)), &Settings { deinterlace: true, resolution: Resolution::P720, ..settings() });
            assert!(!has_pair(&a, "-map", "0:v:0"), "{name}");
            assert!(a.contains(&"-vn".to_string()), "{name}");
            assert!(has_pair(&a, "-c:a", codec), "{name}");
            assert!(!a.contains(&"-vf".to_string()), "{name}");
        }
        assert!(has_pair(&args_for(&job(profile("M4A")), &settings()), "-movflags", "+faststart"));
        assert!(!args_for(&job(profile("MP3")), &settings()).contains(&"-movflags".to_string()));
    }

    #[test]
    fn args_avi_uses_mpeg4_and_mp3() {
        let a = args_for(&job(profile("AVI")), &settings());
        assert!(has_pair(&a, "-c:v", "mpeg4"));
        assert!(has_pair(&a, "-c:a", "libmp3lame"));
    }

    #[test]
    fn args_filters_deinterlace_and_scale() {
        let s = Settings { deinterlace: true, resolution: Resolution::P720, ..settings() };
        let a = args_for(&job(profile("MP4")), &s);
        let i = a.iter().position(|x| x == "-vf").expect("vf");
        assert_eq!(a[i + 1], "bwdif=mode=send_frame:deint=interlaced,scale=-2:'min(720,ih)'");
    }

    #[test]
    fn args_scale_uses_smaller_of_user_and_device_cap() {
        let s = Settings { resolution: Resolution::P1080, ..settings() };
        let a = args_for(&job(profile("Android")), &s); // cap 720
        assert!(a.iter().any(|x| x == "scale=-2:'min(720,ih)'"));
        let s = Settings { resolution: Resolution::P480, ..settings() };
        let a = args_for(&job(profile("iPhone")), &s); // cap 1080
        assert!(a.iter().any(|x| x == "scale=-2:'min(480,ih)'"));
        // Device cap applies on its own when the user keeps the source size.
        let a = args_for(&job(profile("iPhone")), &settings());
        assert!(a.iter().any(|x| x == "scale=-2:'min(1080,ih)'"));
    }

    #[test]
    fn args_locked_reads_from_stdin() {
        let mut j = job(profile("MP4"));
        j.locked = Some((PathBuf::from("/bd"), 800));
        let a = args_for(&j, &settings());
        assert!(has_pair(&a, "-f", "mpegts"));
        assert!(has_pair(&a, "-i", "pipe:0"));
        assert!(!a.contains(&"/disc/in.vob".to_string()));
    }

    #[test]
    fn args_report_progress_on_stdout() {
        let a = args_for(&job(profile("MP4")), &settings());
        assert!(has_pair(&a, "-progress", "pipe:1"));
        assert!(a.contains(&"-nostats".to_string()));
    }

    #[test]
    fn free_path_sanitises_and_avoids_collisions() {
        let dir = unique_dir("free_path");
        assert_eq!(free_path(&dir, "My: Movie/?", "mp4"), dir.join("My_ Movie__.mp4"));
        assert_eq!(free_path(&dir, "Keep (1)-a_b.c", "mkv"), dir.join("Keep (1)-a_b.c.mkv"));
        std::fs::write(dir.join("Film.mp4"), b"x").unwrap();
        assert_eq!(free_path(&dir, "Film", "mp4"), dir.join("Film (2).mp4"));
        std::fs::write(dir.join("Film (2).mp4"), b"x").unwrap();
        assert_eq!(free_path(&dir, "Film", "mp4"), dir.join("Film (3).mp4"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn free_path_keeps_unicode_letters() {
        let dir = unique_dir("free_path_unicode");
        assert_eq!(free_path(&dir, "Amélie", "mp4"), dir.join("Amélie.mp4"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn control_stop_sets_cancel() {
        let c = Control::default();
        assert!(!c.cancel.load(Ordering::SeqCst));
        let c2 = c.clone();
        c2.stop();
        assert!(c.cancel.load(Ordering::SeqCst), "clones share the flag");
    }

    #[cfg(unix)]
    fn fake_ffmpeg(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("ffmpeg");
        std::fs::write(&p, format!("#!/bin/sh\n[ -n \"$OPENRIP_PROBE\" ] && exit 0\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
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

    #[cfg(unix)]
    fn run(ffmpeg: PathBuf, jobs: Vec<Job>, merge_to: Option<PathBuf>, control: Control) -> Vec<Msg> {
        let (tx, rx) = channel();
        start(ffmpeg, jobs, settings(), merge_to, control, tx, || {});
        let mut out = Vec::new();
        for m in rx.iter() {
            let done = matches!(m, Msg::AllDone);
            out.push(m);
            if done {
                break;
            }
        }
        out
    }

    #[cfg(unix)]
    #[test]
    fn start_reports_progress_and_success() {
        let dir = unique_dir("run_ok");
        // The output path is the last argument; create it, and emit progress lines.
        let ffmpeg = fake_ffmpeg(
            &dir,
            r#"for a; do out="$a"; done
echo data > "$out"
echo out_time_us=50000000
echo speed=2.5x
echo out_time_us=500000000
echo progress=end"#,
        );
        let mut j = job(profile("MP4"));
        j.output = dir.join("out.mp4");
        let msgs = run(ffmpeg, vec![j], None, Control::default());
        assert!(matches!(msgs[0], Msg::Started(3)));
        let fractions: Vec<f32> = msgs
            .iter()
            .filter_map(|m| match m {
                Msg::Progress { slot: 3, fraction, .. } => Some(*fraction),
                _ => None,
            })
            .collect();
        assert_eq!(fractions, vec![0.5, 1.0], "second value is clamped");
        assert!(msgs.iter().any(|m| matches!(m, Msg::Progress { speed, .. } if *speed == 2.5)));
        assert!(msgs.iter().any(|m| matches!(m, Msg::Finished { slot: 3, result: Ok(p) } if *p == dir.join("out.mp4"))));
        assert!(matches!(msgs.last().unwrap(), Msg::AllDone));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn start_reports_failure_with_hint_and_removes_output() {
        let dir = unique_dir("run_fail");
        let ffmpeg = fake_ffmpeg(
            &dir,
            r#"for a; do out="$a"; done
echo partial > "$out"
echo "Invalid data found when processing input" >&2
exit 1"#,
        );
        let mut j = job(profile("MP4"));
        j.output = dir.join("out.mp4");
        let msgs = run(ffmpeg, vec![j], None, Control::default());
        let err = msgs
            .iter()
            .find_map(|m| match m {
                Msg::Finished { result: Err(e), .. } => Some(e.clone()),
                _ => None,
            })
            .expect("failure reported");
        assert!(err.starts_with("FFmpeg failed (the disc may be copy-protected):"), "{err}");
        assert!(err.contains("Invalid data"));
        assert!(!dir.join("out.mp4").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn start_failure_without_hint() {
        let dir = unique_dir("run_fail_plain");
        let ffmpeg = fake_ffmpeg(&dir, "echo 'something else' >&2\nexit 1");
        let mut j = job(profile("MP4"));
        j.output = dir.join("out.mp4");
        let msgs = run(ffmpeg, vec![j], None, Control::default());
        let err = msgs.iter().find_map(|m| if let Msg::Finished { result: Err(e), .. } = m { Some(e.clone()) } else { None }).unwrap();
        assert_eq!(err, "FFmpeg failed: something else");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn start_missing_ffmpeg_is_an_error() {
        let dir = unique_dir("run_missing");
        let mut j = job(profile("MP4"));
        j.output = dir.join("out.mp4");
        let msgs = run(dir.join("no-such-ffmpeg"), vec![j], None, Control::default());
        assert!(msgs.iter().any(|m| matches!(m, Msg::Finished { result: Err(e), .. } if e.starts_with("Couldn't start FFmpeg"))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn start_cancelled_before_begin_runs_nothing() {
        let dir = unique_dir("run_cancel");
        let ffmpeg = fake_ffmpeg(&dir, "exit 0");
        let control = Control::default();
        control.cancel.store(true, Ordering::SeqCst);
        let msgs = run(ffmpeg, vec![job(profile("MP4"))], Some(dir.join("merged.mkv")), control);
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0], Msg::AllDone));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn start_merges_multiple_results() {
        let _guard = MERGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = unique_dir("run_merge");
        // Both rips and the merge go through the same fake: it touches its last argument.
        let ffmpeg = fake_ffmpeg(&dir, "for a; do out=\"$a\"; done\necho x > \"$out\"");
        let jobs: Vec<Job> = (0..2)
            .map(|i| {
                let mut j = job(profile("MKV"));
                j.slot = i;
                j.output = dir.join(format!("part{i}.mkv"));
                j
            })
            .collect();
        let merged = dir.join("merged.mkv");
        let msgs = run(ffmpeg, jobs, Some(merged.clone()), Control::default());
        assert!(msgs.iter().any(|m| matches!(m, Msg::Merged(Ok(p)) if *p == merged)));
        assert!(merged.exists());
        assert!(!dir.join("part0.mkv").exists() && !dir.join("part1.mkv").exists(), "parts removed after merge");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn merge_failure_keeps_parts_and_reports_stderr() {
        let _guard = MERGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = unique_dir("merge_fail");
        let ffmpeg = fake_ffmpeg(&dir, "echo 'concat broke' >&2\nexit 1");
        let part = dir.join("p.mkv");
        std::fs::write(&part, b"x").unwrap();
        let err = merge(&ffmpeg, &[part.clone()], &dir.join("o.mkv")).unwrap_err();
        assert_eq!(err, "concat broke");
        assert!(part.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn merge_escapes_quotes_in_list_file() {
        let _guard = MERGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = unique_dir("merge_quote");
        // The fake copies the concat list (the argument after -i) somewhere we can read.
        let ffmpeg = fake_ffmpeg(
            &dir,
            &format!("while [ $# -gt 0 ]; do [ \"$1\" = -i ] && cp \"$2\" '{}/list.txt'; last=\"$1\"; shift; done\necho x > \"$last\"", dir.display()),
        );
        let part = dir.join("it's.mkv");
        std::fs::write(&part, b"x").unwrap();
        merge(&ffmpeg, &[part.clone()], &dir.join("o.mkv")).unwrap();
        let list = std::fs::read_to_string(dir.join("list.txt")).unwrap();
        assert_eq!(list, format!("file '{}/it'\\''s.mkv'\n", dir.display()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
