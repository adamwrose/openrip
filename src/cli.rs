//! Command-line mode: list a disc's titles and rip them without opening the window.
//!
//! `openrip list <disc>`, `openrip rip <disc> ...`, `openrip profiles`. Anything else
//! (including no arguments, or the window-only flags like `--open`) starts the app as usual.
//! Results (paths, tables) go to stdout and progress goes to stderr, so output can be piped.

use crate::disc::{self, Disc, DiscKind, TitleInfo};
use crate::rip::{self, Group, Profile, Quality, Resolution, PROFILES};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::sync::mpsc::channel;

const EXIT_OK: i32 = 0;
const EXIT_FAILED: i32 = 1;
const EXIT_USAGE: i32 = 2;

#[derive(Debug, PartialEq)]
pub enum Command {
    Help,
    Version,
    Profiles,
    List(ListArgs),
    Rip(RipArgs),
}

#[derive(Debug, PartialEq)]
pub struct ListArgs {
    pub path: PathBuf,
    /// Probe every title with FFprobe for picture size, audio and subtitle tracks.
    pub details: bool,
    pub ffmpeg: Option<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub struct RipArgs {
    pub path: PathBuf,
    /// Title numbers as shown by `list`.
    pub titles: Vec<usize>,
    pub main: bool,
    pub all: bool,
    pub profile: Profile,
    pub out: Option<PathBuf>,
    pub resolution: Resolution,
    pub quality: Quality,
    /// 1-based audio track, for every ripped title.
    pub audio: Option<usize>,
    pub subtitles: bool,
    pub deinterlace: bool,
    pub hardware: bool,
    pub merge: bool,
    pub ffmpeg: Option<PathBuf>,
}

const HELP: &str = "\
OpenRip: rip DVDs and Blu-rays. Run with no arguments to open the window.

USAGE:
    openrip list <disc> [--details] [--ffmpeg PATH]
    openrip rip <disc> [options]
    openrip profiles
    openrip --help | --version

<disc> is a mounted disc, a VIDEO_TS or BDMV folder (or its parent), or an .iso file.

LIST:
    --details           Probe each title for picture size, audio and subtitle tracks (slower)

RIP OPTIONS:
    --main              Rip the main movie (the default when no title is given)
    --title N[,N|N-M]   Rip these titles (numbers from `list`); may be repeated
    --all               Rip every title
    --profile NAME      Output format, see `openrip profiles` (default: MP4)
    --out DIR           Output folder (default: the app's Videos/OpenRip folder)
    --resolution R      source (default), 1080p, 720p or 480p
    --quality Q         high (default), standard or small
    --audio N           Audio track number to use (1 is the first)
    --subtitles         Keep subtitle tracks (formats that support them, such as MKV)
    --no-deinterlace    Don't remove interlacing lines
    --no-hardware       Use software encoding even if a graphics card encoder is found
    --merge             Join several ripped titles into one extra file
    --ffmpeg PATH       Use this ffmpeg instead of the one found automatically

Saved file paths are printed to stdout; progress goes to stderr.
Exit status: 0 success, 1 a title or the disc failed, 2 bad command line.
";

/// Whether `args` (without the program name) ask for command-line mode.
fn is_cli(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("list" | "rip" | "profiles" | "help" | "version" | "--help" | "-h" | "--version" | "-V")
    )
}

/// Turn the arguments into a command; `None` means "not command-line mode, open the window".
pub fn parse(args: &[String]) -> Option<Result<Command, String>> {
    if !is_cli(args) {
        return None;
    }
    let rest = &args[1..];
    Some(match args[0].as_str() {
        "help" | "--help" | "-h" => Ok(Command::Help),
        "version" | "--version" | "-V" => Ok(Command::Version),
        "profiles" => Ok(Command::Profiles),
        "list" => parse_list(rest),
        _ => parse_rip(rest),
    })
}

/// Walks the arguments after the subcommand, handing out flags and their values.
struct Cursor<'a> {
    args: &'a [String],
    i: usize,
}

impl<'a> Cursor<'a> {
    /// The next argument split into (flag, inline value) so `--out=dir` and `--out dir` both work.
    fn next(&mut self) -> Option<(&'a str, Option<&'a str>)> {
        let a = self.args.get(self.i)?;
        self.i += 1;
        match a.split_once('=') {
            Some((flag, v)) if flag.starts_with("--") => Some((flag, Some(v))),
            _ => Some((a.as_str(), None)),
        }
    }

