// Screen-capture palette sources -- port of send_palette_from_image.py,
// send_palette_from_screen.py, ambient_screen.py (viz-frame-tools).
//
// Hyprland/Wayland-specific (grim for screenshots, hyprctl for monitor
// geometry, Caelestia's wallpaper state file) -- same scope note as the
// Python originals: other desktop environments need their own capture/
// wallpaper backend.
use crate::render::{Palette, N_LEVELS};
use image::RgbImage;
use std::path::PathBuf;
use std::process::Command;

const CAELESTIA_WALLPAPER_PATH: &str = ".local/state/caelestia/wallpaper/path.txt";
// Omarchy 4 ("quattro"): no path.txt -- this is a symlink straight to the
// current wallpaper image, recreated on every wallpaper change.
const OMARCHY_WALLPAPER_PATH: &str = ".local/state/omarchy/current/background";
// Omarchy 3: same symlink-to-image scheme, just under ~/.config instead of
// ~/.local/state.
const OMARCHY3_WALLPAPER_PATH: &str = ".config/omarchy/current/background";
// hyprpaper: no state file at all, just its static config -- `wallpaper =
// <monitor>,<path>` lines. Least reliable signal of the three (the file
// existing doesn't mean hyprpaper is the active wallpaper daemon), so it's
// checked last in detect_wallpaper_state_file_path().
const HYPRPAPER_CONF_PATH: &str = ".config/hypr/hyprpaper.conf";
// KDE Plasma: one `Image=file://<path>` line per wallpaper-plugin containment
// (i.e. per virtual desktop/monitor) inside this INI file. First match wins,
// same "good enough for single-wallpaper setups" tradeoff as hyprpaper.
const KDE_PLASMA_CONF_PATH: &str = ".config/plasma-org.kde.plasma.desktop-appletsrc";

fn quantize(v: u8, step: u8) -> u8 {
    (v / step) * step
}

/// brightness=1.35 is a fixed internal constant in the Python original --
/// only `gamma` is user-adjustable (the GUI's Gamma slider).
fn correct_for_keyboard_leds((r, g, b): (u8, u8, u8), gamma: f32) -> (u8, u8, u8) {
    const BRIGHTNESS: f32 = 1.35;
    let c = |v: u8| -> u8 {
        let v = ((v as f32 / 255.0).powf(gamma)) * 255.0;
        ((v * BRIGHTNESS).min(255.0)) as u8
    };
    (c(r), c(g), c(b))
}

fn saturation((r, g, b): (u8, u8, u8)) -> i32 {
    let (r, g, b) = (r as i32, g as i32, b as i32);
    r.max(g).max(b) - r.min(g).min(b)
}
fn brightness((r, g, b): (u8, u8, u8)) -> f32 {
    (r as f32 + g as f32 + b as f32) / 3.0
}
fn dist((r1, g1, b1): (u8, u8, u8), (r2, g2, b2): (u8, u8, u8)) -> f32 {
    let d = |a: u8, b: u8| (a as f32 - b as f32).powi(2);
    (d(r1, r2) + d(g1, g2) + d(b1, b2)).sqrt()
}

pub fn palette_distance(a: &Palette, b: &Palette) -> f32 {
    let mut sum = 0.0;
    for i in 0..N_LEVELS {
        let (r1, g1, b1) = a[i];
        let (r2, g2, b2) = b[i];
        sum += ((r1 as f32 - r2 as f32).abs()
            + (g1 as f32 - g2 as f32).abs()
            + (b1 as f32 - b2 as f32).abs())
            / 3.0;
    }
    sum / N_LEVELS as f32
}

// ── Wallpaper (Caelestia) ────────────────────────────────────────────────

/// Auto-picks a wallpaper-state location at startup: first candidate that
/// exists on disk wins (Caelestia checked first since it's the primary
/// target), falling back to the Caelestia path so there's always a sane
/// default to show/edit even if neither exists yet. Add more desktop
/// environments' state locations here as they come up.
pub fn detect_wallpaper_state_file_path() -> PathBuf {
    let candidates = [
        dirs_home().join(CAELESTIA_WALLPAPER_PATH),
        dirs_home().join(OMARCHY_WALLPAPER_PATH),
        dirs_home().join(OMARCHY3_WALLPAPER_PATH),
        dirs_home().join(HYPRPAPER_CONF_PATH),
        dirs_home().join(KDE_PLASMA_CONF_PATH),
    ];
    candidates.iter().find(|p| p.exists()).cloned().unwrap_or_else(|| candidates[0].clone())
}

/// `~` / `~/...` expansion for the user-editable override in the GUI --
/// state file itself is always an absolute path once resolved.
pub fn expand_home(input: &str) -> PathBuf {
    let input = input.trim();
    match input.strip_prefix("~/") {
        Some(rest) => dirs_home().join(rest),
        None if input == "~" => dirs_home(),
        None => PathBuf::from(input),
    }
}

