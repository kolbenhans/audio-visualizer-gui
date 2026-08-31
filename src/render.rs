// Port of python/viz_render.py (viz-frame-tools): all 5 render modes
// (Bars, Dots, Center, Kitt, Waterdrop), palette presets, and the
// underglow-mirrors-nearest-column policy layered on top (not in the
// Python original -- added because the raw g_led_config data mixes
// keylight + underglow LEDs and only the former make sense as "columns").

pub const N_LEVELS: usize = 5;
const VIS_BRIGHTNESS: u32 = 150; // max brightness cap (out of 255), matches Python

// QMK's LED_FLAG_* bitmask (quantum/rgb_matrix/rgb_matrix_types.h).
const LED_FLAG_KEYLIGHT: u8 = 4;

pub type Palette = [(u8, u8, u8); N_LEVELS];

pub const PRESETS: &[(&str, Palette)] = &[
    (
        "Winamp",
        [
            (0, 255, 0),
            (160, 255, 0),
            (255, 220, 0),
            (255, 80, 0),
            (255, 0, 0),
        ],
    ),
    (
        "Purple",
        [
            (76, 0, 153),
            (180, 0, 255),
            (0, 0, 255),
            (0, 255, 255),
            (255, 255, 255),
        ],
    ),
    (
        "Ice",
        [
            (0, 80, 255),
            (0, 180, 255),
            (0, 255, 255),
            (120, 255, 255),
            (255, 255, 255),
        ],
    ),
    (
        "Fire",
        [
            (120, 0, 0),
            (255, 0, 0),
            (255, 80, 0),
            (255, 180, 0),
            (255, 255, 0),
        ],
    ),
    (
        "Neon",
        [
            (255, 0, 128),
            (180, 0, 255),
            (0, 0, 255),
            (0, 255, 128),
            (255, 255, 0),
        ],
    ),
    (
        "Ocean",
        [
            (0, 10, 60),
            (0, 40, 160),
            (0, 120, 255),
            (0, 210, 255),
            (180, 255, 255),
        ],
    ),
];
pub const DEFAULT_PALETTE: Palette = PRESETS[0].1; // Winamp

// ponytail: fallback only, used when fetch_led_layout() fails (plain-VIA
// board, no Vial support). Real boards get their layout read at runtime.
// YMDK ID75 -- id75v3rp.c g_led_config (89 LEDs: 75 key matrix + 14 underglow)
#[rustfmt::skip]
pub const LED_POS_ID75: &[(u16, u16, u8)] = &[
    (224,64,4),(208,64,4),(192,64,4),(176,64,4),(160,64,4),(144,64,4),(128,64,4),(112,64,4),(96,64,4),(80,64,4),(64,64,4),(48,64,4),(32,64,4),(16,64,4),(0,64,4),
    (224,48,4),(208,48,4),(192,48,4),(176,48,4),(160,48,4),(144,48,4),(128,48,4),(112,48,4),(96,48,4),(80,48,4),(64,48,4),(48,48,4),(32,48,4),(16,48,4),(0,48,4),
    (224,32,4),(208,32,4),(192,32,4),(176,32,4),(160,32,4),(144,32,4),(128,32,4),(112,32,4),(96,32,4),(80,32,4),(64,32,4),(48,32,4),(32,32,4),(16,32,4),(0,32,4),
    (224,16,4),(208,16,4),(192,16,4),(176,16,4),(160,16,4),(144,16,4),(128,16,4),(112,16,4),(96,16,4),(80,16,4),(64,16,4),(48,16,4),(32,16,4),(16,16,4),(0,16,4),
    (224,0,4),(208,0,4),(192,0,4),(176,0,4),(160,0,4),(144,0,4),(128,0,4),(112,0,4),(96,0,4),(80,0,4),(64,0,4),(48,0,4),(32,0,4),(16,0,4),(0,0,4),
    (0,64,2),(38,64,2),(76,64,2),(114,64,2),(152,64,2),(190,64,2),(224,64,2),(0,0,2),(38,0,2),(76,0,2),(114,0,2),(152,0,2),(190,0,2),(224,0,2),
];

fn vs(v: u8) -> u8 {
    ((v as u32 * VIS_BRIGHTNESS) / 255) as u8
}
fn s8(a: u8, b: u8) -> u8 {
    ((a as u32 * b as u32) / 255) as u8
}
fn qa8(a: u8, b: u8) -> u8 {
    (a as u16 + b as u16).min(255) as u8
}

/// Distinct physical x-positions among KEYLIGHT-flagged LEDs -> how many
/// audio bands make sense as "columns" for this board.
pub fn column_count(led_pos: &[(u16, u16, u8)]) -> usize {
    let mut xs: Vec<u16> = led_pos
        .iter()
        .filter(|&&(_, _, flags)| flags & LED_FLAG_KEYLIGHT != 0)
        .map(|&(x, _, _)| x)
        .collect();
    xs.sort_unstable();
    xs.dedup();
    xs.len().clamp(4, 24)
}