    fn value(&mut self, flag: &str, inline: Option<&'a str>) -> Result<&'a str, String> {
        if let Some(v) = inline {
            return Ok(v);
        }
        let v = self.args.get(self.i).ok_or_else(|| format!("{flag} needs a value"))?;
        self.i += 1;
        Ok(v)
    }
}

fn no_value(flag: &str, inline: Option<&str>) -> Result<(), String> {
    match inline {
        Some(_) => Err(format!("{flag} doesn't take a value")),
        None => Ok(()),
    }
}

fn parse_list(args: &[String]) -> Result<Command, String> {
    let (mut path, mut details, mut ffmpeg) = (None, false, None);
    let mut c = Cursor { args, i: 0 };
    while let Some((flag, inline)) = c.next() {
        match flag {
            "--details" => {
                no_value(flag, inline)?;
                details = true;
            }
            "--ffmpeg" => ffmpeg = Some(PathBuf::from(c.value(flag, inline)?)),
            f if f.starts_with('-') => return Err(format!("unknown option {f} for `list`")),
            p => set_path(&mut path, p)?,
        }
    }
    Ok(Command::List(ListArgs { path: path.ok_or("`list` needs a disc, folder or .iso file")?, details, ffmpeg }))
}

fn set_path(slot: &mut Option<PathBuf>, p: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("unexpected extra argument: {p}"));
    }
    *slot = Some(PathBuf::from(p));
    Ok(())
}

fn parse_rip(args: &[String]) -> Result<Command, String> {
    let mut path = None;
    let mut a = RipArgs {
        path: PathBuf::new(),
        titles: Vec::new(),
        main: false,
        all: false,
        profile: PROFILES[0],
        out: None,
        resolution: Resolution::Source,
        quality: Quality::High,
        audio: None,
        subtitles: false,
        deinterlace: true,
        hardware: true,
        merge: false,
        ffmpeg: None,
    };
    let mut c = Cursor { args, i: 0 };
    while let Some((flag, inline)) = c.next() {
        match flag {
            "--main" => {
                no_value(flag, inline)?;
                a.main = true;
            }
            "--all" => {
                no_value(flag, inline)?;
                a.all = true;
            }
            "--subtitles" => {
                no_value(flag, inline)?;
                a.subtitles = true;
            }
            "--no-deinterlace" => {
                no_value(flag, inline)?;
                a.deinterlace = false;
            }
            "--no-hardware" => {
                no_value(flag, inline)?;
                a.hardware = false;
            }
            "--merge" => {
                no_value(flag, inline)?;
                a.merge = true;
            }
            "--title" | "--titles" => a.titles.extend(parse_titles(c.value(flag, inline)?)?),
            "--profile" => a.profile = find_profile(c.value(flag, inline)?)?,
            "--out" => a.out = Some(PathBuf::from(c.value(flag, inline)?)),
            "--ffmpeg" => a.ffmpeg = Some(PathBuf::from(c.value(flag, inline)?)),
            "--resolution" => a.resolution = parse_resolution(c.value(flag, inline)?)?,
            "--quality" => a.quality = parse_quality(c.value(flag, inline)?)?,
            "--audio" => {
                let v = c.value(flag, inline)?;
                a.audio = Some(v.parse::<usize>().ok().filter(|n| *n >= 1).ok_or_else(|| format!("--audio expects a track number from 1, not '{v}'"))?);
            }
            f if f.starts_with('-') => return Err(format!("unknown option {f} for `rip` (see --help)")),
            p => set_path(&mut path, p)?,
        }
    }
    a.path = path.ok_or("`rip` needs a disc, folder or .iso file")?;
    Ok(Command::Rip(a))
}

/// "3", "1,3,5" or "2-4" (or a mix) into title numbers.
fn parse_titles(text: &str) -> Result<Vec<usize>, String> {
    let bad = || format!("'{text}' isn't a title list; use numbers like 1, 1,3 or 2-4");
    let mut out = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        let (lo, hi) = match part.split_once('-') {
            Some((a, b)) => (a.trim().parse::<usize>().map_err(|_| bad())?, b.trim().parse::<usize>().map_err(|_| bad())?),
            None => {
                let n = part.parse::<usize>().map_err(|_| bad())?;
                (n, n)
            }
        };
        if lo == 0 || hi < lo {
            return Err(bad());
        }
        out.extend(lo..=hi);
    }
    Ok(out)
}

