# OpenRip

An open-source DVD and Blu-ray ripper with a friendly interface, powered by FFmpeg.

## Requirements

- [FFmpeg](https://ffmpeg.org/) (`ffmpeg` and `ffprobe`) installed and on your `PATH`
- Rust (2024 edition) to build from source: `cargo run --release`
- On Linux, `udisks2` to open ISO images

## Hardware acceleration

On start-up OpenRip test-encodes with each hardware encoder FFmpeg offers and uses the
first that works, falling back to software (x264/x265) if none do. If a hardware encode
fails part-way, that title is retried in software.

| Platform | Tried, in order |
|----------|-----------------|
| macOS    | VideoToolbox |
| Linux    | NVIDIA NVENC, VAAPI (AMD and Intel, each `/dev/dri/renderD*`), Intel Quick Sync |
| Windows  | NVIDIA NVENC, AMD AMF, Intel Quick Sync |

Your FFmpeg build must include the encoder, and the matching graphics driver must be
installed. The settings page shows which one was found. Only H.264 and HEVC output is
hardware-encoded.

## Copy-protected Blu-rays

Most commercial Blu-rays are protected with AACS. OpenRip can read them through
[libbluray](https://www.videolan.org/developers/libbluray.html) and
[libaacs](https://www.videolan.org/developers/libaacs.html), but only if all three of
these are in place:

1. **libbluray** installed
2. **libaacs** installed
3. **A `keydb.cfg` key file, which you must download yourself from online.**
   OpenRip does not include or provide one. Community-maintained key databases are
   available online; search for "keydb.cfg" and pick one that includes your disc.

Without these, OpenRip still lists the titles on a protected disc, but ripping and
previewing fail with a message saying what is missing. If the key file doesn't contain a
key for your particular disc, you'll see "your key file has no key for it".

### Where to put `keydb.cfg`

| Platform | Location |
|----------|----------|
| Linux    | `~/.config/aacs/KEYDB.cfg` (libaacs only reads the **uppercase** name) |
| macOS    | `~/Library/Preferences/aacs/keydb.cfg` or `~/.config/aacs/keydb.cfg` |
| Windows  | `%APPDATA%\aacs\keydb.cfg` |

On Linux, if you saved the file as `keydb.cfg`, either rename it or add a symlink:

```
mkdir -p ~/.config/aacs
ln -s keydb.cfg ~/.config/aacs/KEYDB.cfg
```

### Installing the libraries

- **Arch:** `sudo pacman -S libbluray libaacs`
- **Debian/Ubuntu:** `sudo apt install libbluray2 libaacs0`
- **macOS (Homebrew):** `brew install libbluray libaacs`
- **Windows:** put `libbluray-2.dll` and `libaacs-0.dll` somewhere on your `PATH`

## Legal

Only rip discs you own, and check the laws in your country about circumventing copy
protection before using a key file.

## License

MIT OR Apache-2.0
