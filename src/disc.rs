//! Finding discs and reading what titles are on them.
//!
//! DVDs are read from their VIDEO_TS folder (each title is a run of VTS_NN_M.VOB files),
//! Blu-rays from BDMV/STREAM (each clip is an .m2ts file). ISO images are mounted read-only
//! with the system's `hdiutil` first. Details (length, audio, subtitles, a preview frame)
//! come from FFmpeg's `ffprobe` and `ffmpeg`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A `Command` that doesn't flash a console window when run from the Windows GUI app.
pub fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiscKind {
    Dvd,
    BluRay,
}

impl DiscKind {
    pub fn label(self) -> &'static str {
        match self {
            DiscKind::Dvd => "DVD",
            DiscKind::BluRay => "Blu-ray",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Stream {
    pub codec: String,
    pub language: Option<String>,
    pub channels: Option<u32>,
}

impl Stream {
    pub fn describe(&self, n: usize) -> String {
        let lang = self
            .language
            .as_deref()
            .filter(|l| !l.is_empty() && *l != "und")
            .map(|l| format!("{} ", l.to_uppercase()))
            .unwrap_or_default();
        let ch = match self.channels {
            Some(1) => " mono",
            Some(2) => " stereo",
            Some(6) => " 5.1",
            Some(8) => " 7.1",
            _ => "",
        };
        format!("{n}. {lang}{}{ch}", self.codec.to_uppercase())
    }
}

#[derive(Clone, Debug, Default)]
pub struct TitleInfo {
    pub duration: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub audio: Vec<Stream>,
    pub subtitles: Vec<Stream>,
    /// RGBA preview frame, width and height.
    pub thumb: Option<(Vec<u8>, usize, usize)>,
}

#[derive(Clone, Debug)]
pub struct Title {
    pub number: usize,
    pub files: Vec<PathBuf>,
    pub size: u64,
    /// Where it comes from on the disc ("playlist 00800"), empty for DVD titles.
    pub label: String,
    /// Running time known without probing (Blu-ray playlists carry it).
    pub duration: Option<f64>,
    /// Blu-ray playlist number.
    pub playlist: Option<u32>,
    /// Disc folder, when this is a copy-protected Blu-ray read through libbluray.
    pub locked: Option<PathBuf>,
}

impl Title {
    /// What to hand FFmpeg as input: one file, or the concat protocol for VOB runs.
    pub fn input(&self) -> String {
        if self.files.len() == 1 {
            path_for_ffmpeg(&self.files[0])
        } else {
            let parts: Vec<String> = self
                .files
                .iter()
                .map(|f| path_for_ffmpeg(f))
                .collect();
            format!("concat:{}", parts.join("|"))
        }
    }
}

pub fn path_for_ffmpeg(path: &Path) -> String {
    let path = path.to_string_lossy();
    #[cfg(windows)]
    {
        path.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        path.into_owned()
    }
}

#[derive(Clone, Debug)]
pub struct Disc {
    pub kind: DiscKind,
    pub name: String,
    pub titles: Vec<Title>,
    /// Copy-protected (AACS) Blu-ray, which FFmpeg can't read.
    pub encrypted: bool,
}

impl Disc {
    /// The main movie: the title holding the most video. Not simply the longest, since
    /// protected Blu-rays add hours-long decoy playlists made of tiny clips.
    pub fn main_title(&self) -> Option<usize> {
        self.titles
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| {
                a.size.cmp(&b.size).then(a.duration.unwrap_or(0.0).total_cmp(&b.duration.unwrap_or(0.0)))
            })
            .map(|(i, _)| i)
    }
}

pub struct MountedDisc {
    pub name: String,
    pub path: PathBuf,
    pub kind: DiscKind,
}

fn which(name: &str) -> Option<PathBuf> {
    // A tool shipped next to the app (as the Windows installer does) wins over the PATH.
    let mut dirs: Vec<PathBuf> = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .into_iter()
        .collect();
    dirs.extend(std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).unwrap_or_default());