fn normalise(name: &str) -> String {
    name.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Find a profile by name, ignoring case, spaces and dashes ("mkv-hevc" is "MKV HEVC").
fn find_profile(name: &str) -> Result<Profile, String> {
    let want = normalise(name);
    PROFILES
        .iter()
        .find(|p| normalise(p.name) == want)
        .copied()
        .ok_or_else(|| format!("unknown profile '{name}'; run `openrip profiles` to see the choices"))
}

fn parse_resolution(v: &str) -> Result<Resolution, String> {
    match normalise(v).as_str() {
        "source" | "same" | "original" => Ok(Resolution::Source),
        "1080p" | "1080" => Ok(Resolution::P1080),
        "720p" | "720" => Ok(Resolution::P720),
        "480p" | "480" => Ok(Resolution::P480),
        _ => Err(format!("unknown resolution '{v}'; use source, 1080p, 720p or 480p")),
    }
}

fn parse_quality(v: &str) -> Result<Quality, String> {
    match normalise(v).as_str() {
        "high" => Ok(Quality::High),
        "standard" => Ok(Quality::Standard),
        "small" => Ok(Quality::Small),
        _ => Err(format!("unknown quality '{v}'; use high, standard or small")),
    }
}

/// Which titles (as indexes into `disc.titles`) the options select. No selection means the main movie.
fn select_titles(disc: &Disc, numbers: &[usize], main: bool, all: bool) -> Result<Vec<usize>, String> {
    let mut picked: Vec<usize> = Vec::new();
    if all {
        picked.extend(0..disc.titles.len());
    }
    for n in numbers {
        match disc.titles.iter().position(|t| t.number == *n) {
            Some(i) => picked.push(i),
            None => {
                let last = disc.titles.iter().map(|t| t.number).max().unwrap_or(0);
                return Err(format!("the disc has no title {n} (titles are numbered 1 to {last})"));
            }
        }
    }
    if main || (!all && numbers.is_empty()) {
        picked.push(disc.main_title().ok_or("the disc has no titles")?);
    }
    picked.sort_unstable();
    picked.dedup();
    Ok(picked)
}

// ---------------------------------------------------------------- output

fn format_profiles() -> String {
    let width = PROFILES.iter().map(|p| p.name.len()).max().unwrap_or(0);
    let mut out = String::new();
    for (group, title) in [(Group::Video, "Video"), (Group::Audio, "Audio only"), (Group::Device, "Devices")] {
        out.push_str(&format!("{title}:\n"));
        for p in PROFILES.iter().filter(|p| p.group == group) {
            out.push_str(&format!("  {:<width$}  .{:<4} {}\n", p.name, p.ext, p.note));
        }
    }
    out.push_str("\nNames are not case-sensitive, and spaces or dashes are optional: --profile mkv-hevc\n");
    out
}

/// The table `list` prints. `infos` lines up with `disc.titles` (empty without --details).
fn format_list(disc: &Disc, infos: &[Option<TitleInfo>]) -> String {
    let main = disc.main_title();
    let mut out = format!(
        "{} \"{}\": {} title{}{}\n",
        disc.kind.label(),
        disc.name,
        disc.titles.len(),
        if disc.titles.len() == 1 { "" } else { "s" },
        if disc.encrypted { " (copy-protected)" } else { "" },
    );
    out.push_str("   No.  Size       Length    Source\n");
    for (i, t) in disc.titles.iter().enumerate() {
        let info = infos.get(i).and_then(|i| i.as_ref());
        let length = t.duration.or_else(|| info.and_then(|f| f.duration)).map(disc::human_time).unwrap_or_else(|| "-".into());
        let source = if t.label.is_empty() { format!("{} file{}", t.files.len(), if t.files.len() == 1 { "" } else { "s" }) } else { t.label.clone() };
        out.push_str(&format!(
            " {} {:>4}  {:<9}  {:<8}  {}\n",
            if main == Some(i) { "*" } else { " " },
            t.number,
            disc::human_size(t.size),
            length,
            source
        ));
        if let Some(f) = info {
            if let (Some(w), Some(h)) = (f.width, f.height) {
                out.push_str(&format!("          {w}x{h}\n"));
            }
            let tracks = |label: &str, list: &[disc::Stream]| {
                if list.is_empty() {
                    String::new()
                } else {
                    format!("          {label}: {}\n", list.iter().enumerate().map(|(n, s)| s.describe(n + 1)).collect::<Vec<_>>().join("; "))
                }
            };
            out.push_str(&tracks("Audio", &f.audio));
            out.push_str(&tracks("Subtitles", &f.subtitles));
        }
    }
    out.push_str("\n* = main title\n");
    out
}

/// One line of progress, e.g. "[2/3] Title 5  42%  1.8x".
fn progress_line(position: usize, total: usize, title: usize, fraction: f32, speed: f32) -> String {
    let speed = if speed > 0.0 { format!("  {speed:.1}x") } else { String::new() };
    format!("[{position}/{total}] Title {title}  {:>3.0}%{speed}", (fraction * 100.0).clamp(0.0, 100.0))
}

// ---------------------------------------------------------------- running

/// Run command-line mode if `args` (without the program name) ask for it.
/// Returns the process exit status, or `None` to open the window instead.
pub fn run(args: &[String]) -> Option<i32> {
    let parsed = parse(args)?;
    attach_console();
    Some(match parsed {
        Err(e) => {
            eprintln!("openrip: {e}\nTry `openrip --help`.");
            EXIT_USAGE
        }
        Ok(Command::Help) => {
            print!("{HELP}");
            EXIT_OK
        }
        Ok(Command::Version) => {
            println!("OpenRip {}", env!("CARGO_PKG_VERSION"));
            EXIT_OK
        }
        Ok(Command::Profiles) => {
            print!("{}", format_profiles());
            EXIT_OK
        }
        Ok(Command::List(a)) => report(run_list(a)),
        Ok(Command::Rip(a)) => report(run_rip(a)),
    })
}

fn report(result: Result<i32, String>) -> i32 {
    result.unwrap_or_else(|e| {
        eprintln!("openrip: {e}");
        EXIT_FAILED
    })
}

fn ffmpeg_path(given: &Option<PathBuf>) -> Result<PathBuf, String> {
    match given {
        Some(p) if p.is_file() => Ok(p.clone()),
        Some(p) => Err(format!("{} isn't a file", p.display())),
        None => disc::find_tool("ffmpeg").ok_or_else(|| "FFmpeg wasn't found. Install it, or pass --ffmpeg PATH.".to_string()),
    }
}

fn run_list(a: ListArgs) -> Result<i32, String> {
    crate::bluray::init();
    let disc = disc::open(&a.path)?;
    let infos = if a.details {
        let ffmpeg = ffmpeg_path(&a.ffmpeg)?;
        disc.titles
            .iter()
            .map(|t| {
                eprintln!("Reading title {} of {}...", t.number, disc.titles.len());
                Some(disc::probe(&ffmpeg, t))
            })
            .collect()
    } else {
        Vec::new()
    };
    print!("{}", format_list(&disc, &infos));
    if disc.encrypted {
        let gaps = crate::bluray::missing();
        if !gaps.is_empty() {
            eprintln!("Note: this disc is copy-protected. To rip it you still need: {}.", gaps.join(", "));
        }
    }
    Ok(EXIT_OK)
}

fn run_rip(a: RipArgs) -> Result<i32, String> {
    crate::bluray::init();
    let ffmpeg = ffmpeg_path(&a.ffmpeg)?;
    let disc = disc::open(&a.path)?;
    let picked = select_titles(&disc, &a.titles, a.main, a.all)?;
    let dir = a.out.clone().unwrap_or_else(crate::default_output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't use the output folder {}: {e}", dir.display()))?;
    if a.subtitles && !a.profile.keeps_subs {
        eprintln!("Note: {} can't hold subtitle tracks; --subtitles is ignored (try MKV).", a.profile.name);
    }

    let mut jobs = Vec::new();
    let mut numbers = Vec::new();
    for &i in &picked {
        let t = &disc.titles[i];
        eprintln!("Reading title {}...", t.number);
        let info = disc::probe(&ffmpeg, t);
        let audio_track = match a.audio {
            Some(n) if n > info.audio.len() => {
                return Err(format!("title {} has {} audio track{}, so --audio {n} isn't available", t.number, info.audio.len(), if info.audio.len() == 1 { "" } else { "s" }))
            }
            Some(n) => Some(n - 1),
            None => (!info.audio.is_empty()).then_some(0),
        };
        jobs.push(rip::Job {
            slot: jobs.len(),
            input: t.input(),
            duration: info.duration,
            audio_track,
            subtitles: a.subtitles,
            output: rip::free_path(&dir, &format!("{} - Title {}", disc.name, t.number), a.profile.ext),
            profile: a.profile,
            locked: t.locked.clone().zip(t.playlist),
        });
        numbers.push(t.number);
    }
    let total = jobs.len();
    if disc.kind == DiscKind::BluRay && disc.encrypted {
        let gaps = crate::bluray::missing();
        if !gaps.is_empty() {
            return Err(format!("this disc is copy-protected. Missing: {}.", gaps.join(", ")));
        }
    }
    let merge_to = (a.merge && total > 1).then(|| rip::free_path(&dir, &format!("{} - All titles", disc.name), a.profile.ext));
    let settings = rip::Settings { resolution: a.resolution, quality: a.quality, hardware: a.hardware, deinterlace: a.deinterlace };

    let (tx, rx) = channel();
    rip::start(ffmpeg, jobs, settings, merge_to, rip::Control::default(), tx, || {});
    let live = std::io::stderr().is_terminal();
    let mut failures = 0;
    let mut last_step = -1i32;
    for msg in rx.iter() {
        match msg {
            rip::Msg::Started(slot) => eprintln!("Ripping title {} ({}/{total}) as {}...", numbers[slot], slot + 1, a.profile.name),
            rip::Msg::Progress { slot, fraction, speed } => {
                let line = progress_line(slot + 1, total, numbers[slot], fraction, speed);
                if live {
                    eprint!("\r{line}   ");
                    let _ = std::io::stderr().flush();
                } else if (fraction * 10.0) as i32 != last_step {
                    // Without a terminal, print a line per 10% instead of redrawing one.
                    last_step = (fraction * 10.0) as i32;
                    eprintln!("{line}");
                }
            }
            rip::Msg::Finished { slot, result } => {
                if live {
                    eprint!("\r{:60}\r", "");
                }
                last_step = -1;
                match result {
                    Ok(path) => println!("{}", path.display()),
                    Err(e) => {
                        failures += 1;
                        eprintln!("Title {} failed: {e}", numbers[slot]);
                    }
                }
            }
            rip::Msg::Merged(Ok(path)) => println!("{}", path.display()),
            rip::Msg::Merged(Err(e)) => {
                failures += 1;
                eprintln!("Joining the titles failed: {e}");
            }
            rip::Msg::AllDone => break,
        }
    }
    Ok(if failures == 0 { EXIT_OK } else { EXIT_FAILED })
}

/// On Windows the app has no console of its own; use the one it was started from.
#[cfg(windows)]
fn attach_console() {
    use std::os::windows::io::AsRawHandle;
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn GetStdHandle(which: u32) -> *mut core::ffi::c_void;
        fn SetStdHandle(which: u32, handle: *mut core::ffi::c_void) -> i32;
    }
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    // SAFETY: plain Win32 calls with valid arguments; the console file handle is kept open on purpose.
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let h = GetStdHandle(which);
            // Only fill in handles the parent didn't already redirect to a file or pipe.
            if h.is_null() || h as isize == -1 {
                if let Ok(console) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
                    SetStdHandle(which, console.as_raw_handle() as *mut _);
                    std::mem::forget(console);
                }
            }
        }
    }
}

