//! Reading copy-protected Blu-rays through libbluray and libaacs (both open source), when
//! they are installed together with the person's own key
//! file (keydb.cfg). The libraries are loaded at run time, so the app works without them.

use libloading::{Library, Symbol};
use std::ffi::{CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[cfg(target_os = "macos")]
const BLURAY_LIBS: &[&str] = &["libbluray.dylib", "libbluray.4.dylib", "libbluray.2.dylib"];
#[cfg(target_os = "linux")]
const BLURAY_LIBS: &[&str] = &["libbluray.so.4", "libbluray.so.2", "libbluray.so"];
#[cfg(target_os = "windows")]
const BLURAY_LIBS: &[&str] = &["libbluray-4.dll", "libbluray-2.dll", "libbluray.dll"];
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
const BLURAY_LIBS: &[&str] = &[];

#[cfg(target_os = "macos")]
const AACS_LIBS: &[&str] = &["libaacs.0.dylib", "libaacs.dylib"];
#[cfg(target_os = "linux")]
const AACS_LIBS: &[&str] = &["libaacs.so.0", "libaacs.so"];
#[cfg(target_os = "windows")]
const AACS_LIBS: &[&str] = &["libaacs-0.dll", "libaacs.dll"];
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
const AACS_LIBS: &[&str] = &[];

#[cfg(target_os = "macos")]
const BDPLUS_LIBS: &[&str] = &["libbdplus.0.dylib", "libbdplus.dylib"];

fn find_lib(files: &[&str]) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    #[cfg(target_os = "macos")]
    dirs.extend(["/opt/homebrew/lib", "/usr/local/lib", "/opt/local/lib"].map(PathBuf::from));
    #[cfg(target_os = "linux")]
    dirs.extend([
        "/usr/local/lib",
        "/usr/local/lib64",
        "/lib",
        "/lib64",
        "/lib/x86_64-linux-gnu",
        "/lib/aarch64-linux-gnu",
        "/usr/lib",
        "/usr/lib64",
        "/usr/lib/x86_64-linux-gnu",
        "/usr/lib/aarch64-linux-gnu",
    ].map(PathBuf::from));
    #[cfg(target_os = "windows")]
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.to_path_buf());
        }
    }
    dirs.iter()
        .flat_map(|d| files.iter().map(move |f| d.join(f)))
        .find(|p| p.is_file())
}

/// Where libaacs looks for the key file.
fn keydb_paths() -> Vec<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let Some(home) = home else { return Vec::new() };
    #[cfg(target_os = "macos")]
    {
        vec![
            home.join("Library/Preferences/aacs/keydb.cfg"),
            home.join("Library/Preferences/aacs/KEYDB.cfg"),
            home.join(".config/aacs/keydb.cfg"),
            home.join(".config/aacs/KEYDB.cfg"),
        ]
    }
    #[cfg(target_os = "linux")]
    {
        vec![home.join(".config/aacs/keydb.cfg"), home.join(".config/aacs/KEYDB.cfg")]
    }
    #[cfg(windows)]
    {
        vec![home.join("aacs/keydb.cfg"), home.join("aacs/KEYDB.cfg")]
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Vec::new()
    }
}

/// Tell libbluray where libaacs and libbdplus are. Call once at start, before any threads.
pub fn init() {
    #[cfg(target_os = "macos")]
    for (var, files) in [("LIBAACS_PATH", AACS_LIBS), ("LIBBDPLUS_PATH", BDPLUS_LIBS)] {
        if std::env::var_os(var).is_none() {
            if let Some(p) = find_lib(files) {
                let base = p.to_string_lossy().trim_end_matches(".dylib").trim_end_matches(".0").to_string();
                // SAFETY: called from main before any other thread starts.
                unsafe { std::env::set_var(var, base) };
            }
        }
    }
}

/// What's missing to unlock protected Blu-rays (empty when everything is in place).
pub fn missing() -> Vec<&'static str> {
    let mut m = Vec::new();
    if find_lib(BLURAY_LIBS).is_none() {
        m.push("libbluray");
    }
    if find_lib(AACS_LIBS).is_none() {
        m.push("libaacs");
    }
    if !keydb_paths().iter().any(|p| p.is_file()) {
        m.push("a key file, keydb.cfg, in the platform's AACS config folder");
    }
    m
}

fn library() -> Result<&'static Library, String> {
    static LIB: OnceLock<Result<Library, String>> = OnceLock::new();
    LIB.get_or_init(|| {
        let path = find_lib(BLURAY_LIBS).ok_or("libbluray isn't installed.")?;
        // SAFETY: loading a well-known C library; its initialisers have no preconditions.
        unsafe { Library::new(&path) }.map_err(|e| format!("Couldn't load libbluray: {e}"))
    })
    .as_ref()
    .map_err(Clone::clone)
}

type OpenFn = unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_void;
type CloseFn = unsafe extern "C" fn(*mut c_void);
type TitlesFn = unsafe extern "C" fn(*mut c_void, u8, u32) -> u32;
type SelectFn = unsafe extern "C" fn(*mut c_void, u32) -> c_int;
type SizeFn = unsafe extern "C" fn(*mut c_void) -> u64;
type SeekFn = unsafe extern "C" fn(*mut c_void, u64) -> i64;
type ReadFn = unsafe extern "C" fn(*mut c_void, *mut u8, c_int) -> c_int;

/// One playlist of a Blu-ray, read already unlocked, as an MPEG transport stream.
pub struct Reader {
    bd: *mut c_void,
    close: CloseFn,
    seek: SeekFn,
    read: ReadFn,
    size: u64,
}

// The handle is only ever used by the thread that owns the Reader.
unsafe impl Send for Reader {}