fn strip_file_uri(v: &str) -> &str {
    v.strip_prefix("file://").unwrap_or(v)
}

/// Resolves a wallpaper-state location to the actual image path. Handles
/// all known shapes without needing to be told which one it is:
/// - Caelestia: the file's entire (trimmed) content *is* the image path.
/// - hyprpaper.conf: scan for a `wallpaper = <monitor>,<path>` line.
/// - KDE Plasma's appletsrc: scan for an `Image=file://<path>` line.
/// - Omarchy: a symlink (or plain file) that already *is* the image, so
///   falling through to using the state file itself covers it.
pub fn current_wallpaper_path(state_file: &std::path::Path) -> Result<PathBuf, String> {
    if let Ok(text) = std::fs::read_to_string(state_file) {
        let trimmed = text.trim();
        let pointed = PathBuf::from(trimmed);
        if !trimmed.is_empty() && pointed.exists() {
            return Ok(pointed);
        }

        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();

            if let Some(rest) = line.strip_prefix("wallpaper").and_then(|r| r.trim_start().strip_prefix('=')) {
                let path_part = rest.rsplit(',').next().unwrap_or("").trim();
                let path = expand_home(strip_file_uri(path_part));
                if !path_part.is_empty() && path.exists() {
                    return Ok(path);
                }
            }

            if let Some(rest) = line.strip_prefix("Image=") {
                let rest = rest.trim();
                let path = expand_home(strip_file_uri(rest));
                if !rest.is_empty() && path.exists() {
                    return Ok(path);
                }
            }
        }
    }
    if state_file.exists() {
        return Ok(state_file.to_path_buf());
    }
    Err(format!("wallpaper state file not found: {}", state_file.display()))
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/"))
}

/// `image::open()` guesses format from the file *extension* -- breaks for
/// extensionless wallpaper symlinks (Omarchy's `current/background` has no
/// `.jpg`/`.png` suffix even though it resolves to one). Guess from the
/// actual file content instead.
pub fn load_image_rgb(path: &std::path::Path) -> Result<RgbImage, String> {
    image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map(|img| img.to_rgb8())
        .map_err(|e| e.to_string())
}

// ── grim capture ─────────────────────────────────────────────────────────

fn get_active_monitor_geometry() -> Result<String, String> {
    let active: serde_json::Value = run_json(&["hyprctl", "activewindow", "-j"])?;
    let monitor_id = active["monitor"].as_i64().ok_or("no active monitor")?;
    let monitors: serde_json::Value = run_json(&["hyprctl", "monitors", "-j"])?;
    let m = monitors
        .as_array()
        .and_then(|arr| arr.iter().find(|m| m["id"].as_i64() == Some(monitor_id)))
        .ok_or("monitor not found")?;
    Ok(format!(
        "{},{} {}x{}",
        m["x"], m["y"], m["width"], m["height"]
    ))
}

fn run_json(cmd: &[&str]) -> Result<serde_json::Value, String> {
    let out = Command::new(cmd[0]).args(&cmd[1..]).output().map_err(|e| e.to_string())?;
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

pub fn grab_active_monitor() -> Result<RgbImage, String> {
    let geometry = get_active_monitor_geometry()?;
    let out = Command::new("grim")
        .args(["-g", &geometry, "-"])
        .output()
        .map_err(|e| format!("grim failed: {e}"))?;
    image::load_from_memory(&out.stdout).map(|i| i.to_rgb8()).map_err(|e| e.to_string())
}

/// scale: fraction of full resolution (Python's ambient_screen.py default 0.05,
/// AmbientWorker uses 0.15). output: Hyprland monitor name, or None for all.
pub fn capture_tiny(scale: f32, output: Option<&str>) -> Result<RgbImage, String> {
    let mut cmd = Command::new("grim");
    cmd.args(["-s", &scale.to_string()]);
    if let Some(o) = output {
        cmd.args(["-o", o]);
    }
    cmd.arg("-");
    let out = cmd.output().map_err(|e| format!("grim failed: {e}"))?;
    image::load_from_memory(&out.stdout).map(|i| i.to_rgb8()).map_err(|e| e.to_string())
}

/// Set by Hyprland itself in every session it runs (same signal `hyprctl`
/// relies on internally) -- lets the UI grey out Hyprland-only controls
/// (Color-Shot, Ambient, monitor picker) instead of leaving them clickable
/// and silently doing nothing on other compositors.
pub fn is_hyprland() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

/// (name, "name (WxH)") for every Hyprland output, via `hyprctl monitors -j`.
pub fn list_monitors() -> Vec<(String, String)> {
    let Ok(monitors) = run_json(&["hyprctl", "monitors", "-j"]) else {
        return Vec::new();
    };
    monitors
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| {
                    let name = m["name"].as_str()?.to_string();
                    let w = m["width"].as_i64()?;
                    let h = m["height"].as_i64()?;
                    Some((name.clone(), format!("{name} ({w}×{h})")))
                })
                .collect()
        })
        .unwrap_or_default()
}

