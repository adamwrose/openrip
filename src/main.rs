#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! OpenRip: an open-source DVD and Blu-ray ripper with a friendly interface.
//! The ripping itself is done by FFmpeg.

mod bluray;
mod cli;
mod disc;
mod preview;
mod rip;

use disc::{Disc, DiscKind, MountedDisc, Title, TitleInfo};
use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Frame, Layout, Margin, RichText, Sense,
    Stroke, StrokeKind, TextureHandle, Theme, Vec2, pos2, vec2,
};
use rip::{Group, Profile, PROFILES, Quality, Resolution};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::Arc;

const APP_NAME: &str = "OpenRip";

fn default_output_dir() -> PathBuf {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let base = home.unwrap_or_else(|| PathBuf::from("."));

    #[cfg(target_os = "macos")]
    let videos = "Movies";
    #[cfg(not(target_os = "macos"))]
    let videos = "Videos";

    base.join(videos).join(APP_NAME)
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    // `openrip list|rip|profiles|--help ...` runs in the terminal instead of opening the window.
    if let Some(code) = cli::run(&args[1..]) {
        std::process::exit(code);
    }
    // The release build has no console on Windows, so show startup failures in a dialog.
    std::panic::set_hook(Box::new(|info| show_error(&format!("OpenRip crashed:\n\n{info}"))));
    bluray::init();
    let screenshot = args
        .iter()
        .position(|a| a == "--screenshot")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from);
    // Windows VMs and basic display drivers only offer OpenGL 1.1, which glow can't use, so
    // try wgpu first there (DirectX 12, with a software fallback) and glow if that fails.
    let renderers: &[eframe::Renderer] = if cfg!(windows) {
        &[eframe::Renderer::Wgpu, eframe::Renderer::Glow]
    } else {
        &[eframe::Renderer::Glow]
    };
    let mut errors = Vec::new();
    for &renderer in renderers {
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_title(format!("{APP_NAME} — DVD & Blu-ray Ripper"))
                // Lets Wayland and X11 match the window to its .desktop file and icon.
                .with_app_id("io.github.adamwrose.OpenRip")
                .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../packaging/windows/icon-256.png")).expect("app icon"))
                .with_inner_size([1120.0, 720.0])
                .with_min_inner_size([860.0, 560.0]),
            renderer,
            ..Default::default()
        };
        let args = args.clone();
        let screenshot = screenshot.clone();
        let result = eframe::run_native(
            APP_NAME,
            options,
            Box::new(move |cc| {
                // `--open <folder or ISO>` starts on that disc instead of the one in the drive.
                let open = args.iter().position(|a| a == "--open").and_then(|i| args.get(i + 1)).map(PathBuf::from);
                let mut app = RipperApp::new(&cc.egui_ctx, screenshot, open.is_none());
                if let Some(path) = open {
                    app.open_source(&cc.egui_ctx, path);
                }
                // `--tab settings|about|formats` opens on that screen (handy for screenshots).
                match args.iter().position(|a| a == "--tab").and_then(|i| args.get(i + 1)).map(String::as_str) {
                    Some("settings") => app.tab = Tab::Settings,
                    Some("about") => app.tab = Tab::About,
                    Some("formats") => app.picker_open = true,
                    Some("preview") => app.preview_on_load = true,
                    _ => {}
                }
                Ok(Box::new(app))
            }),
        );
        match result {
            Ok(()) => return Ok(()),
            Err(e) => errors.push(format!("{renderer:?}: {e}")),
        }
    }
    show_error(&format!(
        "OpenRip could not start its window.\n\n{}\n\nThis usually means the graphics driver \
         does not support OpenGL 2.0+ or DirectX 12.",
        errors.join("\n")
    ));
    Err(eframe::Error::AppCreation(errors.join("; ").into()))
}

fn show_error(message: &str) {
    eprintln!("{message}");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title(APP_NAME)
        .set_description(message)
        .show();
}

// ---------------------------------------------------------------- colours

#[derive(Clone, Copy)]
struct Palette {
    bg: Color32,
    card: Color32,
    header: Color32,
    border: Color32,
    text: Color32,
    muted: Color32,
    accent: Color32,
    accent_soft: Color32,
    good: Color32,
    bad: Color32,
}

const ACCENT: Color32 = Color32::from_rgb(0x2F, 0x6B, 0xFF);

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: Color32::from_rgb(0x15, 0x17, 0x1C),
            card: Color32::from_rgb(0x1F, 0x22, 0x2A),
            header: Color32::from_rgb(0x1A, 0x1D, 0x24),
            border: Color32::from_rgb(0x30, 0x34, 0x3E),
            text: Color32::from_rgb(0xE8, 0xEB, 0xF2),
            muted: Color32::from_rgb(0x8E, 0x95, 0xA5),
            accent: Color32::from_rgb(0x4C, 0x82, 0xFF),
            accent_soft: Color32::from_rgb(0x22, 0x30, 0x52),
            good: Color32::from_rgb(0x3D, 0xC7, 0x8A),
            bad: Color32::from_rgb(0xF0, 0x6A, 0x6A),
        }
    } else {
        Palette {
            bg: Color32::from_rgb(0xF3, 0xF5, 0xF9),
            card: Color32::WHITE,
            header: Color32::WHITE,
            border: Color32::from_rgb(0xE1, 0xE5, 0xEE),
            text: Color32::from_rgb(0x1E, 0x23, 0x30),
            muted: Color32::from_rgb(0x7A, 0x82, 0x93),
            accent: ACCENT,
            accent_soft: Color32::from_rgb(0xE8, 0xEF, 0xFF),
            good: Color32::from_rgb(0x1E, 0xA8, 0x6B),
            bad: Color32::from_rgb(0xD9, 0x3F, 0x3F),
        }
    }
}

fn style_visuals(ctx: &egui::Context) {
    for (theme, dark) in [(Theme::Light, false), (Theme::Dark, true)] {
        let p = palette(dark);
        let mut v = if dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        v.panel_fill = p.bg;
        v.window_fill = p.card;
        v.extreme_bg_color = if dark { Color32::from_rgb(0x17, 0x19, 0x1F) } else { Color32::from_rgb(0xF7, 0xF8, 0xFB) };
        v.window_corner_radius = CornerRadius::same(12);
        v.selection.bg_fill = p.accent;
        // Text on blue (selected options, pressed buttons, progress bars) is white.
        v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
        v.widgets.active.weak_bg_fill = p.accent;
        v.widgets.active.bg_fill = p.accent;
        v.widgets.active.bg_stroke = Stroke::new(1.0, p.accent);
        v.widgets.active.fg_stroke = Stroke::new(1.0, Color32::WHITE);
        v.hyperlink_color = p.accent;
        for w in [
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.corner_radius = CornerRadius::same(7);
        }
        v.widgets.inactive.weak_bg_fill = if dark { Color32::from_rgb(0x2A, 0x2E, 0x38) } else { Color32::from_rgb(0xEE, 0xF1, 0xF6) };
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, p.border);
        v.widgets.hovered.weak_bg_fill = p.accent_soft;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, p.accent);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
        ctx.set_visuals_of(theme, v);
        ctx.style_mut_of(theme, |s| {
            s.spacing.item_spacing = vec2(8.0, 8.0);
            s.spacing.button_padding = vec2(12.0, 6.0);
            s.spacing.interact_size.y = 30.0;
        });
    }
}

// ---------------------------------------------------------------- state

#[derive(Clone)]
enum RowState {
    Ready,
    Waiting,
    Ripping { fraction: f32, speed: f32 },
    Done(PathBuf),
    Failed(String),
}