#[cfg(not(windows))]
fn attach_console() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disc::{Stream, Title};
    use std::fs;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    fn parsed(s: &str) -> Command {
        parse(&args(s)).expect("cli mode").expect("valid")
    }

    fn error(s: &str) -> String {
        parse(&args(s)).expect("cli mode").expect_err("should be an error")
    }

    fn title(number: usize, size: u64, duration: Option<f64>, label: &str) -> Title {
        Title { number, files: vec![PathBuf::from("/x/a.vob")], size, label: label.into(), duration, playlist: None, locked: None }
    }

    fn disc() -> Disc {
        Disc {
            kind: DiscKind::BluRay,
            name: "My Disc".into(),
            titles: vec![title(1, 20_000_000_000, Some(7200.0), "playlist 00800"), title(2, 500_000_000, Some(60.0), "playlist 00801"), title(3, 100, None, "")],
            encrypted: false,
        }
    }

    #[test]
    fn window_mode_is_left_alone() {
        assert!(parse(&[]).is_none());
        assert!(parse(&args("--open /disc")).is_none());
        assert!(parse(&args("--tab settings")).is_none());
        assert!(parse(&args("/some/path")).is_none());
        assert!(parse(&args("--screenshot a.png")).is_none());
    }

    #[test]
    fn simple_commands() {
        for a in ["help", "--help", "-h"] {
            assert_eq!(parsed(a), Command::Help);
        }
        for a in ["version", "--version", "-V"] {
            assert_eq!(parsed(a), Command::Version);
        }
        assert_eq!(parsed("profiles"), Command::Profiles);
    }

    #[test]
    fn list_parsing() {
        assert_eq!(parsed("list /disc"), Command::List(ListArgs { path: "/disc".into(), details: false, ffmpeg: None }));
        assert_eq!(
            parsed("list --details --ffmpeg=/bin/ff /disc"),
            Command::List(ListArgs { path: "/disc".into(), details: true, ffmpeg: Some("/bin/ff".into()) })
        );
        assert!(error("list").contains("needs a disc"));
        assert!(error("list a b").contains("extra argument"));
        assert!(error("list --nope a").contains("unknown option"));
        assert!(error("list --details=1 a").contains("doesn't take a value"));
    }

    #[test]
    fn rip_defaults() {
        let Command::Rip(a) = parsed("rip /disc") else { panic!() };
        assert_eq!(a.path, PathBuf::from("/disc"));
        assert_eq!(a.profile.name, "MP4");
        assert!(a.titles.is_empty() && !a.main && !a.all);
        assert_eq!((a.resolution, a.quality), (Resolution::Source, Quality::High));
        assert!(a.deinterlace && a.hardware && !a.subtitles && !a.merge);
        assert!(a.out.is_none() && a.audio.is_none() && a.ffmpeg.is_none());
    }

    #[test]
    fn rip_all_options() {
        let Command::Rip(a) = parsed(
            "rip --title 1,3 --title=5-6 --profile mkv-hevc --out /o --resolution 720p --quality=small --audio 2 --subtitles --no-deinterlace --no-hardware --merge --ffmpeg /f /disc",
        ) else {
            panic!()
        };
        assert_eq!(a.titles, vec![1, 3, 5, 6]);
        assert_eq!(a.profile.name, "MKV HEVC");
        assert_eq!(a.out, Some(PathBuf::from("/o")));
        assert_eq!((a.resolution, a.quality, a.audio), (Resolution::P720, Quality::Small, Some(2)));
        assert!(a.subtitles && !a.deinterlace && !a.hardware && a.merge);
        assert_eq!(a.ffmpeg, Some(PathBuf::from("/f")));
        assert_eq!(a.path, PathBuf::from("/disc"));
    }

    #[test]
    fn rip_errors() {
        assert!(error("rip").contains("needs a disc"));
        assert!(error("rip /d --title").contains("--title needs a value"));
        assert!(error("rip /d --profile nope").contains("unknown profile"));
        assert!(error("rip /d --resolution 9000p").contains("unknown resolution"));
        assert!(error("rip /d --quality best").contains("unknown quality"));
        assert!(error("rip /d --audio 0").contains("--audio expects"));
        assert!(error("rip /d --audio x").contains("--audio expects"));
        assert!(error("rip /d --bogus").contains("unknown option"));
        assert!(error("rip /d /e").contains("extra argument"));
    }

    #[test]
    fn title_lists() {
        assert_eq!(parse_titles("3").unwrap(), vec![3]);
        assert_eq!(parse_titles("1, 3 ,5").unwrap(), vec![1, 3, 5]);
        assert_eq!(parse_titles("2-4").unwrap(), vec![2, 3, 4]);
        assert_eq!(parse_titles("1,4-5,9").unwrap(), vec![1, 4, 5, 9]);
        for bad in ["", "0", "a", "4-2", "1,,2", "-3", "1-"] {
            assert!(parse_titles(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn profile_names_are_forgiving() {
        assert_eq!(find_profile("MP4").unwrap().name, "MP4");
        assert_eq!(find_profile("mkv lossless").unwrap().name, "MKV Lossless");
        assert_eq!(find_profile("MKV-Lossless").unwrap().name, "MKV Lossless");
        assert_eq!(find_profile("appletv").unwrap().name, "Apple TV");
        assert!(find_profile("").is_err());
        for p in PROFILES {
            assert_eq!(find_profile(p.name).unwrap().name, p.name);
        }
    }

    #[test]
    fn resolution_and_quality_names() {
        assert_eq!(parse_resolution("Source").unwrap(), Resolution::Source);
        assert_eq!(parse_resolution("1080").unwrap(), Resolution::P1080);
        assert_eq!(parse_resolution("480P").unwrap(), Resolution::P480);
        assert_eq!(parse_quality("Standard").unwrap(), Quality::Standard);
    }

    #[test]
    fn selection_defaults_to_main_title() {
        assert_eq!(select_titles(&disc(), &[], false, false).unwrap(), vec![0]);
        assert_eq!(select_titles(&disc(), &[], true, false).unwrap(), vec![0]);
    }

    #[test]
    fn selection_by_number_all_and_combined() {
        let d = disc();
        assert_eq!(select_titles(&d, &[3, 2], false, false).unwrap(), vec![1, 2], "sorted, no main added");
        assert_eq!(select_titles(&d, &[2], true, false).unwrap(), vec![0, 1]);
        assert_eq!(select_titles(&d, &[1], true, false).unwrap(), vec![0], "no duplicates");
        assert_eq!(select_titles(&d, &[], false, true).unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn selection_errors() {
        let e = select_titles(&disc(), &[9], false, false).unwrap_err();
        assert_eq!(e, "the disc has no title 9 (titles are numbered 1 to 3)");
        let empty = Disc { titles: vec![], ..disc() };
        assert!(select_titles(&empty, &[], false, false).is_err());
    }

    #[test]
    fn profile_listing_has_every_profile() {
        let text = format_profiles();
        for p in PROFILES {
            assert!(text.contains(p.name), "{}", p.name);
        }
        assert!(text.contains("Audio only:") && text.contains("Devices:"));
    }

    #[test]
    fn list_output_marks_main_title_and_lengths() {
        let text = format_list(&disc(), &[]);
        assert!(text.starts_with("Blu-ray \"My Disc\": 3 titles\n"), "{text}");
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.iter().any(|l| l.starts_with(" *    1") && l.contains("20.00 GB") && l.contains("02:00:00") && l.contains("playlist 00800")), "{text}");
        assert!(lines.iter().any(|l| l.starts_with("      2") && l.contains("00:01:00")), "{text}");
        assert!(lines.iter().any(|l| l.starts_with("      3") && l.contains("  -  ") && l.contains("1 file")), "{text}");
        assert!(text.ends_with("* = main title\n"));
    }

    #[test]
    fn list_output_notes_protection_and_singular() {
        let d = Disc { kind: DiscKind::Dvd, titles: vec![title(1, 5, None, "")], encrypted: true, ..disc() };
        assert!(format_list(&d, &[]).starts_with("DVD \"My Disc\": 1 title (copy-protected)\n"));
    }

    #[test]
    fn list_details_show_picture_and_tracks() {
        let info = TitleInfo {
            duration: Some(90.0),
            width: Some(1920),
            height: Some(1080),
            audio: vec![Stream { codec: "ac3".into(), language: Some("eng".into()), channels: Some(6) }, Stream { codec: "dts".into(), language: None, channels: Some(2) }],
            subtitles: vec![Stream { codec: "pgs".into(), language: Some("fra".into()), channels: None }],
            thumb: None,
        };
        let d = Disc { titles: vec![title(1, 5, None, "")], ..disc() };
        let text = format_list(&d, &[Some(info)]);
        assert!(text.contains("1920x1080"), "{text}");
        assert!(text.contains("Audio: 1. ENG AC3 5.1; 2. DTS stereo"), "{text}");
        assert!(text.contains("Subtitles: 1. FRA PGS"), "{text}");
        assert!(text.contains("00:01:30"), "length comes from probing when the title has none: {text}");
    }

    #[test]
    fn progress_lines() {
        assert_eq!(progress_line(2, 3, 5, 0.425, 1.84), "[2/3] Title 5   42%  1.8x");
        assert_eq!(progress_line(1, 1, 1, 1.5, 0.0), "[1/1] Title 1  100%");
        assert_eq!(progress_line(1, 1, 1, -1.0, 0.0), "[1/1] Title 1    0%");
    }

    #[test]
    fn unknown_command_line_errors_exit_with_usage_status() {
        assert_eq!(run(&args("rip --bogus /d")), Some(EXIT_USAGE));
        assert_eq!(run(&args("list")), Some(EXIT_USAGE));
        assert_eq!(run(&args("--version")), Some(EXIT_OK));
        assert_eq!(run(&args("profiles")), Some(EXIT_OK));
        assert_eq!(run(&args("help")), Some(EXIT_OK));
        assert_eq!(run(&[]), None);
    }

    #[test]
    fn missing_disc_or_ffmpeg_exit_with_failure_status() {
        assert_eq!(run(&args("list /definitely/not/a/disc")), Some(EXIT_FAILED));
        assert_eq!(run(&args("rip --ffmpeg /no/such/ffmpeg /definitely/not/a/disc")), Some(EXIT_FAILED));
    }

    #[test]
    fn list_runs_on_a_dvd_folder() {
        let dir = std::env::temp_dir().join(format!("openrip-cli-list-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("VIDEO_TS")).unwrap();
        fs::write(dir.join("VIDEO_TS/VTS_01_1.VOB"), vec![0u8; 10]).unwrap();
        assert_eq!(run(&[String::from("list"), dir.to_string_lossy().into_owned()]), Some(EXIT_OK));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    fn script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        fs::write(&p, format!("#!/bin/sh\n[ -n \"$OPENRIP_PROBE\" ] && exit 0\n{body}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..200 {
            match std::process::Command::new(&p).env("OPENRIP_PROBE", "1").output() {
                Err(e) if e.raw_os_error() == Some(26) => std::thread::sleep(std::time::Duration::from_millis(5)),
                _ => break,
            }
        }
        p
    }

    #[cfg(unix)]
    fn dvd_with_fakes(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("openrip-cli-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let disc = dir.join("MOVIE");
        fs::create_dir_all(disc.join("VIDEO_TS")).unwrap();
        fs::write(disc.join("VIDEO_TS/VTS_01_1.VOB"), vec![0u8; 100]).unwrap();
        fs::write(disc.join("VIDEO_TS/VTS_02_1.VOB"), vec![0u8; 50]).unwrap();
        // ffprobe: one video, one audio, a 100 s length. ffmpeg: "encodes" by writing the output file.
        script(&dir, "ffprobe", "printf 'stream|codec_type=video|width=720|height=480\\nstream|codec_type=audio|codec_name=ac3|channels=2\\nformat|duration=100.0\\n'");
        // Thumbnails and hardware-encoder checks also call ffmpeg, ending in "pipe:1" or "-": don't make files of those.
        let ffmpeg = script(&dir, "ffmpeg", "for a; do out=\"$a\"; done\ncase \"$out\" in pipe:*|-) exit 0;; esac\necho data > \"$out\"\necho out_time_us=50000000\necho speed=2x\necho progress=end");
        (dir, disc, ffmpeg)
    }

    #[cfg(unix)]
    #[test]
    fn rip_end_to_end_with_fake_ffmpeg() {
        let (dir, disc, ffmpeg) = dvd_with_fakes("rip");
        let out = dir.join("out");
        let a = vec![
            "rip".to_string(),
            disc.to_string_lossy().into_owned(),
            "--all".into(),
            "--profile".into(),
            "mkv".into(),
            "--no-hardware".into(),
            "--out".into(),
            out.to_string_lossy().into_owned(),
            "--ffmpeg".into(),
            ffmpeg.to_string_lossy().into_owned(),
        ];
        assert_eq!(run(&a), Some(EXIT_OK));
        assert!(out.join("MOVIE - Title 1.mkv").is_file());
        assert!(out.join("MOVIE - Title 2.mkv").is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn rip_reports_failure_status_when_ffmpeg_fails() {
        let (dir, disc, _) = dvd_with_fakes("fail");
        let bad = script(&dir, "badffmpeg", "echo nope >&2\nexit 1");
        let a = vec!["rip".to_string(), disc.to_string_lossy().into_owned(), "--ffmpeg".into(), bad.to_string_lossy().into_owned(), "--out".into(), dir.join("o").to_string_lossy().into_owned()];
        assert_eq!(run(&a), Some(EXIT_FAILED));
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn rip_rejects_unavailable_audio_track_and_unknown_title() {
        let (dir, disc, ffmpeg) = dvd_with_fakes("audio");
        let base = |extra: &[&str]| {
            let mut v = vec!["rip".to_string(), disc.to_string_lossy().into_owned(), "--ffmpeg".into(), ffmpeg.to_string_lossy().into_owned(), "--out".into(), dir.join("o").to_string_lossy().into_owned()];
            v.extend(extra.iter().map(|s| s.to_string()));
            v
        };
        assert_eq!(run(&base(&["--audio", "3"])), Some(EXIT_FAILED));
        assert_eq!(run(&base(&["--title", "9"])), Some(EXIT_FAILED));
        assert!(!dir.join("o").read_dir().map(|mut d| d.next().is_some()).unwrap_or(false), "nothing was ripped");
        let _ = fs::remove_dir_all(&dir);
    }
}
