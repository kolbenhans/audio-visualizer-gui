use crate::render::{Palette, N_LEVELS};
use image::RgbImage;
use material_colors::{color::Rgb as McRgb, hct::Hct};
use std::path::PathBuf;
#[cfg(unix)]
use std::process::Command;

const CAELESTIA_WALLPAPER_PATH: &str = ".local/state/caelestia/wallpaper/path.txt";
const OMARCHY_WALLPAPER_PATH: &str = ".local/state/omarchy/current/background";
const OMARCHY3_WALLPAPER_PATH: &str = ".config/omarchy/current/background";
const HYPRPAPER_CONF_PATH: &str = ".config/hypr/hyprpaper.conf";
const KDE_PLASMA_CONF_PATH: &str = ".config/plasma-org.kde.plasma.desktop-appletsrc";

fn quantize(v: u8, step: u8) -> u8 {
    (v / step) * step
}

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

pub fn load_image_rgb(path: &std::path::Path) -> Result<RgbImage, String> {
    image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?
        .decode()
        .map(|img| img.to_rgb8())
        .map_err(|e| e.to_string())
}

#[cfg(windows)]
mod win {
    use image::RgbImage;

    fn to_rgb(img: xcap::image::RgbaImage) -> RgbImage {
        image::DynamicImage::ImageRgba8(img).to_rgb8()
    }

    pub fn grab_active_monitor() -> Result<RgbImage, String> {
        let windows = xcap::Window::all().map_err(|e| e.to_string())?;
        let focused = windows
            .iter()
            .find(|w| w.is_focused().unwrap_or(false))
            .ok_or("no focused window")?;
        let monitor = focused.current_monitor().map_err(|e| e.to_string())?;
        monitor.capture_image().map(to_rgb).map_err(|e| e.to_string())
    }

    pub fn capture_tiny(scale: f32, output: Option<&str>) -> Result<RgbImage, String> {
        let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
        let monitor = match output {
            Some(name) => monitors
                .iter()
                .find(|m| m.name().map(|n| n == name).unwrap_or(false))
                .ok_or_else(|| format!("monitor {name} not found"))?,
            None => monitors
                .iter()
                .find(|m| m.is_primary().unwrap_or(false))
                .or_else(|| monitors.first())
                .ok_or("no monitors found")?,
        };
        let img = to_rgb(monitor.capture_image().map_err(|e| e.to_string())?);
        let (w, h) = img.dimensions();
        let (nw, nh) = (((w as f32 * scale).max(1.0)) as u32, ((h as f32 * scale).max(1.0)) as u32);
        Ok(image::imageops::thumbnail(&img, nw, nh))
    }

    pub fn list_monitors() -> Vec<(String, String)> {
        let Ok(monitors) = xcap::Monitor::all() else {
            return Vec::new();
        };
        monitors
            .iter()
            .filter_map(|m| {
                let name = m.name().ok()?;
                let (w, h) = (m.width().ok()?, m.height().ok()?);
                Some((name.clone(), format!("{name} ({w}×{h})")))
            })
            .collect()
    }

    pub fn is_hyprland() -> bool {
        true
    }
}

#[cfg(windows)]
pub use win::{capture_tiny, grab_active_monitor, is_hyprland, list_monitors};

// ── grim capture (Linux/Hyprland) ───────────────────────────────────────

#[cfg(unix)]
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

#[cfg(unix)]
fn run_json(cmd: &[&str]) -> Result<serde_json::Value, String> {
    let out = Command::new(cmd[0]).args(&cmd[1..]).output().map_err(|e| e.to_string())?;
    serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())
}

#[cfg(unix)]
pub fn grab_active_monitor() -> Result<RgbImage, String> {
    let geometry = get_active_monitor_geometry()?;
    let out = Command::new("grim")
        .args(["-g", &geometry, "-"])
        .output()
        .map_err(|e| format!("grim failed: {e}"))?;
    image::load_from_memory(&out.stdout).map(|i| i.to_rgb8()).map_err(|e| e.to_string())
}

#[cfg(unix)]
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

#[cfg(unix)]
pub fn is_hyprland() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
}