struct Row {
    title: Title,
    info: Option<TitleInfo>,
    texture: Option<TextureHandle>,
    selected: bool,
    audio: usize,
    subtitles: bool,
    state: RowState,
}

enum Loaded {
    Disc(u64, Result<Disc, String>),
    Info(u64, usize, TitleInfo),
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Ripper,
    Settings,
    About,
}

struct RipperApp {
    dark: bool,
    tab: Tab,
    ffmpeg_path: String,
    mounted: Vec<MountedDisc>,
    disc: Option<Disc>,
    rows: Vec<Row>,
    main_title: Option<usize>,
    show_short: bool,
    generation: u64,
    loading: bool,
    message: Option<(bool, String)>,
    profile: usize,
    picker_open: bool,
    picker_group: Group,
    settings: rip::Settings,
    out_dir: String,
    merge: bool,
    open_after: bool,
    load_tx: Sender<Loaded>,
    load_rx: Receiver<Loaded>,
    rip_rx: Option<Receiver<rip::Msg>>,
    control: rip::Control,
    ripping: bool,
    screenshot: Option<PathBuf>,
    frames: u32,
    shot_saved: Arc<AtomicBool>,
    /// When the drives were last looked at, to spot a disc being inserted.
    last_scan: f64,
    preview: Option<PreviewState>,
    /// Set while a preview plays or a rip runs, so background reading of the disc waits.
    hold_probe: Arc<AtomicBool>,
    muted: Arc<AtomicBool>,
    /// Open the main title's preview as soon as a disc is read (for screenshots).
    preview_on_load: bool,
}

/// The preview player window for one title.
struct PreviewState {
    slot: usize,
    source: preview::Source,
    session: Option<preview::Session>,
    texture: Option<TextureHandle>,
    /// Where playback is (or was paused), in seconds.
    position: f64,
    error: Option<String>,
}

impl RipperApp {
    fn new(ctx: &egui::Context, screenshot: Option<PathBuf>, auto_open: bool) -> Self {
        style_visuals(ctx);
        ctx.set_theme(Theme::Light);
        let ffmpeg = disc::find_tool("ffmpeg")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Look for a hardware encoder in the background; the settings show it once found.
        if let Some(path) = disc::find_tool("ffmpeg") {
            let ctx = ctx.clone();
            std::thread::spawn(move || {
                rip::hardware_encoder(&path);
                ctx.request_repaint();
            });
        }
        let out_dir = default_output_dir().to_string_lossy().into_owned();
        let (load_tx, load_rx) = channel();
        let mut app = Self {
            dark: false,
            tab: Tab::Ripper,
            ffmpeg_path: ffmpeg,
            mounted: disc::mounted_discs(),
            disc: None,
            rows: Vec::new(),
            main_title: None,
            show_short: false,
            generation: 0,
            loading: false,
            message: None,
            profile: 0,
            picker_open: false,
            picker_group: Group::Video,
            settings: rip::Settings {
                resolution: Resolution::Source,
                quality: Quality::High,
                // Used only where a hardware encoder is found (see rip::hardware_encoder).
                hardware: true,
                deinterlace: true,
            },
            out_dir,
            merge: false,
            open_after: true,
            load_tx,
            load_rx,
            rip_rx: None,
            control: rip::Control::default(),
            ripping: false,
            screenshot,
            frames: 0,
            shot_saved: Arc::new(AtomicBool::new(false)),
            last_scan: 0.0,
            preview: None,
            hold_probe: Arc::new(AtomicBool::new(false)),
            muted: Arc::new(AtomicBool::new(false)),
            preview_on_load: false,
        };
        // A disc already in the drive is opened straight away.
        if auto_open && app.mounted.len() == 1 {
            let path = app.mounted[0].path.clone();
            app.open_source(ctx, path);
        }
        app
    }

    fn ffmpeg(&self) -> Option<PathBuf> {
        let p = PathBuf::from(self.ffmpeg_path.trim());
        p.is_file().then_some(p)
    }

    fn open_source(&mut self, ctx: &egui::Context, path: PathBuf) {
        if self.ripping {
            return;
        }
        self.generation += 1;
        self.loading = true;
        self.message = None;
        self.disc = None;
        self.rows.clear();
        let tx = self.load_tx.clone();
        let generation = self.generation;
        let ffmpeg = self.ffmpeg();
        let ctx = ctx.clone();
        let hold = self.hold_probe.clone();
        std::thread::spawn(move || {
            let result = disc::open(&path);
            let titles = result.as_ref().map(|d| d.titles.clone()).unwrap_or_default();
            let _ = tx.send(Loaded::Disc(generation, result));
            ctx.request_repaint();
            // Probe the biggest titles first so the main feature fills in quickly.
            if let Some(ffmpeg) = ffmpeg {
                let mut order: Vec<usize> = (0..titles.len()).collect();
                order.sort_by_key(|&i| std::cmp::Reverse(titles[i].size));
                for (rank, i) in order.into_iter().enumerate() {
                    // One reader at a time: a disc drive slows to a crawl when read in two places.
                    while hold.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(300));
                    }
                    // Previewing a protected title means unlocking part of it from the drive,
                    // so only the dozen biggest get a preview; the rest show their playlist length.
                    let info = if titles[i].locked.is_some() && rank >= 12 {
                        TitleInfo { duration: titles[i].duration, ..Default::default() }
                    } else {
                        disc::probe(&ffmpeg, &titles[i])
                    };
                    if tx.send(Loaded::Info(generation, i, info)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            }
        });
    }

    fn row_visible(&self, i: usize) -> bool {
        if self.show_short || Some(i) == self.main_title {
            return true;
        }
        let row = &self.rows[i];
        let long_enough = row
            .info
            .as_ref()
            .and_then(|f| f.duration)
            .or(row.title.duration)
            .map(|d| d >= 60.0)
            .unwrap_or(true);
        // Under ~150 MB per hour is menus, stills or a decoy playlist, not real video.
        let real_video = row
            .title
            .duration
            .map(|d| row.title.size as f64 / d.max(1.0) >= 40_000.0)
            .unwrap_or(true);
        long_enough && real_video && row.title.size >= 20_000_000
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.load_rx.try_recv() {
            match msg {
                Loaded::Disc(g, result) if g == self.generation => {
                    self.loading = false;
                    match result {
                        Ok(d) => {
                            self.main_title = d.main_title();
                            self.rows = d
                                .titles
                                .iter()
                                .enumerate()
                                .map(|(i, t)| Row {
                                    title: t.clone(),
                                    info: None,
                                    texture: None,
                                    selected: Some(i) == self.main_title,
                                    audio: 0,
                                    subtitles: true,
                                    state: RowState::Ready,
                                })
                                .collect();
                            self.settings.deinterlace = d.kind == DiscKind::Dvd;
                            self.disc = Some(d);
                        }
                        Err(e) => self.message = Some((false, e)),
                    }
                }
                Loaded::Info(g, i, info) if g == self.generation => {
                    if let Some(row) = self.rows.get_mut(i) {
                        if let Some((rgba, w, h)) = &info.thumb {
                            let img = egui::ColorImage::from_rgba_unmultiplied([*w, *h], rgba);
                            row.texture = Some(ctx.load_texture(
                                format!("thumb-{g}-{i}"),
                                img,
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                        row.info = Some(info);
                    }
                }
                _ => {}
            }
        }
        let mut finished = false;
        if let Some(rx) = &self.rip_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    rip::Msg::Started(s) => {
                        self.rows[s].state = RowState::Ripping { fraction: 0.0, speed: 0.0 }
                    }
                    rip::Msg::Progress { slot, fraction, speed } => {
                        self.rows[slot].state = RowState::Ripping { fraction, speed }
                    }
                    rip::Msg::Finished { slot, result } => {
                        self.rows[slot].state = match result {
                            Ok(p) => RowState::Done(p),
                            Err(e) => RowState::Failed(e),
                        }
                    }
                    rip::Msg::Merged(r) => {
                        self.message = Some(match r {
                            Ok(p) => (true, format!("Merged into {}", file_name(&p))),
                            Err(e) => (false, format!("Couldn't merge the titles: {e}")),
                        })
                    }
                    rip::Msg::AllDone => finished = true,
                }
            }
        }
        if finished {
            self.ripping = false;
            self.rip_rx = None;
            for r in &mut self.rows {
                if matches!(r.state, RowState::Waiting) {
                    r.state = RowState::Ready;
                }
            }
            let ok = self.rows.iter().filter(|r| matches!(r.state, RowState::Done(_))).count();
            let failed = self.rows.iter().filter(|r| matches!(r.state, RowState::Failed(_))).count();
            if self.message.is_none() {
                self.message = Some(if failed == 0 {
                    (true, format!("Finished: {ok} title(s) saved."))
                } else {
                    (false, format!("{ok} title(s) saved, {failed} failed."))
                });
            }
            if ok > 0 && self.open_after {
                if let Err(e) = disc::open_location(Path::new(&self.out_dir), false) {
                    self.message = Some((false, format!("Couldn't open the output folder: {e}")));
                }
            }
        }
    }