impl Reader {
    pub fn open(root: &Path, playlist: u32) -> Result<Reader, String> {
        let lib = library()?;
        let sym = |e: libloading::Error| format!("libbluray is too old: {e}");
        // SAFETY: the signatures match libbluray's public API (bluray.h).
        unsafe {
            let open: Symbol<OpenFn> = lib.get(b"bd_open\0").map_err(sym)?;
            let close: Symbol<CloseFn> = lib.get(b"bd_close\0").map_err(sym)?;
            let titles: Symbol<TitlesFn> = lib.get(b"bd_get_titles\0").map_err(sym)?;
            let select: Symbol<SelectFn> = lib.get(b"bd_select_playlist\0").map_err(sym)?;
            let size: Symbol<SizeFn> = lib.get(b"bd_get_title_size\0").map_err(sym)?;
            let seek: Symbol<SeekFn> = lib.get(b"bd_seek\0").map_err(sym)?;
            let read: Symbol<ReadFn> = lib.get(b"bd_read\0").map_err(sym)?;
            let dev = CString::new(root.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
            let bd = open(dev.as_ptr(), std::ptr::null());
            if bd.is_null() {
                return Err("libbluray couldn't open the disc.".into());
            }
            let mut r = Reader { bd, close: *close, seek: *seek, read: *read, size: 0 };
            titles(bd, 0, 0);
            if select(bd, playlist) != 1 {
                return Err(format!("The disc's playlist {playlist:05} couldn't be opened."));
            }
            r.size = size(bd);
            r.check_unlocked()?;
            Ok(r)
        }
    }

    /// Unlocked data has a transport-stream sync byte at the same place in every packet.
    fn check_unlocked(&mut self) -> Result<(), String> {
        let mut buf = vec![0u8; 6144];
        let n = self.read(&mut buf)?;
        self.seek(0);
        let ok = n >= 6144 && (0..32).all(|k| buf[4 + k * 192] == 0x47);
        if ok {
            Ok(())
        } else {
            let gaps = missing();
            Err(if gaps.is_empty() {
                "Couldn't unlock this disc: your key file has no key for it.".into()
            } else {
                format!("Couldn't unlock this disc. Missing: {}.", gaps.join(", "))
            })
        }
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn seek(&mut self, pos: u64) {
        // SAFETY: self.bd is a live handle from bd_open.
        unsafe { (self.seek)(self.bd, pos - pos % 6144) };
    }

    /// Fill `buf` with the next bytes; 0 means the end of the playlist.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let len = buf.len().min(i32::MAX as usize) as c_int;
        // SAFETY: buf is valid for len bytes and self.bd is live.
        let n = unsafe { (self.read)(self.bd, buf.as_mut_ptr(), len) };
        if n < 0 { Err("Reading the disc failed.".into()) } else { Ok(n as usize) }
    }

    /// `bytes` of unlocked data starting at `fraction` of the way in.
    pub fn sample(&mut self, fraction: f64, bytes: usize) -> Result<Vec<u8>, String> {
        self.seek((self.size as f64 * fraction) as u64);
        let mut out = vec![0u8; bytes];
        let mut filled = 0;
        while filled < bytes {
            let n = self.read(&mut out[filled..])?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        out.truncate(filled);
        Ok(out)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        // SAFETY: closed exactly once.
        unsafe { (self.close)(self.bd) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_lib_returns_none_for_unknown_library() {
        assert_eq!(find_lib(&["libopenrip-no-such-library.so.99"]), None);
        assert_eq!(find_lib(&[]), None);
    }

    #[test]
    fn libbluray_names_include_current_and_older_versions() {
        // libbluray 1.4 is soname 4 (MSYS2's libbluray-4.dll, Arch's libbluray.so.4);
        // older releases are 2. Both, and the unversioned name, must be tried.
        let has = |needle: &str| BLURAY_LIBS.iter().any(|n| n.contains(needle));
        if !BLURAY_LIBS.is_empty() {
            assert!(has("4") && has("2"), "{BLURAY_LIBS:?}");
        }
    }

    #[test]
    fn library_name_tables_are_not_empty_on_supported_platforms() {
        if cfg!(any(target_os = "macos", target_os = "linux", target_os = "windows")) {
            assert!(!BLURAY_LIBS.is_empty());
            assert!(!AACS_LIBS.is_empty());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn keydb_paths_point_into_aacs_config_folder() {
        for p in keydb_paths() {
            assert!(p.ends_with(".config/aacs/keydb.cfg") || p.ends_with(".config/aacs/KEYDB.cfg"), "{p:?}");
        }
    }

    #[test]
    fn missing_only_reports_known_gaps() {
        for item in missing() {
            assert!(item == "libbluray" || item == "libaacs" || item.starts_with("a key file"), "{item}");
        }
    }

    #[test]
    fn missing_agrees_with_library_lookup() {
        let gaps = missing();
        assert_eq!(gaps.contains(&"libbluray"), find_lib(BLURAY_LIBS).is_none());
        assert_eq!(gaps.contains(&"libaacs"), find_lib(AACS_LIBS).is_none());
    }

    #[test]
    fn init_is_harmless() {
        init();
    }

    #[test]
    fn reader_open_fails_for_a_folder_that_is_not_a_disc() {
        let dir = std::env::temp_dir().join(format!("openrip-bd-notdisc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Either libbluray is absent, or it can't open/unlock an empty folder.
        let err = Reader::open(&dir, 1).err().expect("should fail");
        assert!(!err.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reader_open_rejects_paths_with_nul() {
        let err = Reader::open(Path::new("bad\0path"), 1).err().expect("should fail");
        assert!(!err.is_empty());
    }
}
