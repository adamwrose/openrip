# OpenRip

An open-source DVD and Blu-ray ripper with a friendly interface, powered by FFmpeg.

## Requirements

- [FFmpeg](https://ffmpeg.org/) (`ffmpeg` and `ffprobe`) installed and on your `PATH`
- Rust (2024 edition) to build from source: `cargo run --release`
- On Linux, `udisks2` to open ISO images

## Installing on Linux

Download `openrip-linux-x86_64.tar.gz` from the releases page, unpack it and run
`./install.sh` (installs to `~/.local`; use `sudo ./install.sh` for `/usr/local`).
`./uninstall.sh` removes it. You still need FFmpeg from your package manager.

Or install the Flatpak, which bundles FFmpeg, libbluray and libaacs:

```
flatpak install --user openrip-linux-x86_64.flatpak
flatpak run io.github.adamwrose.OpenRip
```

To build the Flatpak yourself, run `packaging/flatpak/build.sh` (needs `flatpak-builder`).
Inside the Flatpak, mount `.iso` images yourself and open the mounted folder; the app can't
mount them from the sandbox. Put `keydb.cfg` in `~/.config/aacs/` as usual.

## Command line

Run `openrip` with no arguments to open the window, or use it from a terminal:

```
openrip list <disc>                    # show the titles, main movie marked with *
openrip list <disc> --details          # also probe picture size, audio and subtitle tracks
openrip rip <disc>                     # rip the main movie as MP4
openrip rip <disc> --title 1,3 --profile mkv-hevc --out ~/Videos --quality small
openrip rip <disc> --all --merge       # every title, plus one joined file
openrip profiles                       # the output formats
```

`<disc>` is a mounted disc, a `VIDEO_TS` or `BDMV` folder, or an `.iso` file. Other options:
`--resolution`, `--audio N`, `--subtitles`, `--no-deinterlace`, `--no-hardware`,
`--ffmpeg PATH`. See `openrip --help`. Saved file paths go to stdout and progress to stderr;
the exit status is 0 on success, 1 if a title failed, 2 for a bad command line.

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
- **Windows:** put `libbluray-2.dll` and `libaacs.dll` somewhere on your `PATH`

## Legal

Only rip discs you own, and check the laws in your country about circumventing copy
protection before using a key file.

## License

MIT OR Apache-2.0