    fn start_rip(&mut self, ctx: &egui::Context) {
        let Some(ffmpeg) = self.ffmpeg() else {
            self.message = Some((false, "FFmpeg wasn't found. Install FFmpeg with your package manager or set its location in Settings.".into()));
            return;
        };
        let Some(disc) = &self.disc else { return };
        let dir = PathBuf::from(self.out_dir.trim());
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.message = Some((false, format!("Can't use that output folder: {e}")));
            return;
        }
        let profile = PROFILES[self.profile];
        let mut jobs = Vec::new();
        for i in 0..self.rows.len() {
            if !self.rows[i].selected || !self.row_visible(i) {
                continue;
            }
            let row = &mut self.rows[i];
            let stem = format!("{} - Title {}", disc.name, row.title.number);
            jobs.push(rip::Job {
                slot: i,
                input: row.title.input(),
                duration: row.info.as_ref().and_then(|f| f.duration),
                audio_track: row.info.as_ref().filter(|f| !f.audio.is_empty()).map(|_| row.audio),
                subtitles: row.subtitles,
                output: rip::free_path(&dir, &stem, profile.ext),
                profile,
                locked: row.title.locked.clone().zip(row.title.playlist),
            });
            row.state = RowState::Waiting;
        }
        if jobs.is_empty() {
            self.message = Some((false, "Tick at least one title to rip.".into()));
            return;
        }
        let merge_to = (self.merge && jobs.len() > 1)
            .then(|| rip::free_path(&dir, &format!("{} - All titles", disc.name), profile.ext));
        let (tx, rx) = channel();
        self.control = rip::Control::default();
        self.rip_rx = Some(rx);
        self.ripping = true;
        self.message = None;
        let ctx = ctx.clone();
        rip::start(ffmpeg, jobs, self.settings.clone(), merge_to, self.control.clone(), tx, move || {
            ctx.request_repaint()
        });
    }

    fn pick_folder(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new()
            .set_title("Choose a DVD or Blu-ray folder (VIDEO_TS or BDMV)")
            .pick_folder()
        {
            self.open_source(ctx, p);
        }
    }

    fn pick_iso(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new()
            .set_title("Choose a disc image")
            .add_filter("Disc image", &["iso", "ISO"])
            .pick_file()
        {
            self.open_source(ctx, p);
        }
    }
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

// ---------------------------------------------------------------- drawing helpers

fn primary_button(ui: &mut egui::Ui, p: &Palette, text: &str, size: Vec2, enabled: bool) -> egui::Response {
    // Stays blue with white text when unavailable (just paler), and ignores clicks then.
    let fill = if enabled { p.accent } else { p.accent.lerp_to_gamma(p.bg, 0.3) };
    let resp = ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE).strong().size(15.0))
            .fill(fill)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(8))
            .min_size(size)
            .sense(if enabled { Sense::click() } else { Sense::hover() }),
    );
    if enabled { resp } else { resp.on_hover_text("Load a disc and tick a title first") }
}

fn logo(ui: &mut egui::Ui, p: &Palette) {
    let (rect, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, CornerRadius::same(9), p.accent);
    let c = rect.center();
    painter.circle_filled(c, 11.0, Color32::WHITE);
    painter.circle_filled(c, 7.5, p.accent.gamma_multiply(0.35));
    painter.circle_filled(c, 3.0, p.accent);
}

fn disc_glyph(painter: &egui::Painter, c: egui::Pos2, r: f32, color: Color32) {
    painter.circle_stroke(c, r, Stroke::new(2.0, color));
    painter.circle_stroke(c, r * 0.62, Stroke::new(1.0, color.gamma_multiply(0.5)));
    painter.circle_filled(c, r * 0.22, color);
}

fn badge(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) {
    Frame::new()
        .fill(bg)
        .corner_radius(CornerRadius::same(5))
        .inner_margin(Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(RichText::new(text).size(11.0).strong().color(fg));
        });
}

// ---------------------------------------------------------------- the window

impl eframe::App for RipperApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        self.watch_drives(&ctx);
        let p = palette(self.dark);

        // Drag & drop of a disc folder or ISO.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if let Some(path) = dropped.into_iter().next() {
            self.tab = Tab::Ripper;
            self.open_source(&ctx, path);
        }

        self.header(ui, &p);
        if self.tab == Tab::Ripper {
            self.footer(ui, &p);
        }
        egui::CentralPanel::default()
            .frame(Frame::new().fill(p.bg).inner_margin(Margin::symmetric(20, 14)))
            .show(ui, |ui| match self.tab {
                Tab::Ripper => self.ripper(ui, &p),
                Tab::Settings => self.settings_page(ui, &p),
                Tab::About => self.about_page(ui, &p),
            });

        if self.picker_open {
            self.format_picker(&ctx, &p);
        }
        if self.preview_on_load && self.rows.iter().any(|r| r.info.is_some()) {
            self.preview_on_load = false;
            if let Some(i) = self.main_title {
                self.open_preview(&ctx, i);
            }
        }
        if self.preview.is_some() {
            self.preview_window(&ctx, &p);
        }
        let playing = self.preview.as_ref().is_some_and(|pv| pv.session.is_some());
        self.hold_probe.store(playing || self.ripping, Ordering::SeqCst);
        if self.loading || self.ripping {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        self.screenshot_step(&ctx);
    }
}

impl RipperApp {
    /// Every two seconds, look for a newly inserted disc and open it.
    fn watch_drives(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
        if now - self.last_scan < 2.0 {
            return;
        }
        self.last_scan = now;
        let found = disc::mounted_discs();
        let inserted = found
            .iter()
            .find(|m| !self.mounted.iter().any(|o| o.path == m.path))
            .map(|m| m.path.clone());
        self.mounted = found;
        if let Some(path) = inserted.filter(|_| !self.ripping) {
            self.tab = Tab::Ripper;
            self.open_source(ctx, path);
        }
    }