/// (name, "name (WxH)") for every Hyprland output, via `hyprctl monitors -j`.
#[cfg(unix)]
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

// ── Variant color boost ──────────────────────────────────────────────────
// First attempt here ran every pixel through Google's Material Color
// Utilities DynamicScheme from one seed color (matugen/Caelestia's
// approach). Turned out wrong for this: that machinery is tuned for
// generating a single harmonious *UI theme* from one seed, not "give me N
// bold, varied colors from this image". Several variants (TonalSpot,
// Rainbow, FruitSalad, Content, ...) reuse the same hue for primary and
// secondary, Vibrant/Expressive's "distinct" hue rotations are small for
// some seed hues and huge for others (hand-tuned per hue bucket, by
// Google's own design), and `error_palette` is hardcoded red regardless of
// variant. Net effect: washed-out, same-ish, or off-topic colors -- see
// conversation history.
//
// This instead stays on the existing dominant-cluster extraction (the same
// one Zones/extract_dominant_palette uses -- real, distinct colors already
// present in the image) and only uses HCT to push each picked color's
// chroma/tone, keeping its real hue. "Variant" now means two knobs: how
// spread-out the picks must be (`spread`, the cluster dedup distance) and
// how hard chroma/tone get pushed (`chroma`/`tone`).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorVariant {
    /// The original per-source extraction (sample_zones for WPWatch/
    /// Ambient, extract_dominant_palette for Color-Shot) -- literal colors
    /// from the image, no boosting.
    Zones,
    TonalSpot,
    Vibrant,
    Expressive,
    FruitSalad,
    Rainbow,
    Content,
    Neutral,
}

impl ColorVariant {
    pub const ALL: &'static [ColorVariant] = &[
        Self::Zones,
        Self::TonalSpot,
        Self::Vibrant,
        Self::Expressive,
        Self::FruitSalad,
        Self::Rainbow,
        Self::Content,
        Self::Neutral,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Self::Zones => "Zones (default)",
            Self::TonalSpot => "Tonal Spot",
            Self::Vibrant => "Vibrant",
            Self::Expressive => "Expressive",
            Self::FruitSalad => "Fruit Salad",
            Self::Rainbow => "Rainbow",
            Self::Content => "Content",
            Self::Neutral => "Neutral",
        }
    }

    /// (cluster spread, min chroma, chroma multiplier, forced tone).
    /// Spread is the minimum plain-RGB distance enforced between the 5
    /// picked colors (see dominant_colors) -- higher forces a wider hue
    /// spread when the image actually has one (Rainbow/Expressive/
    /// FruitSalad), lower keeps a tighter, more cohesive set (TonalSpot/
    /// Neutral). Tone of None keeps each color's own extracted brightness
    /// instead of forcing one (Content, which means "stay close to the
    /// source" in Material You too).
    fn boost(&self) -> Option<Boost> {
        Some(match self {
            Self::Zones => return None,
            Self::TonalSpot => Boost { spread: 40.0, min_chroma: 25.0, chroma_mult: 1.15, tone: Some(52.0) },
            Self::Content => Boost { spread: 40.0, min_chroma: 10.0, chroma_mult: 1.0, tone: None },
            Self::Neutral => Boost { spread: 25.0, min_chroma: 0.0, chroma_mult: 0.5, tone: Some(55.0) },
            Self::Vibrant => Boost { spread: 50.0, min_chroma: 55.0, chroma_mult: 1.6, tone: Some(50.0) },
            Self::FruitSalad => Boost { spread: 65.0, min_chroma: 50.0, chroma_mult: 1.4, tone: Some(58.0) },
            Self::Expressive => Boost { spread: 70.0, min_chroma: 55.0, chroma_mult: 1.5, tone: Some(50.0) },
            Self::Rainbow => Boost { spread: 85.0, min_chroma: 50.0, chroma_mult: 1.4, tone: Some(55.0) },
        })
    }
}

#[derive(Clone, Copy)]
struct Boost {
    spread: f32,
    min_chroma: f64,
    chroma_mult: f64,
    tone: Option<f64>,
}