    #[cfg(target_os = "macos")]
    for d in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin", "/usr/bin"] {
        dirs.push(PathBuf::from(d));
    }
    #[cfg(target_os = "linux")]
    for d in ["/usr/local/bin", "/usr/bin", "/bin"] {
        dirs.push(PathBuf::from(d));
    }
    #[cfg(target_os = "windows")]
    for d in [
        std::env::var_os("ProgramFiles").map(PathBuf::from).map(|p| p.join("FFmpeg/bin")),
        std::env::var_os("ProgramFiles(x86)").map(PathBuf::from).map(|p| p.join("FFmpeg/bin")),
    ].into_iter().flatten() {
        dirs.push(d);
    }

    let names = if cfg!(windows) && Path::new(name).extension().is_none() {
        vec![name.to_string(), format!("{name}.exe")]
    } else {
        vec![name.to_string()]
    };
    for dir in dirs {
        for candidate in &names {
            let path = dir.join(candidate);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

pub fn find_tool(name: &str) -> Option<PathBuf> {
    which(name)
}

/// The ffprobe next to the chosen ffmpeg, or any ffprobe we can find.
pub fn ffprobe_for(ffmpeg: &Path) -> Option<PathBuf> {
    let sibling = ffmpeg.with_file_name(if cfg!(windows) { "ffprobe.exe" } else { "ffprobe" });
    if sibling.is_file() { Some(sibling) } else { which("ffprobe") }
}

fn kind_of(root: &Path) -> Option<DiscKind> {
    if root.join("VIDEO_TS").is_dir() {
        Some(DiscKind::Dvd)
    } else if root.join("BDMV").is_dir() {
        Some(DiscKind::BluRay)
    } else {
        None
    }
}

/// Discs (and disc images) currently mounted on this computer.
pub fn mounted_discs() -> Vec<MountedDisc> {
    let mut roots = Vec::new();
    #[cfg(target_os = "macos")]
    roots.push(PathBuf::from("/Volumes"));
    #[cfg(target_os = "linux")]
    {
        if let Some(user) = std::env::var_os("USER") {
            roots.push(PathBuf::from("/media").join(&user));
            roots.push(PathBuf::from("/run/media").join(user));
        }
        roots.push(PathBuf::from("/mnt"));
    }
    #[cfg(target_os = "windows")]
    for letter in b'A'..=b'Z' {
        roots.push(PathBuf::from(format!("{}:\\", letter as char)));
    }

    let mut out = Vec::new();
    for root in roots {
        #[cfg(windows)]
        {
            if let Some(kind) = kind_of(&root) {
                let name = root.to_string_lossy().trim_end_matches('\\').trim_end_matches('/').to_string();
                out.push(MountedDisc { name, path: root, kind });
            }
        }
        #[cfg(not(windows))]
        {
            let Ok(entries) = std::fs::read_dir(&root) else { continue };
            for e in entries.flatten() {
                let path = e.path();
                if let Some(kind) = kind_of(&path) {
                    out.push(MountedDisc {
                        name: e.file_name().to_string_lossy().into_owned(),
                        path,
                        kind,
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

#[cfg(target_os = "macos")]
fn mount_iso(iso: &Path) -> Result<PathBuf, String> {
    let out = Command::new("hdiutil")
        .args(["attach", "-readonly", "-nobrowse"])
        .arg(iso)
        .output()
        .map_err(|e| format!("Couldn't mount the image: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "Couldn't mount the image: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split('\t').last())
        .map(str::trim)
        .find(|p| p.starts_with('/'))
        .map(PathBuf::from)
        .ok_or_else(|| "The image mounted but no folder appeared.".to_string())
}

#[cfg(target_os = "linux")]
fn mount_iso(iso: &Path) -> Result<PathBuf, String> {
    let loop_device = Command::new("udisksctl")
        .args(["loop-setup", "--file"])
        .arg(iso)
        .arg("--no-user-interaction")
        .output()
        .map_err(|e| format!("Couldn't mount the image (install udisks2): {e}"))?;
    if !loop_device.status.success() {
        return Err(format!("Couldn't set up the image: {}", String::from_utf8_lossy(&loop_device.stderr).trim()));
    }
    let output = String::from_utf8_lossy(&loop_device.stdout);
    let device = output
        .split(" as ")
        .nth(1)
        .and_then(|s| s.trim().trim_end_matches('.').split_whitespace().next())
        .ok_or_else(|| "The image was set up but no loop device was reported.".to_string())?;
    let mounted = Command::new("udisksctl")
        .args(["mount", "--block-device", device, "--no-user-interaction"])
        .output()
        .map_err(|e| format!("Couldn't mount the image: {e}"))?;
    if !mounted.status.success() {
        return Err(format!("Couldn't mount the image: {}", String::from_utf8_lossy(&mounted.stderr).trim()));
    }
    String::from_utf8_lossy(&mounted.stdout)
        .split(" at ")
        .nth(1)
        .map(str::trim)
        .map(PathBuf::from)
        .ok_or_else(|| "The image mounted but no folder was reported.".to_string())
}

#[cfg(target_os = "windows")]
fn mount_iso(iso: &Path) -> Result<PathBuf, String> {
    let image = iso.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$ErrorActionPreference = 'Stop'; \
         try {{ \
           $image = Mount-DiskImage -ImagePath '{image}' -PassThru; \
           $volume = $image | Get-Volume | Where-Object DriveLetter | Select-Object -First 1; \
           if ($volume) {{ $volume.DriveLetter }} \
         }} catch {{ [Console]::Error.WriteLine($_); exit 1 }}"
    );
    let output = hidden_command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .map_err(|e| format!("Couldn't mount the image with Windows PowerShell: {e}"))?;
    if !output.status.success() {
        return Err(format!("Couldn't mount the image: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    let letter = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if letter.is_empty() {
        return Err("The image mounted but Windows didn't assign it a drive letter.".into());
    }
    Ok(PathBuf::from(format!("{letter}:\\")))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn mount_iso(_iso: &Path) -> Result<PathBuf, String> {
    Err("Mounting ISO images isn't supported on this platform.".into())
}

/// Open a directory, or reveal a file, in the platform's file manager.
pub fn open_location(path: &Path, reveal: bool) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = Command::new("open");
        if reveal {
            c.arg("-R");
        }
        c.arg(path);
        c
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = Command::new("explorer.exe");
        if reveal {
            c.arg("/select,").arg(path);
        } else {
            c.arg(path);
        }
        c
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let target = if reveal { path.parent().unwrap_or(path) } else { path };
        let mut c = Command::new("xdg-open");
        c.arg(target);
        c
    };
    command.spawn().map(|_| ()).map_err(|e| format!("Couldn't open the file manager: {e}"))
}

/// Read a disc from a mounted volume, a VIDEO_TS/BDMV folder (or its parent) or an ISO file.
pub fn open(path: &Path) -> Result<Disc, String> {
    let is_iso = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("iso"));
    let mut root = if is_iso { mount_iso(path)? } else { path.to_path_buf() };
    // Accept the VIDEO_TS / BDMV folder itself, or BDMV/STREAM.
    for _ in 0..2 {
        let leaf = root
            .file_name()
            .map(|n| n.to_string_lossy().to_uppercase())
            .unwrap_or_default();
        if matches!(leaf.as_str(), "VIDEO_TS" | "BDMV" | "STREAM") {
            if let Some(p) = root.parent() {
                root = p.to_path_buf();
            }
        }
    }
    let name = if is_iso {
        path.file_stem()
    } else {
        root.file_name()
    }
    .map(|n| n.to_string_lossy().into_owned())
    .unwrap_or_else(|| "Disc".into());

    match kind_of(&root) {
        Some(DiscKind::Dvd) => dvd_titles(&root.join("VIDEO_TS")).map(|titles| Disc {
            kind: DiscKind::Dvd,
            name,
            titles,
            encrypted: false,
        }),
        Some(DiscKind::BluRay) => bluray_titles(&root.join("BDMV")).map(|mut titles| {
            let encrypted = root.join("AACS").is_dir();
            if encrypted {
                for t in titles.iter_mut().filter(|t| t.playlist.is_some()) {
                    t.locked = Some(root.clone());
                }
            }
            Disc { kind: DiscKind::BluRay, name, titles, encrypted }
        }),
        None => Err("No DVD (VIDEO_TS) or Blu-ray (BDMV) folder was found there.".into()),
    }
}

fn dvd_titles(dir: &Path) -> Result<Vec<Title>, String> {
    let mut groups: std::collections::BTreeMap<usize, Vec<(usize, PathBuf, u64)>> =
        Default::default();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("Couldn't read the disc: {e}"))?;
    for e in entries.flatten() {
        let n = e.file_name().to_string_lossy().to_uppercase();
        // VTS_NN_M.VOB, M = 0 is the menu.
        let Some(rest) = n.strip_prefix("VTS_").and_then(|r| r.strip_suffix(".VOB")) else {
            continue;
        };
        let mut it = rest.split('_');
        let (Some(Ok(set)), Some(Ok(part))) = (
            it.next().map(str::parse::<usize>),
            it.next().map(str::parse::<usize>),
        ) else {
            continue;
        };
        if part == 0 {
            continue;
        }
        let size = e.metadata().map(|m| m.len()).unwrap_or(0);
        groups.entry(set).or_default().push((part, e.path(), size));
    }
    let titles: Vec<Title> = groups
        .into_values()
        .enumerate()
        .map(|(i, mut parts)| {
            parts.sort_by_key(|p| p.0);
            Title {
                number: i + 1,
                size: parts.iter().map(|p| p.2).sum(),
                files: parts.into_iter().map(|p| p.1).collect(),
                label: String::new(),
                duration: None,
                playlist: None,
                locked: None,
            }
        })
        .collect();
    if titles.is_empty() {
        Err("This DVD has no video titles.".into())
    } else {
        Ok(titles)
    }
}

/// Clip names and running time (seconds) of a Blu-ray playlist (.mpls) file.
fn parse_mpls(data: &[u8]) -> Option<(Vec<String>, f64)> {
    if data.get(0..4)? != b"MPLS" {
        return None;
    }
    let be16 = |at: usize| -> Option<usize> { Some(u16::from_be_bytes(data.get(at..at + 2)?.try_into().ok()?) as usize) };
    let be32 = |at: usize| -> Option<u32> { Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?)) };
    let start = be32(8)? as usize;
    let count = be16(start + 6)?;
    let mut pos = start + 10;
    let mut clips = Vec::with_capacity(count);
    let mut ticks = 0u64;
    for _ in 0..count {
        let len = be16(pos)?;
        let name = std::str::from_utf8(data.get(pos + 2..pos + 7)?).ok()?.to_string();
        let (t_in, t_out) = (be32(pos + 14)?, be32(pos + 18)?);
        ticks += t_out.saturating_sub(t_in) as u64;
        clips.push(name);
        pos += 2 + len;
    }
    // Playlist times count at 45 kHz.
    Some((clips, ticks as f64 / 45_000.0))
}

/// Blu-ray titles come from its playlists, the way a player finds the movie: one playlist
/// can join many clips.
fn bluray_titles(bdmv: &Path) -> Result<Vec<Title>, String> {
    let stream = bdmv.join("STREAM");
    let clip_path = |name: &str| {
        ["m2ts", "M2TS", "mts", "MTS"]
            .iter()
            .map(|ext| stream.join(format!("{name}.{ext}")))
            .find(|p| p.is_file())
    };
    let mut lists: Vec<(String, Vec<PathBuf>, f64)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(bdmv.join("PLAYLIST")) {
        let mut names: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("mpls")))
            .collect();
        names.sort();
        for path in names {
            let Some((clips, secs)) = std::fs::read(&path).ok().and_then(|d| parse_mpls(&d)) else {
                continue;
            };
            let Some(files) = clips.iter().map(|c| clip_path(c)).collect::<Option<Vec<_>>>() else {
                continue;
            };
            // Discs often repeat the same playlist under several numbers; keep one.
            if files.is_empty() || lists.iter().any(|(_, f, _)| *f == files) {
                continue;
            }
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            lists.push((stem, files, secs));
        }
    }
    if !lists.is_empty() {
        let mut titles: Vec<Title> = lists
            .into_iter()
            .map(|(stem, files, secs)| {
                let mut unique = files.clone();
                unique.sort();
                unique.dedup();
                Title {
                    number: 0,
                    size: unique.iter().filter_map(|f| f.metadata().ok()).map(|m| m.len()).sum(),
                    files,
                    playlist: stem.parse().ok(),
                    label: format!("playlist {stem}"),
                    duration: Some(secs),
                    locked: None,
                }
            })
            .collect();
        // Most video first, so the movie and real extras lead the list.
        titles.sort_by(|a, b| b.size.cmp(&a.size));
        for (i, t) in titles.iter_mut().enumerate() {
            t.number = i + 1;
        }
        return Ok(titles);
    }
    // No readable playlists: fall back to one title per clip.
    let entries = std::fs::read_dir(&stream).map_err(|e| format!("Couldn't read the disc: {e}"))?;
    let mut clips: Vec<(PathBuf, u64)> = entries
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("m2ts"))
        })
        .map(|e| (e.path(), e.metadata().map(|m| m.len()).unwrap_or(0)))
        .collect();
    clips.sort();
    let titles: Vec<Title> = clips
        .into_iter()
        .enumerate()
        .map(|(i, (p, size))| Title {
            number: i + 1,
            label: format!("clip {}", p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()),
            files: vec![p],
            size,
            duration: None,
            playlist: None,
            locked: None,
        })
        .collect();
    if titles.is_empty() {
        Err("This Blu-ray has no video clips.".into())
    } else {
        Ok(titles)
    }
}

/// Length, picture size and tracks of a title, plus a preview frame.
pub fn probe(ffmpeg: &Path, title: &Title) -> TitleInfo {
    let mut info = TitleInfo::default();
    // A protected title can't be read directly: probe a short unlocked sample instead.
    let sample = match (&title.locked, title.playlist) {
        (Some(root), Some(pl)) => {
            let path = std::env::temp_dir().join(format!("openrip-sample-{}-{pl}.ts", std::process::id()));
            let data = crate::bluray::Reader::open(root, pl).and_then(|mut r| r.sample(0.1, 24 << 20));
            match data.and_then(|d| std::fs::write(&path, d).map_err(|e| e.to_string())) {
                Ok(()) => Some(path),
                Err(_) => {
                    info.duration = title.duration;
                    return info;
                }
            }
        }
        _ => None,
    };
    let input = sample.as_ref().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| title.input());
    if let Some(ffprobe) = ffprobe_for(ffmpeg) {
        let out = hidden_command(&ffprobe)
            .args([
                "-v",
                "error",
                "-analyzeduration",
                "60M",
                "-probesize",
                "60M",
                "-show_entries",
                "format=duration:stream=codec_type,codec_name,channels,width,height:stream_tags=language",
                "-of",
                "compact=p=0",
            ])
            .arg(&input)
            .stdin(Stdio::null())
            .output();
        if let Ok(out) = out {
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let kv: Vec<(&str, &str)> =
                    line.split('|').filter_map(|p| p.split_once('=')).collect();
                let get = |k: &str| kv.iter().find(|(a, _)| *a == k).map(|(_, v)| *v);
                match get("codec_type") {
                    Some("video") if info.width.is_none() => {
                        info.width = get("width").and_then(|v| v.parse().ok());
                        info.height = get("height").and_then(|v| v.parse().ok());
                    }
                    Some(t @ ("audio" | "subtitle")) => {
                        let s = Stream {
                            codec: get("codec_name").unwrap_or("?").replace("_subtitle", ""),
                            language: get("tag:language").map(str::to_string),
                            channels: get("channels").and_then(|v| v.parse().ok()),
                        };
                        if t == "audio" {
                            info.audio.push(s)
                        } else {
                            info.subtitles.push(s)
                        }
                    }
                    None => {
                        if let Some(d) = get("duration").and_then(|v| v.parse::<f64>().ok()) {
                            info.duration = Some(d);
                        }
                    }
                    _ => {}
                }
            }
        }
        // FFmpeg misjudges the length of joined VOB files, so add up each part's length.
        if title.duration.is_some() {
            info.duration = title.duration;
        } else if title.files.len() > 1 {
            let parts: Option<Vec<f64>> = title
                .files
                .iter()
                .map(|f| {
                    let out = hidden_command(&ffprobe)
                        .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
                        .arg(f)
                        .stdin(Stdio::null())
                        .output()
                        .ok()?;
                    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
                })
                .collect();
            if let Some(parts) = parts {
                info.duration = Some(parts.iter().sum());
            }
        }
    }
    if let Some(path) = sample {
        info.thumb = thumbnail(ffmpeg, &input, 1.0);
        info.duration = title.duration;
        let _ = std::fs::remove_file(path);
        return info;
    }
    let at = info.duration.map(|d| (d * 0.1).min(90.0)).unwrap_or(5.0);
    info.thumb = thumbnail(ffmpeg, &input, at);
    info
}

