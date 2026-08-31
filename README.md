# Audiovisualizer (audio-visualizer-gui)

Rust/egui GUI for controlling VIA/Vial keyboard RGB (per-key colors + a
Python-viz-style audio visualizer relay) and driving it from audio bands
or the desktop wallpaper. Rust port of `viz-frame-tools`' `viz_gui.py`.

---

<details>
<summary>Vorschau</summary>
<img width="666" height="938" alt="grafik" src="https://github.com/user-attachments/assets/f0138e0e-260f-4282-a9fe-83fb7191b395" />
</details>

---

## Linux setup

### hidraw permission (all distros)

`/dev/hidraw*` is root-only by default. Without a udev rule, Connect fails
with:

```
connect failed: hidapi error: Failed to open a device with path '/dev/hidraw5': Permission denied
```

Fix:

```bash
sudo cp packaging/70-audio-visualizer-gui-hidraw.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Then replug the keyboard (no reboot needed). The app also prints this hint
itself when it hits a permission-denied connect failure.

### Audio bands / Color-Shot / WPWatch / Ambient

Audio capture uses `pactl`/`parec` (PulseAudio/PipeWire) as subprocesses --
if the status bar shows `audio sources unavailable: ...`, those binaries
aren't on `PATH`, or nothing on the system provides a PulseAudio-compatible
socket.

**Fedora**: ships PipeWire by default, but not its PulseAudio-compat CLI --
that's the separate `pulseaudio-utils` package (unlike Debian/Ubuntu, where
`pipewire-pulse` pulls it in automatically):

```bash
sudo dnf install pulseaudio-utils
```

Screen-capture features (Color-Shot, Ambient, and the monitor picker) shell
out to `hyprctl`/`grim` and only work under Hyprland. On other compositors
the status bar will say `monitor picker needs Hyprland` -- WPWatch (reading
the wallpaper file itself) doesn't need Hyprland and works anywhere the
wallpaper-state path can be found (see below).

### Wallpaper detection (WPWatch)

Auto-detected at startup, first match wins, override via the ☰ menu:

1. Caelestia -- `~/.local/state/caelestia/wallpaper/path.txt`
2. Omarchy 4 -- `~/.local/state/omarchy/current/background`
3. Omarchy 3 -- `~/.config/omarchy/current/background`
4. hyprpaper -- `~/.config/hypr/hyprpaper.conf`
5. KDE Plasma -- `~/.config/plasma-org.kde.plasma.desktop-appletsrc`

GNOME (dconf, no plain file) and COSMIC aren't supported yet.

## Building

```bash
cargo build --release
```

### Portable AppImage (old-glibc build)

The AppImage should run on distros far older than the build machine.
Building with the host toolchain links against the host's glibc symbol
versions (e.g. a rolling-release box on glibc 2.44 produces a binary that
refuses to start anywhere older: `version 'GLIBC_2.44' not found`).

Build against an older glibc target with
[`cargo-zigbuild`](https://github.com/rust-cross/cargo-zigbuild) instead
(needs `zig` + `cargo install cargo-zigbuild`):

```bash
CFLAGS_x86_64_unknown_linux_gnu="-I/usr/include" \
  cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.17
cp target/x86_64-unknown-linux-gnu/release/audio-visualizer-gui AppDir/usr/bin/audio-visualizer-gui
ARCH=x86_64 appimagetool AppDir audio-visualizer-gui-x86_64.AppImage
```

The `CFLAGS` include path is needed because `hidapi`'s Linux backend
compiles a C file that includes `libudev.h`, which zig's bundled sysroot
doesn't ship -- pointing it at the host's `/usr/include` is enough (only
the header is needed at compile time; `libudev.so.1` itself is resolved
dynamically at runtime on the target machine, so its exact version doesn't
matter).

Sanity-check the result before shipping it:

```bash
objdump -T target/x86_64-unknown-linux-gnu/release/audio-visualizer-gui \
  | grep -oP 'GLIBC_\K[0-9.]+' | sort -V | tail -1   # should be 2.17
```

If it fails to *launch* on an old target (as opposed to refusing to start),
that's likely libfuse2 -- many current distros no longer ship it, which
AppImage's self-mounting needs. Workaround: run with
`--appimage-extract-and-run`.