/// Picks N_LEVELS distinct dominant colors out of `pixels` (see
/// dominant_colors) and pushes each one's HCT chroma/tone per `variant`,
/// keeping every color's real hue. Returns None for ColorVariant::Zones
/// (caller should use the per-source method instead).
pub fn pick_palette_variant(pixels: &[(u8, u8, u8)], variant: ColorVariant, gamma: f32) -> Option<Palette> {
    let boost = variant.boost()?;
    let colors = dominant_colors(pixels, boost.spread);

    let mut out = [(0u8, 0u8, 0u8); N_LEVELS];
    for i in 0..N_LEVELS {
        let (r, g, b) = colors[i];
        let hct = Hct::new(McRgb::new(r, g, b));
        let chroma = (hct.get_chroma() * boost.chroma_mult).max(boost.min_chroma);
        let tone = boost.tone.unwrap_or_else(|| hct.get_tone());
        let boosted: McRgb = Hct::from(hct.get_hue(), chroma, tone).into();
        out[i] = correct_for_keyboard_leds((boosted.red, boosted.green, boosted.blue), gamma);
    }
    Some(out)
}

/// WPWatch/Ambient entry point: boosted dominant colors for any variant,
/// falling back to the original sample_zones extraction for
/// ColorVariant::Zones. 600 (vs. the 96 image_to_pixels' other two callers
/// use) because a thumbnail that small can blur away a thin accent (a neon
/// sign, a small flower) entirely before the color histogram ever sees it
/// -- confirmed by testing a busy wallpaper where real, highly-saturated
/// accent colors simply weren't in the pixel data anymore at 96-150px.
/// Only costs anything on WPWatch (loads the full-res wallpaper file) --
/// Ambient's own grim capture is already downscaled well below 600px by
/// its own `-s` scale flag (see capture_tiny), so this is a no-op there.
pub fn pick_palette_for_image(img: &RgbImage, variant: ColorVariant, gamma: f32) -> Palette {
    let pixels = filter_pixels(&image_to_pixels(img, 600));
    pick_palette_variant(&pixels, variant, gamma).unwrap_or_else(|| pick_palette(sample_zones(img), gamma))
}