// ── ambient_screen.py: sample_zones + pick_palette ──────────────────────

/// 5 horizontal stripes from the center third of the image, saturation-
/// weighted average per stripe. Index 0 = bottom -> palette level 0.
pub fn sample_zones(img: &RgbImage) -> [(u8, u8, u8); N_LEVELS] {
    let (w, h) = img.dimensions();
    let (x0, x1) = (w / 3, (2 * w) / 3);
    let stripe = h / N_LEVELS as u32;
    let mut zones = [(0u8, 0u8, 0u8); N_LEVELS];

    for i in 0..N_LEVELS {
        let y1 = h.saturating_sub(i as u32 * stripe);
        let y0 = h.saturating_sub((i as u32 + 1) * stripe).max(0);
        let (mut sr, mut sg, mut sb, mut sw) = (0f64, 0f64, 0f64, 0f64);
        for y in y0..y1 {
            for x in x0..x1 {
                let p = img.get_pixel(x, y);
                let (r, g, b) = (p[0], p[1], p[2]);
                let sat = saturation((r, g, b)) as f64;
                let weight = (sat + 1.0).powi(2);
                sr += r as f64 * weight;
                sg += g as f64 * weight;
                sb += b as f64 * weight;
                sw += weight;
            }
        }
        zones[i] = if sw > 0.0 {
            ((sr / sw) as u8, (sg / sw) as u8, (sb / sw) as u8)
        } else {
            (0, 0, 0)
        };
    }
    zones
}

/// Dice-5: zones ARE the palette, one color per zone, no selection.
pub fn pick_palette(zones: [(u8, u8, u8); N_LEVELS], gamma: f32) -> Palette {
    let mut out = [(0u8, 0u8, 0u8); N_LEVELS];
    for i in 0..N_LEVELS {
        out[i] = correct_for_keyboard_leds(zones[i], gamma);
    }
    out
}

// ── send_palette_from_image.py: filter_pixels + image_to_pixels ─────────

pub fn image_to_pixels(img: &RgbImage, size: u32) -> Vec<(u8, u8, u8)> {
    let (w, h) = img.dimensions();
    let scale = (size as f32 / w.max(h) as f32).min(1.0);
    let (nw, nh) = ((w as f32 * scale).max(1.0) as u32, (h as f32 * scale).max(1.0) as u32);
    let small = image::imageops::thumbnail(img, nw, nh);
    small.pixels().map(|p| (p[0], p[1], p[2])).collect()
}

pub fn filter_pixels(pixels: &[(u8, u8, u8)]) -> Vec<(u8, u8, u8)> {
    let filtered: Vec<(u8, u8, u8)> = pixels
        .iter()
        .copied()
        .filter(|&p| {
            let b = brightness(p);
            b > 30.0 && b < 210.0 && saturation(p) > 20
        })
        .collect();
    if filtered.is_empty() {
        pixels.to_vec()
    } else {
        filtered
    }
}

// ── send_palette_from_screen.py: extract_dominant_palette ───────────────

pub fn extract_dominant_palette(pixels: &[(u8, u8, u8)], gamma: f32) -> Palette {
    use std::collections::HashMap;
    let mut counts: HashMap<(u8, u8, u8), u32> = HashMap::new();
    for &(r, g, b) in pixels {
        *counts.entry((quantize(r, 24), quantize(g, 24), quantize(b, 24))).or_insert(0) += 1;
    }

    let mut candidates: Vec<((u8, u8, u8), u32, f32)> = counts
        .into_iter()
        .map(|(c, n)| (c, n, saturation(c) as f32))
        .collect();

    let valid: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|&(c, _, sat)| brightness(c) > 40.0 && sat > 20.0)
        .collect();
    if !valid.is_empty() {
        candidates = valid;
    }

    candidates.sort_by(|a, b| {
        let score_a = a.1 as f32 * (a.2 + 1.0).powf(1.3);
        let score_b = b.1 as f32 * (b.2 + 1.0).powf(1.3);
        score_b.partial_cmp(&score_a).unwrap()
    });

    let mut selected: Vec<(u8, u8, u8)> = Vec::new();
    for (color, _, _) in &candidates {
        if selected.len() >= N_LEVELS {
            break;
        }
        if selected.iter().any(|&s| dist(*color, s) < 40.0) {
            continue;
        }
        selected.push(*color);
    }
    while selected.len() < N_LEVELS {
        selected.push(selected.last().copied().unwrap_or((16, 16, 16)));
    }

    let mut out = [(0u8, 0u8, 0u8); N_LEVELS];
    for i in 0..N_LEVELS {
        out[i] = correct_for_keyboard_leds(selected[i], gamma);
    }
    out
}