    fn screenshot_step(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot.clone() else { return };
        // When opened on a preview, wait (up to 40 s) for its first picture.
        let waiting = self.preview_on_load || self.preview.as_ref().is_some_and(|pv| pv.texture.is_none() && pv.error.is_none());
        if waiting && self.frames == 119 && ctx.input(|i| i.time) < 40.0 {
            ctx.request_repaint();
            return;
        }
        self.frames += 1;
        ctx.request_repaint();
        if self.frames == 120 {
            let saved = self.shot_saved.clone();
            ctx.request_screenshot(move |img| {
                let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Some(buf) = image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, bytes) {
                    if let Err(e) = buf.save(&path) {
                        eprintln!("couldn't save screenshot: {e}");
                    }
                }
                saved.store(true, Ordering::SeqCst);
            });
        }
        if self.shot_saved.load(Ordering::SeqCst) || self.frames > 600 {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn header(&mut self, ui: &mut egui::Ui, p: &Palette) {
        egui::Panel::top("header")
            .frame(
                Frame::new()
                    .fill(p.header)
                    .inner_margin(Margin::symmetric(18, 10))
                    .stroke(Stroke::new(1.0, p.border)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    logo(ui, p);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new(APP_NAME).size(17.0).strong().color(p.text));
                        ui.label(RichText::new("DVD & Blu-ray Ripper · open source").size(11.0).color(p.muted));
                    });
                    ui.add_space(40.0);
                    for (tab, label) in [(Tab::Ripper, "Ripper"), (Tab::Settings, "Settings"), (Tab::About, "About")] {
                        let on = self.tab == tab;
                        let (rect, resp) = ui.allocate_exact_size(vec2(96.0, 34.0), Sense::click());
                        let color = if on { p.accent } else if resp.hovered() { p.text } else { p.muted };
                        ui.painter().text(rect.center(), Align2::CENTER_CENTER, label, FontId::proportional(15.0), color);
                        if on {
                            let bar = egui::Rect::from_center_size(pos2(rect.center().x, rect.bottom() + 8.0), vec2(40.0, 3.0));
                            ui.painter().rect_filled(bar, CornerRadius::same(2), p.accent);
                        }
                        if resp.clicked() {
                            self.tab = tab;
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = if self.dark { "☀ Light" } else { "🌙 Dark" };
                        if ui.button(label).on_hover_text("Switch light / dark look").clicked() {
                            self.dark = !self.dark;
                            ui.ctx().set_theme(if self.dark { Theme::Dark } else { Theme::Light });
                        }
                    });
                });
            });
    }

    fn footer(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let ctx = ui.ctx().clone();
        egui::Panel::bottom("footer")
            .frame(
                Frame::new()
                    .fill(p.header)
                    .inner_margin(Margin::symmetric(20, 14))
                    .stroke(Stroke::new(1.0, p.border)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Save to:").color(p.muted));
                    ui.add_enabled(!self.ripping, egui::TextEdit::singleline(&mut self.out_dir).desired_width(340.0));
                    if ui.add_enabled(!self.ripping, egui::Button::new("Browse…")).clicked() {
                        if let Some(d) = rfd::FileDialog::new().set_title("Save rips to").pick_folder() {
                            self.out_dir = d.to_string_lossy().into_owned();
                        }
                    }
                    if ui.button("Open folder").clicked() {
                        match std::fs::create_dir_all(&self.out_dir) {
                            Ok(()) => {
                                if let Err(e) = disc::open_location(Path::new(&self.out_dir), false) {
                                    self.message = Some((false, format!("Couldn't open the output folder: {e}")));
                                }
                            }
                            Err(e) => self.message = Some((false, format!("Couldn't create the output folder: {e}"))),
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if self.ripping {
                            if primary_button(ui, p, "Stop", vec2(130.0, 44.0), true).clicked() {
                                self.control.stop();
                            }
                            let (done, total, frac) = self.overall();
                            ui.add(
                                egui::ProgressBar::new(frac)
                                    .desired_width(180.0)
                                    .fill(p.accent)
                                    .text(format!("{done} of {total} · {:.0}%", frac * 100.0)),
                            );
                        } else {
                            let selected = (0..self.rows.len()).filter(|&i| self.rows[i].selected && self.row_visible(i)).count();
                            if primary_button(ui, p, "Rip All", vec2(130.0, 44.0), selected > 0).clicked() {
                                self.start_rip(&ctx);
                            }
                            ui.add_space(6.0);
                            ui.checkbox(&mut self.merge, "Merge into one file");
                        }
                    });
                });
            });
    }

    fn overall(&self) -> (usize, usize, f32) {
        let active: Vec<&Row> = self
            .rows
            .iter()
            .filter(|r| !matches!(r.state, RowState::Ready))
            .collect();
        let total = active.len().max(1);
        let mut sum = 0.0;
        let mut done = 0;
        for r in &active {
            sum += match r.state {
                RowState::Ripping { fraction, .. } => fraction,
                RowState::Done(_) | RowState::Failed(_) => {
                    done += 1;
                    1.0
                }
                _ => 0.0,
            };
        }
        (done, total, sum / total as f32)
    }

    fn ripper(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let ctx = ui.ctx().clone();
        // Toolbar: load buttons on the left, output format on the right.
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!self.ripping, |ui| {
                let load = egui::Button::new(RichText::new("💿  Load Disc  ⏷").color(Color32::WHITE).strong())
                    .fill(p.accent)
                    .stroke(Stroke::NONE)
                    .min_size(vec2(0.0, 36.0));
                let resp = ui.add(load);
                egui::Popup::menu(&resp).show(|ui| {
                    ui.set_min_width(240.0);
                    if self.mounted.is_empty() {
                        ui.label(RichText::new("No disc in the drive").color(p.muted));
                    }
                    let mut chosen = None;
                    for m in &self.mounted {
                        if ui.button(format!("{}  ({})", m.name, m.kind.label())).clicked() {
                            chosen = Some(m.path.clone());
                        }
                    }
                    if let Some(c) = chosen {
                        self.open_source(&ctx, c);
                    }
                    ui.separator();
                    if ui.button("Load DVD / Blu-ray folder…").clicked() {
                        self.pick_folder(&ctx);
                    }
                    if ui.button("Load ISO image…").clicked() {
                        self.pick_iso(&ctx);
                    }
                });
                if ui.add(egui::Button::new("⟳  Refresh drives").min_size(vec2(0.0, 36.0))).clicked() {
                    self.mounted = disc::mounted_discs();
                    if self.disc.is_none() && self.mounted.len() == 1 {
                        let path = self.mounted[0].path.clone();
                        self.open_source(&ctx, path);
                    }
                }
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let pr = PROFILES[self.profile];
                let resp = ui.add_enabled(
                    !self.ripping,
                    egui::Button::new(RichText::new(format!("{}  ·  {}   ⏷", pr.name, pr.note)).color(p.text))
                        .min_size(vec2(230.0, 36.0)),
                );
                if resp.clicked() {
                    self.picker_open = true;
                    self.picker_group = pr.group;
                }
                ui.label(RichText::new("Rip All to:").color(p.muted).size(14.0));
            });
        });
        ui.add_space(6.0);

        if let Some((ok, text)) = self.message.clone() {
            let (fg, bg) = if ok { (p.good, p.good.gamma_multiply(0.12)) } else { (p.bad, p.bad.gamma_multiply(0.12)) };
            Frame::new()
                .fill(bg)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&text).color(fg));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("✕").clicked() {
                                self.message = None;
                            }
                        });
                    });
                });
            ui.add_space(6.0);
        }
        if self.ffmpeg().is_none() {
            Frame::new()
                .fill(p.bad.gamma_multiply(0.10))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("FFmpeg (the free engine that does the ripping) wasn't found. Install it with your package manager or set its location in Settings.").color(p.bad));
                    });
                });
            ui.add_space(6.0);
        }

        if self.disc.is_none() {
            self.empty_state(ui, p);
            return;
        }
        let disc = self.disc.clone().unwrap();
        // Disc summary line.
        ui.horizontal(|ui| {
            badge(ui, disc.kind.label(), Color32::WHITE, p.accent);
            ui.label(RichText::new(&disc.name).size(16.0).strong().color(p.text));
            let shown = (0..self.rows.len()).filter(|&i| self.row_visible(i)).count();
            ui.label(RichText::new(format!("{shown} of {} titles shown", self.rows.len())).color(p.muted));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_enabled_ui(!self.ripping, |ui| {
                    if ui.button("Select none").clicked() {
                        self.rows.iter_mut().for_each(|r| r.selected = false);
                    }
                    if ui.button("Select all").clicked() {
                        for i in 0..self.rows.len() {
                            let v = self.row_visible(i);
                            self.rows[i].selected = v;
                        }
                    }
                    ui.checkbox(&mut self.show_short, "Show short clips");
                });
            });
        });
        ui.add_space(4.0);

        if disc.encrypted {
            let missing = bluray::missing();
            let (color, text) = if missing.is_empty() {
                (p.good, "🔓 Copy-protected Blu-ray: it's unlocked with the open-source libaacs and your key file while ripping.".to_string())
            } else {
                (p.bad, format!("This Blu-ray is copy-protected. To rip it, install: {}.", missing.join("; ")))
            };
            Frame::new()
                .fill(color.gamma_multiply(0.10))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(text).color(color));
                });
            ui.add_space(6.0);
        }

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            // The main movie always leads the list.
            let order = self.main_title.into_iter().chain((0..self.rows.len()).filter(|&i| Some(i) != self.main_title));
            for i in order.collect::<Vec<_>>() {
                if self.row_visible(i) {
                    self.title_row(ui, p, i);
                    ui.add_space(8.0);
                }
            }
        });
    }

    fn empty_state(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let ctx = ui.ctx().clone();
        let avail = ui.available_rect_before_wrap();
        let rect = avail.shrink2(vec2(0.0, 6.0));
        let resp = ui.allocate_rect(rect, Sense::click());
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(14), p.card);
        let hover = resp.hovered() && !self.loading;
        let line = if hover { p.accent } else { p.border };
        let r = rect.shrink(14.0);
        let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
        painter.extend(egui::Shape::dashed_line(&corners, Stroke::new(1.5, line), 8.0, 6.0));

        let c = rect.center() - vec2(0.0, 40.0);
        if self.loading {
            painter.circle_filled(c, 52.0, p.accent_soft);
            let t = ui.input(|i| i.time) as f32;
            for k in 0..8 {
                let a = t * 3.0 + k as f32 * std::f32::consts::TAU / 8.0;
                let pos = c + vec2(a.cos(), a.sin()) * 24.0;
                painter.circle_filled(pos, 4.0, p.accent.gamma_multiply(k as f32 / 8.0 + 0.1));
            }
            painter.text(c + vec2(0.0, 86.0), Align2::CENTER_CENTER, "Reading the disc…", FontId::proportional(20.0), p.text);
            return;
        }
        painter.circle_filled(c, 52.0, if hover { p.accent } else { p.accent_soft });
        disc_glyph(painter, c, 30.0, if hover { Color32::WHITE } else { p.accent });
        let plus = c + vec2(38.0, 38.0);
        painter.circle_filled(plus, 15.0, p.accent);
        painter.circle_stroke(plus, 15.0, Stroke::new(3.0, p.card));
        painter.line_segment([plus - vec2(6.0, 0.0), plus + vec2(6.0, 0.0)], Stroke::new(2.5, Color32::WHITE));
        painter.line_segment([plus - vec2(0.0, 6.0), plus + vec2(0.0, 6.0)], Stroke::new(2.5, Color32::WHITE));
        painter.text(c + vec2(0.0, 88.0), Align2::CENTER_CENTER, "Load a DVD or Blu-ray to start ripping", FontId::proportional(22.0), p.text);
        painter.text(
            c + vec2(0.0, 120.0),
            Align2::CENTER_CENTER,
            "Insert a disc, click here to choose a VIDEO_TS / BDMV folder, or drop a folder or ISO image here",
            FontId::proportional(14.0),
            p.muted,
        );
        let chips = ["DVD-5 / DVD-9", "Blu-ray folders", "ISO images", "MP4 · MKV · MOV · MP3 · FLAC"];
        let font = FontId::proportional(12.5);
        let widths: Vec<f32> = chips
            .iter()
            .map(|t| painter.layout_no_wrap(t.to_string(), font.clone(), p.muted).size().x + 22.0)
            .collect();
        let total: f32 = widths.iter().sum::<f32>() + 10.0 * (chips.len() as f32 - 1.0);
        let mut x = rect.center().x - total / 2.0;
        let y = c.y + 160.0;
        for (t, w) in chips.iter().zip(widths) {
            let chip = egui::Rect::from_min_size(pos2(x, y - 13.0), vec2(w, 26.0));
            painter.rect_filled(chip, CornerRadius::same(13), p.bg);
            painter.text(chip.center(), Align2::CENTER_CENTER, *t, font.clone(), p.muted);
            x += w + 10.0;
        }
        if resp.clicked() {
            self.pick_folder(&ctx);
        }
    }

    fn title_row(&mut self, ui: &mut egui::Ui, p: &Palette, i: usize) {
        let is_main = Some(i) == self.main_title;
        let ripping = self.ripping;
        let profile: Profile = PROFILES[self.profile];
        let mut want_preview = false;
        let row = &mut self.rows[i];
        let border = if row.selected { p.accent.gamma_multiply(0.6) } else { p.border };
        Frame::new()
            .fill(p.card)
            .stroke(Stroke::new(1.0, border))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add_enabled(!ripping, egui::Checkbox::without_text(&mut row.selected));
                    // Preview frame (or a placeholder until it's read).
                    let (rect, thumb) = ui.allocate_exact_size(vec2(128.0, 72.0), Sense::click());
                    match &row.texture {
                        Some(t) => {
                            ui.painter().image(
                                t.id(),
                                rect,
                                egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
                        None => {
                            ui.painter().rect_filled(rect, CornerRadius::same(6), p.accent_soft);
                            disc_glyph(ui.painter(), rect.center(), 18.0, p.accent.gamma_multiply(0.7));
                        }
                    }
                    if let Some(d) = row.info.as_ref().and_then(|f| f.duration).or(row.title.duration) {
                        let chip = egui::Rect::from_min_size(rect.right_bottom() - vec2(58.0, 18.0), vec2(56.0, 16.0));
                        ui.painter().rect_filled(chip, CornerRadius::same(4), Color32::from_black_alpha(170));
                        ui.painter().text(chip.center(), Align2::CENTER_CENTER, disc::human_time(d), FontId::monospace(10.5), Color32::WHITE);
                    }
                    // Play button over the picture: click it to preview the title.
                    let c = rect.center() - vec2(0.0, 4.0);
                    let hot = thumb.hovered();
                    ui.painter().circle_filled(c, 15.0, if hot { p.accent } else { Color32::from_black_alpha(130) });
                    ui.painter().add(egui::Shape::convex_polygon(
                        vec![c + vec2(-4.5, -7.0), c + vec2(7.5, 0.0), c + vec2(-4.5, 7.0)],
                        Color32::WHITE,
                        Stroke::NONE,
                    ));
                    if thumb.on_hover_text("Preview this title").on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        want_preview = true;
                    }
                    ui.add_space(4.0);

                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("Title {}", row.title.number)).size(16.0).strong().color(p.text));
                            if is_main {
                                badge(ui, "MAIN MOVIE", p.accent, p.accent_soft);
                            }
                        });
                        let mut facts = vec![disc::human_size(row.title.size)];
                        if let Some(d) = row.info.as_ref().and_then(|f| f.duration).or(row.title.duration) {
                            facts.insert(0, disc::human_time(d));
                        }
                        match &row.info {
                            Some(info) => {
                                if let (Some(w), Some(h)) = (info.width, info.height) {
                                    facts.push(format!("{w}×{h}"));
                                }
                            }
                            None => facts.push("reading…".into()),
                        }
                        facts.push(format!("{} file(s)", row.title.files.len()));
                        if !row.title.label.is_empty() {
                            facts.push(row.title.label.clone());
                        }
                        ui.label(RichText::new(facts.join("   ·   ")).color(p.muted));
                        ui.horizontal(|ui| {
                            ui.add_enabled_ui(!ripping, |ui| {
                                let audio: Vec<String> = row
                                    .info
                                    .as_ref()
                                    .map(|f| f.audio.iter().enumerate().map(|(n, s)| s.describe(n + 1)).collect())
                                    .unwrap_or_default();
                                let current = audio.get(row.audio).cloned().unwrap_or_else(|| "Audio track 1".into());
                                egui::ComboBox::from_id_salt(("audio", i))
                                    .selected_text(format!("🔊 {current}"))
                                    .width(180.0)
                                    .show_ui(ui, |ui| {
                                        for (n, a) in audio.iter().enumerate() {
                                            ui.selectable_value(&mut row.audio, n, a);
                                        }
                                    });
                                let subs = row.info.as_ref().map(|f| f.subtitles.len()).unwrap_or(0);
                                let sub_text = if !profile.keeps_subs {
                                    "No subtitles (pick MKV to keep them)".to_string()
                                } else if subs == 0 {
                                    "No subtitle tracks".to_string()
                                } else if row.subtitles {
                                    format!("Keep {subs} subtitle track(s)")
                                } else {
                                    "Subtitles off".to_string()
                                };
                                if profile.keeps_subs && subs > 0 {
                                    ui.toggle_value(&mut row.subtitles, format!("💬 {sub_text}"));
                                } else {
                                    ui.label(RichText::new(format!("💬 {sub_text}")).color(p.muted));
                                }
                            });
                        });
                    });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.set_max_width(240.0);
                        ui.vertical(|ui| {
                            ui.with_layout(Layout::top_down(Align::Max), |ui| {
                                ui.horizontal(|ui| {
                                    badge(ui, &format!("{} → {}", profile.name, profile.ext.to_uppercase()), p.text, p.bg);
                                    if ui.small_button("▶ Preview").clicked() {
                                        want_preview = true;
                                    }
                                });
                                match &row.state {
                                    RowState::Ready => {
                                        ui.label(RichText::new(if row.selected { "Ready to rip" } else { "Not selected" }).color(p.muted));
                                    }
                                    RowState::Waiting => {
                                        ui.label(RichText::new("Waiting…").color(p.muted));
                                    }
                                    RowState::Ripping { fraction, speed } => {
                                        let eta = row
                                            .info
                                            .as_ref()
                                            .and_then(|f| f.duration)
                                            .filter(|_| *speed > 0.0)
                                            .map(|d| format!(" · {} left", disc::human_time(d * (1.0 - *fraction as f64) / *speed as f64)))
                                            .unwrap_or_default();
                                        ui.add(
                                            egui::ProgressBar::new(*fraction)
                                                .desired_width(200.0)
                                                .fill(p.accent)
                                                .text(format!("{:.0}%  {speed:.1}×{eta}", fraction * 100.0)),
                                        );
                                    }
                                    RowState::Done(path) => {
                                        let r = ui.label(RichText::new(format!("✔ Saved {}", file_name(path))).color(p.good));
                                        if r.on_hover_text("Show in file manager").clicked() {
                                            if let Err(e) = disc::open_location(path, true) {
                                                self.message = Some((false, format!("Couldn't show the saved file: {e}")));
                                            }
                                        }
                                    }
                                    RowState::Failed(e) => {
                                        let short: String = e.chars().take(40).collect();
                                        ui.label(RichText::new(format!("✖ {short}")).color(p.bad)).on_hover_text(e);
                                    }
                                }
                            });
                        });
                    });
                });
            });
        if want_preview {
            let ctx = ui.ctx().clone();
            self.open_preview(&ctx, i);
        }
    }

    fn open_preview(&mut self, ctx: &egui::Context, i: usize) {
        let row = &self.rows[i];
        let probed = row.info.as_ref().is_some_and(|f| f.width.is_some());
        let source = preview::Source {
            input: row.title.input(),
            locked: row.title.locked.clone().zip(row.title.playlist),
            duration: row.info.as_ref().and_then(|f| f.duration).or(row.title.duration),
            audio_track: row.audio,
            has_audio: !probed || row.info.as_ref().is_some_and(|f| !f.audio.is_empty()),
        };
        self.preview = Some(PreviewState { slot: i, source, session: None, texture: None, position: 0.0, error: None });
        self.play_from(ctx, 0.0);
    }

    /// (Re)start the preview at `at` seconds.
    fn play_from(&mut self, ctx: &egui::Context, at: f64) {
        let Some(ffmpeg) = self.ffmpeg() else {
            if let Some(pv) = &mut self.preview {
                pv.error = Some("FFmpeg wasn't found, so titles can't be previewed.".into());
            }
            return;
        };
        let muted = self.muted.clone();
        let Some(pv) = &mut self.preview else { return };
        pv.session = None; // stop the old run first, so the drive isn't read twice
        pv.position = at;
        let wake = { let ctx = ctx.clone(); move || ctx.request_repaint() };
        match preview::Session::start(&ffmpeg, &pv.source, at, pv.source.has_audio, muted, wake) {
            Ok(s) => {
                pv.session = Some(s);
                pv.error = None;
            }
            Err(e) => pv.error = Some(e),
        }
    }

    fn preview_window(&mut self, ctx: &egui::Context, p: &Palette) {
        // Bring in the newest frame and keep track of where playback is.
        let mut restart_silent = false;
        if let Some(pv) = &mut self.preview {
            if let Some(s) = &mut pv.session {
                if let Some(rgba) = s.frame.lock().unwrap().take() {
                    let img = egui::ColorImage::from_rgba_unmultiplied([preview::WIDTH, preview::HEIGHT], &rgba);
                    match &mut pv.texture {
                        Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                        None => pv.texture = Some(ctx.load_texture("preview", img, egui::TextureOptions::LINEAR)),
                    }
                }
                pv.position = s.position();
                if s.failed() {
                    if s.with_audio {
                        restart_silent = true;
                    } else {
                        pv.error = Some("This title couldn't be played.".into());
                        pv.session = None;
                    }
                } else if s.ended() {
                    pv.session = None;
                }
            }
        }
        if restart_silent {
            if let Some(pv) = &mut self.preview {
                pv.source.has_audio = false;
            }
            let at = self.preview.as_ref().map(|pv| pv.position).unwrap_or(0.0);
            self.play_from(ctx, at);
        }

        let Some(pv) = &self.preview else { return };
        let title = format!("Title {}", self.rows.get(pv.slot).map(|r| r.title.number).unwrap_or(0));
        let duration = pv.source.duration.unwrap_or(0.0);
        let playing = pv.session.is_some();
        let (mut position, error) = (pv.position, pv.error.clone());
        let tex = pv.texture.as_ref().map(|t| t.id());
        let mut action: Option<f64> = None; // play from here
        let mut pause = false;
        let mut close = false;

        let resp = egui::Modal::new(egui::Id::unique("preview-window")).show(ctx, |ui| {
            ui.set_width(preview::WIDTH as f32);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Preview · {title}")).size(18.0).strong().color(p.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
            ui.add_space(4.0);
            let (rect, screen) = ui.allocate_exact_size(vec2(preview::WIDTH as f32, preview::HEIGHT as f32), Sense::click());
            ui.painter().rect_filled(rect, CornerRadius::same(8), Color32::BLACK);
            if let Some(id) = tex {
                ui.painter().image(id, rect, egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            if let Some(e) = &error {
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, e, FontId::proportional(16.0), Color32::WHITE);
            } else if tex.is_none() {
                ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Loading…", FontId::proportional(16.0), Color32::WHITE);
            }
            if screen.clicked() {
                if playing { pause = true } else { action = Some(position) }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = if playing { "⏸  Pause" } else { "▶  Play" };
                if primary_button(ui, p, label, vec2(110.0, 34.0), true).clicked() {
                    if playing { pause = true } else { action = Some(position) }
                }
                let back = ui.button("⟲ 10 s").on_hover_text("Back 10 seconds");
                if back.clicked() {
                    action = Some((position - 10.0).max(0.0));
                }
                if ui.button("30 s ⟳").on_hover_text("Forward 30 seconds").clicked() {
                    action = Some(position + 30.0);
                }
                let muted = self.muted.load(Ordering::Relaxed);
                if ui.button(if muted { "🔇 Sound off" } else { "🔊 Sound on" }).clicked() {
                    self.muted.store(!muted, Ordering::Relaxed);
                }
                ui.label(RichText::new(format!("{} / {}", disc::human_time(position), disc::human_time(duration))).monospace().color(p.muted));
            });
            if duration > 0.0 {
                ui.spacing_mut().slider_width = preview::WIDTH as f32;
                let slider = ui.add(egui::Slider::new(&mut position, 0.0..=duration).show_value(false));
                if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                    action = Some(position);
                }
            }
        });
        if resp.should_close() {
            close = true;
        }
        if close {
            self.preview = None;
        } else if let Some(at) = action {
            self.play_from(ctx, at.min((duration - 1.0).max(0.0)));
        } else if pause {
            if let Some(pv) = &mut self.preview {
                pv.session = None;
            }
        }
    }

    fn format_picker(&mut self, ctx: &egui::Context, p: &Palette) {
        let resp = egui::Modal::new(egui::Id::unique("format-picker")).show(ctx, |ui| {
            ui.set_width(660.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Output format").size(18.0).strong().color(p.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Done").clicked() {
                        self.picker_open = false;
                    }
                });
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                for (g, label) in [(Group::Video, "Video"), (Group::Audio, "Audio"), (Group::Device, "Device")] {
                    ui.selectable_value(&mut self.picker_group, g, RichText::new(label).size(14.0));
                }
            });
            ui.separator();
            let tiles: Vec<usize> = (0..PROFILES.len()).filter(|&i| PROFILES[i].group == self.picker_group).collect();
            egui::Grid::new("tiles").spacing(vec2(10.0, 10.0)).show(ui, |ui| {
                for (n, &idx) in tiles.iter().enumerate() {
                    let pr = PROFILES[idx];
                    let on = idx == self.profile;
                    let (rect, r) = ui.allocate_exact_size(vec2(152.0, 66.0), Sense::click());
                    let fill = if on { p.accent_soft } else if r.hovered() { p.bg } else { p.card };
                    ui.painter().rect_filled(rect, CornerRadius::same(9), fill);
                    ui.painter().rect_stroke(
                        rect,
                        CornerRadius::same(9),
                        Stroke::new(if on { 2.0 } else { 1.0 }, if on { p.accent } else { p.border }),
                        StrokeKind::Inside,
                    );
                    ui.painter().text(rect.left_top() + vec2(12.0, 12.0), Align2::LEFT_TOP, pr.name, FontId::proportional(15.0), if on { p.accent } else { p.text });
                    ui.painter().text(rect.left_top() + vec2(12.0, 36.0), Align2::LEFT_TOP, pr.note, FontId::proportional(11.5), p.muted);
                    if r.clicked() {
                        self.profile = idx;
                    }
                    if n % 4 == 3 {
                        ui.end_row();
                    }
                }
            });
            ui.add_space(6.0);
            ui.separator();
            quality_controls(ui, &mut self.settings, p);
        });
        if resp.should_close() {
            self.picker_open = false;
        }
    }

    fn settings_page(&mut self, ui: &mut egui::Ui, p: &Palette) {
        egui::ScrollArea::vertical().show(ui, |ui| {
            card(ui, p, "Ripping engine", |ui| {
                ui.label(RichText::new("OpenRip uses FFmpeg, the free and open-source video toolkit, to read discs and convert them.").color(p.muted));
                ui.horizontal(|ui| {
                    ui.label("FFmpeg location:");
                    ui.add(egui::TextEdit::singleline(&mut self.ffmpeg_path).desired_width(360.0));
                    if ui.button("Choose…").clicked() {
                        if let Some(f) = rfd::FileDialog::new().pick_file() {
                            self.ffmpeg_path = f.to_string_lossy().into_owned();
                        }
                    }
                    if ui.button("Find automatically").clicked() {
                        if let Some(f) = disc::find_tool("ffmpeg") {
                            self.ffmpeg_path = f.to_string_lossy().into_owned();
                        }
                    }
                });
                if self.ffmpeg().is_some() {
                    ui.label(RichText::new("✔ FFmpeg found").color(p.good));
                } else {
                    ui.label(RichText::new("✖ Not found. Install FFmpeg with your package manager.").color(p.bad));
                }
            });
            card(ui, p, "Video quality", |ui| quality_controls(ui, &mut self.settings, p));
            card(ui, p, "When ripping finishes", |ui| {
                ui.checkbox(&mut self.open_after, "Open the output folder");
            });
            card(ui, p, "Copy-protected discs", |ui| {
                ui.label(RichText::new(
                    "Store-bought Blu-rays are encrypted (AACS). OpenRip unlocks them with the open-source libbluray and libaacs \
                     libraries plus your own key file, when they are installed. Encrypted DVDs (CSS) aren't unlocked yet. \
                     Only rip discs you own, where the law where you live allows it.",
                ).color(p.muted));
                let missing = bluray::missing();
                if missing.is_empty() {
                    ui.label(RichText::new("✔ Blu-ray unlocking is ready").color(p.good));
                } else {
                    for m in missing {
                        ui.label(RichText::new(format!("✖ Missing: {m}")).color(p.bad));
                    }
                }
            });
        });
    }

    fn about_page(&mut self, ui: &mut egui::Ui, p: &Palette) {
        card(ui, p, APP_NAME, |ui| {
            ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(p.muted));
            ui.label("A free, open-source DVD and Blu-ray ripper. Licensed MIT OR Apache-2.0.");
            ui.label("Built with Rust and egui. Ripping is done by FFmpeg (LGPL/GPL), which you install separately.");
            ui.add_space(6.0);
            ui.label(RichText::new("How to rip").strong());
            for step in [
                "1. Insert a disc, or load a VIDEO_TS / BDMV folder or an ISO image.",
                "2. Tick the titles you want; the main movie is ticked for you.",
                "3. Pick an output format with \"Rip All to\".",
                "4. Choose where to save and press Rip All.",
            ] {
                ui.label(step);
            }
        });
    }
}