/// Color-Shot entry point: same idea as pick_palette_for_image, but for
/// callers that already have a filtered pixel list (Color-Shot samples the
/// screen grab down to a pixel list before this point, see main.rs).
pub fn pick_palette_for_pixels(pixels: &[(u8, u8, u8)], variant: ColorVariant, gamma: f32) -> Palette {
    pick_palette_variant(pixels, variant, gamma).unwrap_or_else(|| extract_dominant_palette(pixels, gamma))
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

/// Quantized color histogram, scored by count × saturation and sorted
/// best-first -- shared by extract_dominant_palette and dominant_colors
/// (both take the top N, deduped at their own distance threshold).
fn dominant_candidates(pixels: &[(u8, u8, u8)]) -> Vec<((u8, u8, u8), u32, f32)> {
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

    // Count capped, saturation weighted hard: an image can have a vivid
    // accent color (neon sign, a single flower) outnumbered 50:1 by a
    // large muted area (sky, wall, haze) -- with count uncapped and a mild
    // saturation exponent, that accent never outranks the muted area no
    // matter how vivid, so it never gets picked. Found by checking what
    // dominant_candidates actually returned for a busy neon-cyberpunk
    // wallpaper: real, highly-saturated green/yellow neon text sat ~200+
    // ranked in the hundreds, buried under tens of thousands of a
    // moderately-saturated (~100-150) blue haze.
    const COUNT_CAP: u32 = 1500;
    candidates.sort_by(|a, b| {
        let score = |count: u32, sat: f32| count.min(COUNT_CAP) as f32 * (sat + 1.0).powf(2.2);
        let score_a = score(a.1, a.2);
        let score_b = score(b.1, b.2);
        score_b.partial_cmp(&score_a).unwrap()
    });
    candidates
}

/// Up to N_LEVELS distinct dominant colors in `pixels`, best-first, each at
/// least `min_dist` (plain RGB euclidean) apart -- padded by repeating the
/// last pick if the image doesn't have N_LEVELS distinct-enough colors.
/// Shared by extract_dominant_palette (fixed 40.0 spread) and
/// pick_palette_variant (spread set per ColorVariant, see Boost).
fn dominant_colors(pixels: &[(u8, u8, u8)], min_dist: f32) -> [(u8, u8, u8); N_LEVELS] {
    let candidates = dominant_candidates(pixels);

    let mut selected: Vec<(u8, u8, u8)> = Vec::new();
    for (color, _, _) in &candidates {
        if selected.len() >= N_LEVELS {
            break;
        }
        if selected.iter().any(|&s| dist(*color, s) < min_dist) {
            continue;
        }
        selected.push(*color);
    }
    while selected.len() < N_LEVELS {
        selected.push(selected.last().copied().unwrap_or((16, 16, 16)));
    }

    std::array::from_fn(|i| selected[i])
}

pub fn extract_dominant_palette(pixels: &[(u8, u8, u8)], gamma: f32) -> Palette {
    let selected = dominant_colors(pixels, 40.0);
    let mut out = [(0u8, 0u8, 0u8); N_LEVELS];
    for i in 0..N_LEVELS {
        out[i] = correct_for_keyboard_leds(selected[i], gamma);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 5 clearly distinct, saturated colors, each repeated so it dominates
    /// the histogram -- enough for dominant_colors to find N_LEVELS
    /// distinct clusters at any of the Boost presets' spread values.
    fn rainbow_pixels() -> Vec<(u8, u8, u8)> {
        let base = [(220u8, 30u8, 30u8), (30, 200, 60), (40, 80, 230), (230, 200, 30), (190, 40, 190)];
        base.iter().cycle().take(50 * base.len()).copied().collect()
    }

    // Regression guard for the bugs found by actually rendering the output
    // across iterations of this (see the Boost doc comment for what didn't
    // work: single-seed DynamicScheme converged every variant toward
    // pale/same-ish/off-hue colors regardless of the image).
    #[test]
    fn variant_palettes_differ_and_stay_saturated() {
        let pixels = rainbow_pixels();
        let gamma = 1.6;
        let tonal_spot = pick_palette_variant(&pixels, ColorVariant::TonalSpot, gamma).unwrap();
        let vibrant = pick_palette_variant(&pixels, ColorVariant::Vibrant, gamma).unwrap();
        let rainbow = pick_palette_variant(&pixels, ColorVariant::Rainbow, gamma).unwrap();

        assert_ne!(tonal_spot, vibrant, "variants should differ for the same image");
        assert_ne!(vibrant, rainbow, "variants should differ for the same image");

        for (name, pal) in [("Vibrant", vibrant), ("Rainbow", rainbow)] {
            let near_white = pal.iter().filter(|&&(r, g, b)| r > 230 && g > 230 && b > 230).count();
            assert!(near_white < N_LEVELS, "{name}: too many near-white levels: {pal:?}");
            let washed_out = pal.iter().filter(|&&c| saturation(c) < 40).count();
            assert!(washed_out == 0, "{name}: expected all levels saturated, got {pal:?}");
        }

        assert!(pick_palette_variant(&pixels, ColorVariant::Zones, gamma).is_none());
    }
}

#[cfg(test)]
mod preview_debug {
    use super::*;

    #[test]
    #[ignore]
    fn preview_variants_on_wallpaper() {
        let path_str = std::env::var("PREVIEW_WALLPAPER").expect("set PREVIEW_WALLPAPER");
        let out_path = std::env::var("PREVIEW_OUT").expect("set PREVIEW_OUT");
        let img = load_image_rgb(std::path::Path::new(&path_str)).expect("load wallpaper");

        let mut out = image::RgbImage::new(5 * 60, ColorVariant::ALL.len() as u32 * 40);
        for (row, &variant) in ColorVariant::ALL.iter().enumerate() {
            let pal = pick_palette_for_image(&img, variant, 1.6);
            println!("{:>16}: {:?}", variant.label(), pal);
            for (col, &(r, g, b)) in pal.iter().enumerate() {
                for y in 0..40u32 {
                    for x in 0..60u32 {
                        out.put_pixel(col as u32 * 60 + x, row as u32 * 40 + y, image::Rgb([r, g, b]));
                    }
                }
            }
        }
        out.save(&out_path).unwrap();
    }
}
