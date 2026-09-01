# Audio Visualizer GUI

A **Rust/egui desktop application for VIA/Vial-compatible keyboards**, combining per-key RGB control with a real-time audio-reactive visualizer.

The application communicates directly with the keyboard over USB HID and can drive RGB effects from:

* 🎵 system audio / frequency bands
* 🖼️ desktop wallpaper colors
* 🎨 per-key color configuration
* 🌈 color palettes
* 🖥️ screen-based color sampling
* 🌄 wallpaper state

The project is a Rust port of the `viz_gui.py` interface from `viz-frame-tools`.

---

<details>
<summary>Preview</summary>
<img width="666" height="938" alt="grafik" src="https://github.com/user-attachments/assets/f0138e0e-260f-4282-a9fe-83fb7191b395" />
</details>

---
## Features

* **VIA/Vial keyboard support**
* **Per-key RGB color configuration**
* **Audio-reactive RGB visualization**
* **FFT / frequency-band processing**
* **Audio peak detection**
* **Wallpaper color extraction**
* **Color-Shot / screen color sampling**
* **Ambient screen visualization**
* **WPWatch wallpaper monitoring**
* **Direct USB HID communication**
* **Rust + egui desktop GUI**
* **Linux support**
* **AppImage packaging**

The actual RGB animation is rendered by the keyboard firmware. The desktop application acts as the host-side controller and data source.
---

# Linux Setup

Several features require additional Linux configuration or system packages.

## USB HID permissions

Linux normally restricts access to `/dev/hidraw*` to root.

Without a udev rule, connecting to the keyboard may fail with an error similar to:

```text
connect failed: hidapi error:
Failed to open a device with path '/dev/hidraw5':
Permission denied
```

Install the included udev rule:

```bash
sudo cp packaging/70-audio-visualizer-gui-hidraw.rules \
    /etc/udev/rules.d/
```

Reload the rules:

```bash
sudo udevadm control --reload-rules
sudo udevadm trigger
```

Then **unplug and reconnect the keyboard**.

A reboot is not required.

The application also reports a corresponding hint when it encounters a permission-denied HID connection failure.

---

## Audio Capture

Audio capture uses the PulseAudio-compatible command-line tools:

* `pactl`
* `parec`

This works with systems using PulseAudio as well as PipeWire's PulseAudio compatibility layer.

If the application reports:

```text
audio sources unavailable: ...
```

check that:

1. `pactl` and `parec` are installed and available in `PATH`
2. a PulseAudio-compatible socket is available
3. your audio server is running

### Fedora

Fedora uses PipeWire by default, but the PulseAudio-compatible command-line utilities are provided separately.

Install them with:

```bash
sudo dnf install pulseaudio-utils
```

On Debian/Ubuntu-based systems, the required compatibility packages may already be installed depending on the audio setup.

---

# Hyprland Features

Some visual features use screen capture and are currently implemented through Hyprland-specific tools.

The following features require **Hyprland**:

* Color-Shot
* Ambient
* monitor picker

They use:

* `hyprctl`
* `grim`

Install the required tools according to your distribution.

On other Wayland compositors the application will report:

```text
monitor picker needs Hyprland
```

These features are therefore currently **Hyprland-specific**.
---

### Wallpaper detection (WPWatch)

Auto-detected at startup, first match wins, override via the ☰ menu:

1. Caelestia -- `~/.local/state/caelestia/wallpaper/path.txt`
2. Omarchy 4 -- `~/.local/state/omarchy/current/background`
3. Omarchy 3 -- `~/.config/omarchy/current/background` (*untested)
4. hyprpaper -- `~/.config/hypr/hyprpaper.conf` (*untested)
5. KDE Plasma -- `~/.config/plasma-org.kde.plasma.desktop-appletsrc` (*untested)

The wallpaper source can also be overridden manually from the application's menu.

> Some wallpaper integrations are currently untested.

### Current limitations

The following wallpaper systems are currently not supported:

* GNOME — wallpaper state is stored in dconf rather than a directly readable file
* COSMIC

---

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