fn thumbnail(ffmpeg: &Path, input: &str, at: f64) -> Option<(Vec<u8>, usize, usize)> {
    let out = hidden_command(ffmpeg)
        .args(["-v", "error", "-nostdin", "-ss", &format!("{at:.1}"), "-i", input])
        .args(["-frames:v", "1", "-vf", "scale=240:-2", "-f", "image2pipe", "-c:v", "png", "pipe:1"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let img = image::load_from_memory(&out.stdout).ok()?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    Some((img.into_raw(), w, h))
}

pub fn human_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} KB", b / 1e3)
    }
}

pub fn human_time(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A fresh empty directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(name: &str) -> Self {
            let p = std::env::temp_dir().join(format!("openrip-unit-{}-{name}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn touch(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![0u8; bytes]).unwrap();
    }

    /// Build a minimal .mpls file with the given (clip, in, out) items.
    fn mpls(items: &[(&str, u32, u32)]) -> Vec<u8> {
        let start = 20u32;
        let mut d = b"MPLS0200".to_vec();
        d.extend(start.to_be_bytes());
        d.resize(start as usize, 0);
        d.extend([0u8; 6]);
        d.extend((items.len() as u16).to_be_bytes());
        d.extend([0u8; 2]);
        for (name, t_in, t_out) in items {
            let mut item = vec![0u8; 22];
            item[0..2].copy_from_slice(&20u16.to_be_bytes());
            item[2..7].copy_from_slice(name.as_bytes());
            item[14..18].copy_from_slice(&t_in.to_be_bytes());
            item[18..22].copy_from_slice(&t_out.to_be_bytes());
            d.extend(item);
        }
        d
    }

    fn title(files: &[&str], size: u64, duration: Option<f64>) -> Title {
        Title {
            number: 0,
            files: files.iter().map(PathBuf::from).collect(),
            size,
            label: String::new(),
            duration,
            playlist: None,
            locked: None,
        }
    }

    #[test]
    fn disc_kind_labels() {
        assert_eq!(DiscKind::Dvd.label(), "DVD");
        assert_eq!(DiscKind::BluRay.label(), "Blu-ray");
    }

    #[test]
    fn stream_describe_with_language_and_channels() {
        let s = Stream { codec: "ac3".into(), language: Some("eng".into()), channels: Some(6) };
        assert_eq!(s.describe(1), "1. ENG AC3 5.1");
    }

    #[test]
    fn stream_describe_channel_variants() {
        for (ch, label) in [(Some(1), " mono"), (Some(2), " stereo"), (Some(8), " 7.1"), (Some(3), ""), (None, "")] {
            let s = Stream { codec: "dts".into(), language: None, channels: ch };
            assert_eq!(s.describe(2), format!("2. DTS{label}"));
        }
    }

    #[test]
    fn stream_describe_ignores_undefined_or_empty_language() {
        for lang in ["und", ""] {
            let s = Stream { codec: "pgssub".into(), language: Some(lang.into()), channels: None };
            assert_eq!(s.describe(3), "3. PGSSUB");
        }
    }

    #[test]
    fn title_input_single_file() {
        let t = title(&["/disc/a.m2ts"], 1, None);
        assert_eq!(t.input(), "/disc/a.m2ts");
    }

    #[test]
    fn title_input_concat_for_many_files() {
        let t = title(&["/d/VTS_01_1.VOB", "/d/VTS_01_2.VOB"], 1, None);
        assert_eq!(t.input(), "concat:/d/VTS_01_1.VOB|/d/VTS_01_2.VOB");
    }

    #[test]
    fn path_for_ffmpeg_keeps_unix_paths() {
        #[cfg(not(windows))]
        assert_eq!(path_for_ffmpeg(Path::new("/a/b c/d.vob")), "/a/b c/d.vob");
        #[cfg(windows)]
        assert_eq!(path_for_ffmpeg(Path::new(r"C:\a\b.vob")), "C:/a/b.vob");
    }

    #[test]
    fn main_title_empty_disc() {
        let d = Disc { kind: DiscKind::Dvd, name: "x".into(), titles: vec![], encrypted: false };
        assert_eq!(d.main_title(), None);
    }

    #[test]
    fn main_title_prefers_largest_size_over_duration() {
        let d = Disc {
            kind: DiscKind::BluRay,
            name: "x".into(),
            titles: vec![title(&["a"], 100, Some(36000.0)), title(&["b"], 5000, Some(60.0)), title(&["c"], 200, None)],
            encrypted: true,
        };
        assert_eq!(d.main_title(), Some(1));
    }

    #[test]
    fn main_title_breaks_size_ties_by_duration() {
        let d = Disc {
            kind: DiscKind::BluRay,
            name: "x".into(),
            titles: vec![title(&["a"], 100, Some(10.0)), title(&["b"], 100, Some(20.0)), title(&["c"], 100, None)],
            encrypted: false,
        };
        assert_eq!(d.main_title(), Some(1));
    }

    #[test]
    fn human_size_units() {
        assert_eq!(human_size(0), "0 KB");
        assert_eq!(human_size(1_500), "2 KB");
        assert_eq!(human_size(250_000_000), "250 MB");
        assert_eq!(human_size(4_700_000_000), "4.70 GB");
    }

    #[test]
    fn human_time_formats() {
        assert_eq!(human_time(0.0), "00:00:00");
        assert_eq!(human_time(59.6), "00:01:00");
        assert_eq!(human_time(3661.0), "01:01:01");
        assert_eq!(human_time(-5.0), "00:00:00");
        assert_eq!(human_time(100.0 * 3600.0), "100:00:00");
    }

    #[test]
    fn kind_of_detects_folders() {
        let t = TempDir::new("kind_of");
        assert_eq!(kind_of(&t.0), None);
        fs::create_dir(t.0.join("VIDEO_TS")).unwrap();
        assert_eq!(kind_of(&t.0), Some(DiscKind::Dvd));
        let b = TempDir::new("kind_of_bd");
        fs::create_dir(b.0.join("BDMV")).unwrap();
        assert_eq!(kind_of(&b.0), Some(DiscKind::BluRay));
    }

    #[test]
    fn kind_of_ignores_files_named_like_folders() {
        let t = TempDir::new("kind_of_file");
        touch(&t.0.join("VIDEO_TS"), 1);
        assert_eq!(kind_of(&t.0), None);
    }

    #[test]
    fn ffprobe_for_uses_sibling() {
        let t = TempDir::new("ffprobe_sibling");
        let name = if cfg!(windows) { "ffprobe.exe" } else { "ffprobe" };
        touch(&t.0.join(name), 1);
        let ffmpeg = t.0.join(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" });
        assert_eq!(ffprobe_for(&ffmpeg), Some(t.0.join(name)));
    }

    #[test]
    fn find_tool_looks_next_to_the_app_first() {
        let exe_dir = std::env::current_exe().unwrap().parent().unwrap().to_path_buf();
        let name = format!("openrip-bundled-tool-{}", std::process::id());
        let file = exe_dir.join(if cfg!(windows) { format!("{name}.exe") } else { name.clone() });
        fs::write(&file, b"x").unwrap();
        let found = find_tool(&name);
        let _ = fs::remove_file(&file);
        assert_eq!(found, Some(file));
    }

    #[test]
    fn find_tool_missing_returns_none() {
        assert_eq!(find_tool("definitely-not-a-real-tool-openrip"), None);
    }

    #[test]
    fn parse_mpls_rejects_bad_data() {
        assert!(parse_mpls(b"").is_none());
        assert!(parse_mpls(b"NOPE0200").is_none());
        // Right magic but truncated.
        assert!(parse_mpls(b"MPLS0200\0\0\0\x14").is_none());
    }

    #[test]
    fn parse_mpls_reads_clips_and_duration() {
        let data = mpls(&[("00001", 0, 45_000 * 60), ("00002", 45_000, 45_000 * 31)]);
        let (clips, secs) = parse_mpls(&data).expect("parsed");
        assert_eq!(clips, vec!["00001".to_string(), "00002".to_string()]);
        assert!((secs - 90.0).abs() < 1e-9, "{secs}");
    }

    #[test]
    fn parse_mpls_truncated_items_fail() {
        let mut data = mpls(&[("00001", 0, 45_000)]);
        data.truncate(data.len() - 5);
        assert!(parse_mpls(&data).is_none());
    }

    #[test]
    fn parse_mpls_out_before_in_saturates() {
        let (_, secs) = parse_mpls(&mpls(&[("00001", 1000, 10)])).unwrap();
        assert_eq!(secs, 0.0);
    }

    #[test]
    fn dvd_titles_groups_sorts_and_skips_menu() {
        let t = TempDir::new("dvd_titles");
        let v = t.0.join("VIDEO_TS");
        touch(&v.join("VIDEO_TS.VOB"), 5);
        touch(&v.join("VTS_01_0.VOB"), 5); // menu
        touch(&v.join("VTS_01_2.VOB"), 20);
        touch(&v.join("VTS_01_1.VOB"), 10);
        touch(&v.join("vts_02_1.vob"), 7); // lowercase still counts
        touch(&v.join("VTS_01_0.IFO"), 3);
        touch(&v.join("VTS_XX_1.VOB"), 3);
        let titles = dvd_titles(&v).unwrap();
        assert_eq!(titles.len(), 2);
        assert_eq!(titles[0].number, 1);
        assert_eq!(titles[0].size, 30);
        let names: Vec<String> = titles[0].files.iter().map(|f| f.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec!["VTS_01_1.VOB", "VTS_01_2.VOB"]);
        assert_eq!(titles[1].number, 2);
        assert_eq!(titles[1].size, 7);
        assert!(titles[0].playlist.is_none() && titles[0].locked.is_none() && titles[0].duration.is_none());
    }

    #[test]
    fn dvd_titles_errors() {
        let t = TempDir::new("dvd_empty");
        let v = t.0.join("VIDEO_TS");
        fs::create_dir(&v).unwrap();
        touch(&v.join("VTS_01_0.VOB"), 1);
        assert_eq!(dvd_titles(&v).unwrap_err(), "This DVD has no video titles.");
        assert!(dvd_titles(&t.0.join("missing")).unwrap_err().starts_with("Couldn't read the disc"));
    }

    #[test]
    fn bluray_titles_from_playlists_sorted_and_deduped() {
        let t = TempDir::new("bd_playlists");
        let bdmv = t.0.join("BDMV");
        touch(&bdmv.join("STREAM/00001.m2ts"), 100);
        touch(&bdmv.join("STREAM/00002.M2TS"), 300);
        touch(&bdmv.join("STREAM/00003.m2ts"), 50);
        let pl = bdmv.join("PLAYLIST");
        fs::create_dir_all(&pl).unwrap();
        fs::write(pl.join("00000.mpls"), mpls(&[("00001", 0, 45_000 * 10)])).unwrap();
        fs::write(pl.join("00001.mpls"), mpls(&[("00001", 0, 45_000 * 10)])).unwrap(); // duplicate
        fs::write(pl.join("00800.mpls"), mpls(&[("00002", 0, 45_000 * 100), ("00003", 0, 45_000 * 20)])).unwrap();
        fs::write(pl.join("00900.mpls"), mpls(&[("99999", 0, 45_000)])).unwrap(); // clip missing
        fs::write(pl.join("junk.mpls"), b"garbage").unwrap();
        fs::write(pl.join("notes.txt"), b"x").unwrap();

        let titles = bluray_titles(&bdmv).unwrap();
        assert_eq!(titles.len(), 2);
        assert_eq!(titles[0].number, 1);
        assert_eq!(titles[0].label, "playlist 00800");
        assert_eq!(titles[0].playlist, Some(800));
        assert_eq!(titles[0].size, 350);
        assert_eq!(titles[0].duration, Some(120.0));
        assert_eq!(titles[0].files.len(), 2);
        assert_eq!(titles[1].number, 2);
        assert_eq!(titles[1].label, "playlist 00000");
        assert_eq!(titles[1].size, 100);
    }

    #[test]
    fn bluray_titles_counts_repeated_clip_once_in_size() {
        let t = TempDir::new("bd_repeat");
        let bdmv = t.0.join("BDMV");
        touch(&bdmv.join("STREAM/00001.m2ts"), 100);
        fs::create_dir_all(bdmv.join("PLAYLIST")).unwrap();
        fs::write(bdmv.join("PLAYLIST/00001.mpls"), mpls(&[("00001", 0, 45_000), ("00001", 0, 45_000)])).unwrap();
        let titles = bluray_titles(&bdmv).unwrap();
        assert_eq!(titles[0].files.len(), 2);
        assert_eq!(titles[0].size, 100);
    }

    #[test]
    fn bluray_titles_falls_back_to_clips() {
        let t = TempDir::new("bd_clips");
        let bdmv = t.0.join("BDMV");
        touch(&bdmv.join("STREAM/00002.m2ts"), 20);
        touch(&bdmv.join("STREAM/00001.m2ts"), 10);
        touch(&bdmv.join("STREAM/readme.txt"), 1);
        let titles = bluray_titles(&bdmv).unwrap();
        assert_eq!(titles.len(), 2);
        assert_eq!(titles[0].label, "clip 00001");
        assert_eq!(titles[0].size, 10);
        assert_eq!(titles[1].label, "clip 00002");
        assert!(titles.iter().all(|t| t.playlist.is_none() && t.duration.is_none()));
    }

    #[test]
    fn bluray_titles_errors() {
        let t = TempDir::new("bd_empty");
        let bdmv = t.0.join("BDMV");
        fs::create_dir_all(bdmv.join("STREAM")).unwrap();
        assert_eq!(bluray_titles(&bdmv).unwrap_err(), "This Blu-ray has no video clips.");
        assert!(bluray_titles(&t.0.join("nowhere")).is_err());
    }

    #[test]
    fn open_dvd_root_and_video_ts_folder() {
        let t = TempDir::new("open_dvd");
        let root = t.0.join("MY_MOVIE");
        touch(&root.join("VIDEO_TS/VTS_01_1.VOB"), 10);
        for path in [root.clone(), root.join("VIDEO_TS")] {
            let d = open(&path).unwrap();
            assert_eq!(d.kind, DiscKind::Dvd);
            assert_eq!(d.name, "MY_MOVIE");
            assert_eq!(d.titles.len(), 1);
            assert!(!d.encrypted);
        }
    }

    #[test]
    fn open_bluray_accepts_stream_folder_and_flags_encryption() {
        let t = TempDir::new("open_bd");
        let root = t.0.join("BD_DISC");
        touch(&root.join("BDMV/STREAM/00001.m2ts"), 10);
        fs::create_dir_all(root.join("BDMV/PLAYLIST")).unwrap();
        fs::write(root.join("BDMV/PLAYLIST/00001.mpls"), mpls(&[("00001", 0, 45_000)])).unwrap();

        let d = open(&root.join("BDMV/STREAM")).unwrap();
        assert_eq!(d.kind, DiscKind::BluRay);
        assert_eq!(d.name, "BD_DISC");
        assert!(!d.encrypted);
        assert!(d.titles[0].locked.is_none());

        fs::create_dir(root.join("AACS")).unwrap();
        let d = open(&root).unwrap();
        assert!(d.encrypted);
        assert_eq!(d.titles[0].locked.as_deref(), Some(root.as_path()));
    }

    #[test]
    fn open_encrypted_bluray_only_locks_playlist_titles() {
        let t = TempDir::new("open_bd_clips");
        let root = t.0.join("BD");
        touch(&root.join("BDMV/STREAM/00001.m2ts"), 10);
        fs::create_dir(root.join("AACS")).unwrap();
        let d = open(&root).unwrap();
        assert!(d.encrypted);
        assert!(d.titles[0].locked.is_none());
    }

    #[test]
    fn open_without_disc_folders_errors() {
        let t = TempDir::new("open_none");
        let err = open(&t.0).unwrap_err();
        assert!(err.contains("No DVD"), "{err}");
    }

    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
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

    #[cfg(unix)]
    #[test]
    fn probe_parses_ffprobe_output() {
        let t = TempDir::new("probe");
        let ffmpeg = script(&t.0, "ffmpeg", "exit 1"); // thumbnail fails -> None
        script(
            &t.0,
            "ffprobe",
            r#"cat <<'OUT'
stream|codec_name=mpeg2video|codec_type=video|width=720|height=480
stream|codec_name=ac3|codec_type=audio|channels=6|tag:language=eng
stream|codec_name=dts|codec_type=audio|channels=2
stream|codec_name=dvd_subtitle|codec_type=subtitle|tag:language=fra
stream|codec_name=mpeg2video|codec_type=video|width=1|height=1
format|duration=123.5
OUT"#,
        );
        let info = probe(&ffmpeg, &title(&["/x/a.vob"], 1, None));
        assert_eq!((info.width, info.height), (Some(720), Some(480)));
        assert_eq!(info.duration, Some(123.5));
        assert_eq!(info.audio.len(), 2);
        assert_eq!(info.audio[0].codec, "ac3");
        assert_eq!(info.audio[0].channels, Some(6));
        assert_eq!(info.audio[0].language.as_deref(), Some("eng"));
        assert_eq!(info.subtitles.len(), 1);
        assert_eq!(info.subtitles[0].codec, "dvd");
        assert!(info.thumb.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn probe_prefers_playlist_duration() {
        let t = TempDir::new("probe_pl");
        let ffmpeg = script(&t.0, "ffmpeg", "exit 1");
        script(&t.0, "ffprobe", "echo 'format|duration=5.0'");
        let info = probe(&ffmpeg, &title(&["/x/a.m2ts"], 1, Some(99.0)));
        assert_eq!(info.duration, Some(99.0));
    }

    #[cfg(unix)]
    #[test]
    fn probe_sums_parts_of_joined_files() {
        let t = TempDir::new("probe_sum");
        let ffmpeg = script(&t.0, "ffmpeg", "exit 1");
        // Whole-input probe reports a wrong length; per-file probes report CSV durations.
        script(&t.0, "ffprobe", r#"case "$*" in *concat:*) echo 'format|duration=1.0';; *) echo 10.5;; esac"#);
        let info = probe(&ffmpeg, &title(&["/x/1.vob", "/x/2.vob"], 1, None));
        assert_eq!(info.duration, Some(21.0));
    }

    #[cfg(unix)]
    #[test]
    fn probe_without_tools_gives_empty_info() {
        let t = TempDir::new("probe_none");
        let info = probe(&t.0.join("no-ffmpeg"), &title(&["/x/a.vob"], 1, None));
        assert!(info.duration.is_none() && info.thumb.is_none() && info.audio.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn thumbnail_decodes_png_from_ffmpeg() {
        let t = TempDir::new("thumb");
        // Write a real 2x1 PNG via the image crate, then have a fake ffmpeg cat it.
        let png_path = t.0.join("frame.png");
        image::RgbaImage::from_pixel(2, 1, image::Rgba([1, 2, 3, 255])).save(&png_path).unwrap();
        let ffmpeg = script(&t.0, "ffmpeg", &format!("cat '{}'", png_path.display()));
        let (rgba, w, h) = thumbnail(&ffmpeg, "ignored", 1.0).expect("thumb");
        assert_eq!((w, h), (2, 1));
        assert_eq!(rgba.len(), 8);
        assert_eq!(&rgba[..4], &[1, 2, 3, 255]);
    }

    #[cfg(unix)]
    #[test]
    fn thumbnail_none_on_garbage() {
        let t = TempDir::new("thumb_bad");
        let ffmpeg = script(&t.0, "ffmpeg", "echo not-a-png");
        assert!(thumbnail(&ffmpeg, "x", 0.0).is_none());
    }
}