/// Precomputed per-LED lookup tables, built once per connect() -- port of
/// viz_render.py's set_layout(), plus a generic underglow-mirror policy that
/// applies uniformly across all render modes (not just Bars).
pub struct LedLayout {
    pub led_count: usize,
    pub bands: Vec<usize>,  // Bars/Dots: which audio band this LED belongs to
    pub levels: Vec<usize>, // Bars/Dots: 0=bottom..N_LEVELS-1=top
    pub center_rows: Vec<usize>, // Center/Kitt: which of the N_LEVELS rows
    pub center_dist: Vec<i32>, // Center/Kitt: distance from the board's x-center
    pub mirror_of: Vec<usize>, // non-keylight LEDs: index to copy final color from
}

const CENTER_X: i32 = 112; // QMK's fixed 0-224 coordinate convention's midpoint

pub fn build_layout(led_pos: &[(u16, u16, u8)], n_bands: usize) -> LedLayout {
    let keylight: Vec<usize> = led_pos
        .iter()
        .enumerate()
        .filter(|(_, &(_, _, flags))| flags & LED_FLAG_KEYLIGHT != 0)
        .map(|(i, _)| i)
        .collect();

    let x_max = keylight
        .iter()
        .map(|&i| led_pos[i].0)
        .max()
        .unwrap_or(0)
        .max(1) as usize
        + 1;
    let y_max = keylight
        .iter()
        .map(|&i| led_pos[i].1)
        .max()
        .unwrap_or(0)
        .max(1) as usize
        + 1;

    let mut bands = Vec::with_capacity(led_pos.len());
    let mut levels = Vec::with_capacity(led_pos.len());
    let mut center_rows = Vec::with_capacity(led_pos.len());
    let mut center_dist = Vec::with_capacity(led_pos.len());
    let mut mirror_of = Vec::with_capacity(led_pos.len());

    for (i, &(x, y, flags)) in led_pos.iter().enumerate() {
        bands.push(((x as usize * n_bands) / x_max).min(n_bands - 1));
        levels.push((N_LEVELS - 1) - ((y as usize * N_LEVELS) / y_max).min(N_LEVELS - 1));
        center_rows.push((y as usize * N_LEVELS / y_max).min(N_LEVELS - 1));
        center_dist.push((x as i32 - CENTER_X).abs());

        mirror_of.push(if flags & LED_FLAG_KEYLIGHT != 0 {
            i
        } else {
            // Nearest KEYLIGHT LED, weighted heavily towards same x (column)
            // over exact y (row) match.
            keylight
                .iter()
                .min_by_key(|&&k| {
                    let (kx, ky) = (led_pos[k].0 as i32, led_pos[k].1 as i32);
                    let (x, y) = (x as i32, y as i32);
                    (kx - x).abs() * 4 + (ky - y).abs()
                })
                .copied()
                .unwrap_or(i)
        });
    }

    LedLayout {
        led_count: led_pos.len(),
        bands,
        levels,
        center_rows,
        center_dist,
        mirror_of,
    }
}

/// Applies the underglow-mirror policy to an already-rendered frame. Call
/// this after any render_* function, regardless of mode.
pub fn apply_mirror(frame: &mut [(u8, u8, u8)], layout: &LedLayout) {
    for i in 0..frame.len() {
        let src = layout.mirror_of[i];
        if src != i {
            frame[i] = frame[src];
        }
    }
}

/// Modes 0 (filled bars) / 1 (dots -- outline=true, only the topmost lit LED
/// per column). display: per-band smoothed magnitudes, 0..=255.
pub fn render_bars(
    display: &[f32],
    pal: &Palette,
    layout: &LedLayout,
    outline: bool,
) -> Vec<(u8, u8, u8)> {
    let heights: Vec<usize> = display
        .iter()
        .map(|&d| (((d * (N_LEVELS as f32 + 1.0)) / 256.0) as usize).min(N_LEVELS))
        .collect();

    (0..layout.led_count)
        .map(|i| {
            let h = heights[layout.bands[i]];
            let level = layout.levels[i];
            let lit = if outline {
                h > 0 && level == h - 1
            } else {
                level < h
            };
            if lit {
                let (r, g, b) = pal[level];
                (vs(r), vs(g), vs(b))
            } else {
                (0, 0, 0)
            }
        })
        .collect()
}