fn card(ui: &mut egui::Ui, p: &Palette, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).size(16.0).strong().color(p.text));
            ui.add_space(4.0);
            add(ui);
        });
    ui.add_space(12.0);
}

fn quality_controls(ui: &mut egui::Ui, s: &mut rip::Settings, p: &Palette) {
    egui::Grid::new(ui.next_auto_id()).spacing(vec2(16.0, 8.0)).show(ui, |ui| {
        ui.label(RichText::new("Resolution").color(p.muted));
        ui.horizontal(|ui| {
            for r in Resolution::ALL {
                ui.selectable_value(&mut s.resolution, r, r.label());
            }
        });
        ui.end_row();
        ui.label(RichText::new("Quality").color(p.muted));
        ui.horizontal(|ui| {
            for q in Quality::ALL {
                ui.selectable_value(&mut s.quality, q, q.label());
            }
        });
        ui.end_row();
        // Only offered once a working hardware encoder has been found.
        if let Some(hw) = rip::known_hardware_encoder() {
            ui.label(RichText::new("Speed").color(p.muted));
            ui.checkbox(&mut s.hardware, format!("Use hardware acceleration: {} (much faster)", hw.label()));
            ui.end_row();
        }
        ui.label(RichText::new("Picture").color(p.muted));
        ui.checkbox(&mut s.deinterlace, "Remove interlacing lines (recommended for DVD)");
        ui.end_row();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rips the first title of a small test DVD, if one was made at /tmp/openrip-test.
    #[test]
    fn rips_test_dvd() {
        let root = Path::new("/tmp/openrip-test/TESTDISC");
        let Some(ffmpeg) = disc::find_tool("ffmpeg") else { return };
        if !root.exists() {
            return;
        }
        let d = disc::open(root).expect("open");
        assert_eq!(d.kind, DiscKind::Dvd);
        assert_eq!(d.titles.len(), 2);
        assert_eq!(d.titles[0].files.len(), 2, "menu VOB is skipped, parts joined");
        let info = disc::probe(&ffmpeg, &d.titles[0]);
        assert!(info.duration.unwrap_or(0.0) > 10.0, "{info:?}");
        assert!(info.thumb.is_some());
        let out_dir = std::env::temp_dir().join("openrip-out");
        std::fs::create_dir_all(&out_dir).unwrap();
        for profile in [PROFILES[0], PROFILES[2], PROFILES[8]] {
            let (tx, rx) = channel();
            let job = rip::Job {
                slot: 0,
                input: d.titles[0].input(),
                duration: info.duration,
                audio_track: Some(0),
                subtitles: true,
                output: rip::free_path(&out_dir, "Title 1", profile.ext),
                profile,
                locked: None,
            };
            let settings = rip::Settings { resolution: Resolution::P720, quality: Quality::Small, hardware: false, deinterlace: true };
            rip::start(ffmpeg.clone(), vec![job], settings, None, rip::Control::default(), tx, || {});
            let mut ok = None;
            for m in rx.iter() {
                match m {
                    rip::Msg::Finished { result, .. } => ok = Some(result),
                    rip::Msg::AllDone => break,
                    _ => {}
                }
            }
            let path = ok.unwrap().expect("rip");
            assert!(std::fs::metadata(&path).unwrap().len() > 1000, "{profile:?}");
        }
    }
}

#[cfg(test)]
mod bluray_tests {
    use super::*;

    /// With a Blu-ray in the drive, the main movie is the longest playlist and gets ticked.
    #[test]
    fn finds_main_movie_on_inserted_bluray() {
        let Some(m) = disc::mounted_discs().into_iter().find(|m| m.kind == DiscKind::BluRay) else { return };
        let d = disc::open(&m.path).expect("open");
        let main = d.main_title().expect("main");
        for t in d.titles.iter().take(6) {
            eprintln!("Title {} {} {:?} {} files {}", t.number, t.label, t.duration.map(disc::human_time), t.files.len(), disc::human_size(t.size));
        }
        eprintln!("{} titles, encrypted {}", d.titles.len(), d.encrypted);
        let t = &d.titles[main];
        eprintln!("main: Title {} {}", t.number, t.label);
        assert!(t.duration.unwrap() > 3600.0 && t.size > 10_000_000_000);
    }
}

#[cfg(test)]
mod unlock_tests {
    use super::*;

    /// Rips a short title of a protected Blu-ray in the drive, if one is there and can be unlocked.
    #[test]
    fn rips_protected_bluray_title() {
        bluray::init();
        let (Some(m), Some(ffmpeg)) = (
            disc::mounted_discs().into_iter().find(|m| m.kind == DiscKind::BluRay),
            disc::find_tool("ffmpeg"),
        ) else {
            return;
        };
        let d = disc::open(&m.path).expect("open");
        if !d.encrypted || !bluray::missing().is_empty() {
            return;
        }
        // A short title with real video in it.
        let t = d
            .titles
            .iter()
            .filter(|t| t.duration.is_some_and(|s| (20.0..150.0).contains(&s)) && t.size > 20_000_000)
            .min_by_key(|t| t.size)
            .expect("a short title");
        eprintln!("ripping Title {} {} {:?}", t.number, t.label, t.duration);
        let info = disc::probe(&ffmpeg, t);
        eprintln!("probe: {}x{:?} audio {} subs {} thumb {}", info.width.unwrap_or(0), info.height, info.audio.len(), info.subtitles.len(), info.thumb.is_some());
        assert!(info.thumb.is_some() && info.width.is_some());
        let out_dir = std::env::temp_dir().join("openrip-out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let (tx, rx) = channel();
        let job = rip::Job {
            slot: 0,
            input: t.input(),
            duration: t.duration,
            audio_track: Some(0),
            subtitles: false,
            output: rip::free_path(&out_dir, "Protected", "mp4"),
            profile: PROFILES[0],
            locked: t.locked.clone().zip(t.playlist),
        };
        let settings = rip::Settings { resolution: Resolution::P720, quality: Quality::Small, hardware: true, deinterlace: false };
        rip::start(ffmpeg.clone(), vec![job], settings, None, rip::Control::default(), tx, || {});
        let mut result = None;
        for msg in rx.iter() {
            match msg {
                rip::Msg::Finished { result: r, .. } => result = Some(r),
                rip::Msg::AllDone => break,
                _ => {}
            }
        }
        let path = result.unwrap().expect("rip");
        let out = std::process::Command::new(disc::ffprobe_for(&ffmpeg).unwrap())
            .args(["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0"])
            .arg(&path)
            .output()
            .unwrap();
        let secs: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
        eprintln!("saved {} ({secs:.1} s)", path.display());
        assert!((secs - t.duration.unwrap()).abs() < 5.0);
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn default_output_dir_is_app_folder_under_videos() {
        let dir = default_output_dir();
        assert_eq!(dir.file_name().unwrap(), APP_NAME);
        let parent = dir.parent().unwrap().file_name().unwrap().to_string_lossy().into_owned();
        if cfg!(target_os = "macos") {
            assert_eq!(parent, "Movies");
        } else {
            assert_eq!(parent, "Videos");
        }
    }

    #[test]
    fn file_name_returns_last_component() {
        assert_eq!(file_name(Path::new("/a/b/movie.mkv")), "movie.mkv");
        assert_eq!(file_name(Path::new("movie.mkv")), "movie.mkv");
        assert_eq!(file_name(Path::new("/a/b/")), "b");
    }

    #[test]
    fn file_name_empty_for_root() {
        assert_eq!(file_name(Path::new("/")), "");
        assert_eq!(file_name(Path::new("")), "");
    }

    #[test]
    fn palettes_differ_between_light_and_dark() {
        let (light, dark) = (palette(false), palette(true));
        assert_ne!(light.bg, dark.bg);
        assert_ne!(light.text, dark.text);
        assert_eq!(light.accent, ACCENT);
        assert_ne!(dark.accent, ACCENT);
    }

    #[test]
    fn palette_text_contrasts_with_background() {
        fn luma(c: Color32) -> f32 {
            0.2126 * c.r() as f32 + 0.7152 * c.g() as f32 + 0.0722 * c.b() as f32
        }
        let light = palette(false);
        assert!(luma(light.text) < luma(light.bg));
        let dark = palette(true);
        assert!(luma(dark.text) > luma(dark.bg));
    }
}
