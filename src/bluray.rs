//! Reading copy-protected Blu-rays through libbluray and libaacs (both open source), when
//! they are installed (Homebrew: libbluray, libaacs) together with the person's own key
//! file (keydb.cfg). The libraries are loaded at run time, so the app works without them.

use libloading::{Library, Symbol};
use std::ffi::{CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const LIB_DIRS: [&str; 3] = ["/opt/homebrew/lib", "/usr/local/lib", "/opt/local/lib"];

fn find_lib(file: &str) -> Option<PathBuf> {
    LIB_DIRS.iter().map(|d| Path::new(d).join(file)).find(|p| p.exists())
}

/// Where libaacs looks for the key file.
fn keydb_paths() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return Vec::new() };
    vec![
        home.join("Library/Preferences/aacs/keydb.cfg"),
        home.join("Library/Preferences/aacs/KEYDB.cfg"),
        home.join(".config/aacs/keydb.cfg"),
        home.join(".config/aacs/KEYDB.cfg"),
    ]
}

/// Tell libbluray where libaacs and libbdplus are. Call once at start, before any threads.
pub fn init() {
    // libbluray adds ".<version>.dylib" itself, so give the path without it.
    for (var, file) in [("LIBAACS_PATH", "libaacs.0.dylib"), ("LIBBDPLUS_PATH", "libbdplus.0.dylib")] {
        if std::env::var_os(var).is_none() {
            if let Some(p) = find_lib(file) {
                let base = p.to_string_lossy().trim_end_matches(".0.dylib").to_string();
                // SAFETY: called from main before any other thread starts.
                unsafe { std::env::set_var(var, base) };
            }
        }
    }
}

/// What's missing to unlock protected Blu-rays (empty when everything is in place).
pub fn missing() -> Vec<&'static str> {
    let mut m = Vec::new();
    if find_lib("libbluray.dylib").is_none() && find_lib("libbluray.2.dylib").is_none() {
        m.push("libbluray (brew install libbluray)");
    }
    if find_lib("libaacs.0.dylib").is_none() {
        m.push("libaacs (brew install libaacs)");
    }
    if !keydb_paths().iter().any(|p| p.is_file()) {
        m.push("a key file, keydb.cfg, in ~/Library/Preferences/aacs/");
    }
    m
}

fn library() -> Result<&'static Library, String> {
    static LIB: OnceLock<Result<Library, String>> = OnceLock::new();
    LIB.get_or_init(|| {
        let path = ["libbluray.dylib", "libbluray.2.dylib"]
            .iter()
            .find_map(|f| find_lib(f))
            .ok_or("libbluray isn't installed (brew install libbluray).")?;
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