/// Mode 2 (Center) / Mode 3 (Kitt): horizontal bars expanding from the
/// board's x-center. Each of the N_LEVELS rows is driven by every other
/// band. Center: color by distance from center; Kitt: color by row.
pub fn render_center(
    display: &[f32],
    pal: &Palette,
    layout: &LedLayout,
    kitt: bool,
) -> Vec<(u8, u8, u8)> {
    const MAX_DIST: i32 = CENTER_X;
    let n_bands = display.len();
    let row_vals: Vec<f32> = (0..N_LEVELS)
        .map(|i| display[(i * 2).min(n_bands - 1)])
        .collect();

    (0..layout.led_count)
        .map(|i| {
            let row = layout.center_rows[i];
            let val = row_vals[row];
            if val <= 0.0 {
                return (0, 0, 0);
            }
            let width = (val * MAX_DIST as f32 / 210.0) as i32;
            let dist = layout.center_dist[i];
            if dist <= width {
                let ci = if kitt {
                    row
                } else {
                    ((dist * N_LEVELS as i32 / MAX_DIST) as usize).min(N_LEVELS - 1)
                };
                let (r, g, b) = pal[ci];
                (vs(r), vs(g), vs(b))
            } else {
                (0, 0, 0)
            }
        })
        .collect()
}

// ── Waterdrop ─────────────────────────────────────────────────────────────

const WD_COUNT: usize = 6;
const WD_SPEED: i32 = 6;
const WD_THRESHOLD: f32 = 187.0; // 0-255; bass above this triggers a drop
const WD_GAP: u8 = 3; // frames between consecutive drops

#[derive(Clone, Copy)]
struct WdDrop {
    radius: i32,
    age: u8,
    active: bool,
    color: usize,
}

pub struct WaterdropState {
    drops: [WdDrop; WD_COUNT],
    next_color: usize,
    cooldown: u8,
}

impl WaterdropState {
    pub fn new() -> Self {
        Self {
            drops: [WdDrop {
                radius: 0,
                age: 0,
                active: false,
                color: 0,
            }; WD_COUNT],
            next_color: 0,
            cooldown: 0,
        }
    }
}

impl Default for WaterdropState {
    fn default() -> Self {
        Self::new()
    }
}

/// Mode 4: bass-triggered ripple rings expanding from each half's center.
pub fn render_waterdrop(
    display: &[f32],
    pal: &Palette,
    layout: &LedLayout,
    led_pos: &[(u16, u16, u8)],
    state: &mut WaterdropState,
) -> Vec<(u8, u8, u8)> {
    let bass = display[0]
        .max(display[1])
        .max(display.get(2).copied().unwrap_or(0.0));

    if state.cooldown > 0 {
        state.cooldown -= 1;
    }
    if bass > WD_THRESHOLD && state.cooldown == 0 {
        if let Some(drop) = state.drops.iter_mut().find(|d| !d.active) {
            drop.radius = 0;
            drop.age = 255;
            drop.active = true;
            drop.color = state.next_color;
            state.next_color = (state.next_color + 1) % N_LEVELS;
            state.cooldown = WD_GAP;
        }
    }

    let mut frame = vec![(0u8, 0u8, 0u8); layout.led_count];
    for (i, &(x, y, _)) in led_pos.iter().enumerate() {
        let cx = if (x as i32) < 112 { 56 } else { 168 };
        let dist = (((x as i32 - cx).pow(2) + (y as i32 - 32).pow(2)) as f32).sqrt() as i32;

        let (mut r, mut g, mut b) = (0u8, 0u8, 0u8);
        for drop in &state.drops {
            if !drop.active {
                continue;
            }
            let effect = (drop.radius - dist).clamp(0, 255);
            let bright = 255 - effect;
            if bright == 0 {
                continue;
            }
            let delta = (dist - drop.radius).abs();
            // ponytail: matches viz_render.py's own dead code -- `ci` is
            // computed there too but never actually used (color comes from
            // drop.color below, not this delta-based index). Kept for
            // faithful parity rather than silently "fixing" upstream's bug.
            let _ci = if delta < 12 {
                4
            } else if delta < 28 {
                3
            } else if delta < 44 {
                2
            } else if delta < 64 {
                1
            } else {
                0
            };
            let fade = (255 - delta * 4).max(0) as u8;
            let fb = s8(s8(bright as u8, fade), drop.age);
            let (pr, pg, pb) = pal[drop.color];
            r = qa8(r, s8(vs(pr), fb));
            g = qa8(g, s8(vs(pg), fb));
            b = qa8(b, s8(vs(pb), fb));
        }
        frame[i] = (r, g, b);
    }

    for drop in &mut state.drops {
        if !drop.active {
            continue;
        }
        drop.radius += WD_SPEED;
        if drop.age > 8 {
            drop.age -= 8;
        } else {
            drop.active = false;
        }
    }

    frame
}
