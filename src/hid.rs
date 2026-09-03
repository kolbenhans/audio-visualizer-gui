// VialRGB FASTSET protocol — port of python/viz_hid.py (viz-frame-tools).
use hidapi::{HidApi, HidDevice};

const USAGE_PAGE: u16 = 0xFF60;
const USAGE: u16 = 0x61;
const LEDS_PER_PKT: usize = 9;

const VIA_LIGHTING_SET: u8 = 0x07;
const VIA_LIGHTING_GET: u8 = 0x08;
const GET_INFO: u8 = 0x40;
const FASTSET: u8 = 0x42;
// VialRGB's onboard RGB Matrix mode get/set (quantum/vialrgb.h/.c upstream) --
// distinct from GET_INFO/FASTSET above: this drives the board's own stock
// RGB Matrix animations (breathing/rainbow/etc.), not FASTSET's host-streamed
// per-LED frames. Same command byte (0x41) under both GET and SET, distinct
// namespaces since data[0] (VIA_LIGHTING_GET vs _SET) picks which one.
const RGB_MODE: u8 = 0x41;
const RGB_SUPPORTED: u8 = 0x42;

// Vial's own definition-fetch protocol (same one keypeek uses to read a
// board's keymap layout) -- reused here to read rgb_matrix.layout instead of
// hand-copying LED_POS tables per board. See qmk-via-api/vial.rs upstream.
const VIAL_PREFIX: u8 = 0xFE;
const VIAL_CMD_SIZE: u8 = 0x01;
const VIAL_CMD_DEF: u8 = 0x02;

pub struct DeviceInfo {
    pub path: std::ffi::CString,
    pub label: String,
    serial: String, // used only to disambiguate identical-looking devices, see disambiguate()
}

pub fn list_devices(api: &HidApi) -> Vec<DeviceInfo> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for d in api.device_list() {
        if d.usage_page() == USAGE_PAGE && d.usage() == USAGE {
            let path = d.path().to_owned();
            if seen.insert(path.clone()) {
                let label = d.product_string().unwrap_or("Keyboard").trim().to_string();
                out.push(DeviceInfo {
                    path,
                    label: if label.is_empty() {
                        "Keyboard".into()
                    } else {
                        label
                    },
                    serial: d.serial_number().unwrap_or("").trim().to_string(),
                });
            }
        }
    }
    // hidapi on Linux sometimes reports usage_page=0 — fall back to interface 1
    if out.is_empty() {
        for d in api.device_list() {
            if d.interface_number() == 1 {
                let path = d.path().to_owned();
                if seen.insert(path.clone()) {
                    out.push(DeviceInfo {
                        path,
                        label: d.product_string().unwrap_or("Keyboard").trim().to_string(),
                        serial: d.serial_number().unwrap_or("").trim().to_string(),
                    });
                }
            }
        }
    }
    disambiguate(&mut out);
    out
}

/// Two identical boards (same product string, e.g. two ID75s) would
/// otherwise show up as indistinguishable duplicate entries in the picker.
/// Appends a distinguishing suffix -- the USB serial number if the firmware
/// sets one (RP2040 boards typically expose the chip's unique flash ID by
/// default, so this works even with identical firmware/`VIAL_KEYBOARD_UID`
/// on both), falling back to the hidraw path otherwise.
fn disambiguate(devices: &mut [DeviceInfo]) {
    let mut counts = std::collections::HashMap::new();
    for d in devices.iter() {
        *counts.entry(d.label.clone()).or_insert(0u32) += 1;
    }
    for d in devices.iter_mut() {
        if counts[&d.label] > 1 {
            let suffix = if !d.serial.is_empty() {
                d.serial.clone()
            } else {
                d.path.to_string_lossy().rsplit('/').next().unwrap_or("?").to_string()
            };
            d.label = format!("{} ({suffix})", d.label);
        }
    }
}

pub struct VizKeyboard {
    dev: HidDevice,
}

#[derive(Clone, Copy, PartialEq)]
pub struct RgbMatrixMode {
    pub effect: u16,
    pub speed: u8,
    pub hue: u8,
    pub sat: u8,
    pub val: u8, // brightness
}

/// VialRGB effect ids 0-44, in table order (quantum/vialrgb_effects.inc
/// upstream: "never reorder or remove these, only add new animations at the
/// end" -- ids are a stable, board-independent enum, so this can be a plain
/// static table. `get_supported_rgb_effects()` says which of these a given
/// board actually has compiled in; anything beyond this table (a newer
/// upstream animation, or one of our own key_colors/audio_visualizer modes,
/// which don't go through this list at all) falls back to "Effect N".
pub const RGB_EFFECT_NAMES: &[&str] = &[
    "Off", "Direct", "Solid Color", "Alphas/Mods", "Gradient Up/Down", "Gradient Left/Right",
    "Breathing", "Band Sat", "Band Val", "Band Pinwheel Sat", "Band Pinwheel Val",
    "Band Spiral Sat", "Band Spiral Val", "Cycle All", "Cycle Left/Right", "Cycle Up/Down",
    "Rainbow Moving Chevron", "Cycle Out/In", "Cycle Out/In Dual", "Cycle Pinwheel",
    "Cycle Spiral", "Dual Beacon", "Rainbow Beacon", "Rainbow Pinwheels", "Raindrops",
    "Jellybean Raindrops", "Hue Breathing", "Hue Pendulum", "Hue Wave", "Typing Heatmap",
    "Digital Rain", "Solid Reactive Simple", "Solid Reactive", "Solid Reactive Wide",
    "Solid Reactive Multiwide", "Solid Reactive Cross", "Solid Reactive Multicross",
    "Solid Reactive Nexus", "Solid Reactive Multinexus", "Splash", "Multisplash",
    "Solid Splash", "Solid Multisplash", "Pixel Rain", "Pixel Fractal",
];

pub fn rgb_effect_name(id: u16) -> String {
    RGB_EFFECT_NAMES
        .get(id as usize)
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("Effect {id}"))
}

fn rgb_to_qhsv(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let (rf, gf, bf) = (r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0);
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;
    let v = max;
    let s = if max == 0.0 { 0.0 } else { delta / max };
    let h = if delta == 0.0 {
        0.0
    } else if max == rf {
        ((gf - bf) / delta).rem_euclid(6.0) / 6.0
    } else if max == gf {
        ((bf - rf) / delta + 2.0) / 6.0
    } else {
        ((rf - gf) / delta + 4.0) / 6.0
    };
    ((h * 255.0) as u8, (s * 255.0) as u8, (v * 255.0) as u8)
}

impl VizKeyboard {
    pub fn open(api: &HidApi, path: &std::ffi::CStr) -> Result<Self, String> {
        let dev = api.open_path(path).map_err(|e| e.to_string())?;
        Ok(Self { dev })
    }

    /// Drains any stale queued HID reports before a request/response round
    /// trip. `send_frame()`'s FASTSET writes are fire-and-forget (no read --
    /// reading an ack per LED chunk would tank the visualizer's frame rate),
    /// but the firmware still sends one ack per write, so those pile up
    /// unread in the kernel's hidraw queue while Visualizer runs. Without
    /// this, the next wr() call here reads one of those leftover acks
    /// instead of its own reply -- garbage decoded as whatever that stale
    /// packet's bytes happen to be.
    fn flush(&self) {
        let mut buf = [0u8; 32];
        while matches!(self.dev.read_timeout(&mut buf, 0), Ok(n) if n > 0) {}
    }

    fn wr(&self, data: &[u8]) -> Result<[u8; 32], String> {
        self.flush();
        let mut pkt = [0u8; 33];
        pkt[1..1 + data.len()].copy_from_slice(data);
        self.dev.write(&pkt).map_err(|e| e.to_string())?;
        let mut resp = [0u8; 32];
        self.dev
            .read_timeout(&mut resp, 500)
            .map_err(|e| e.to_string())?;
        Ok(resp)
    }

    /// Returns (protocol_version, max_brightness).
    pub fn get_info(&self) -> Result<(u16, u8), String> {
        let r = self.wr(&[VIA_LIGHTING_GET, GET_INFO])?;
        Ok((r[2] as u16 | ((r[3] as u16) << 8), r[4]))
    }

    /// Switch firmware into audio_visualizer mode via Raw HID 0x02/0xA3
    /// (our own keymap-level opcode, not VialRGB's).
    pub fn activate_audio_visualizer(&self) -> Result<(), String> {
        let mut pkt = [0u8; 33];
        pkt[1] = 0x02;
        pkt[2] = 0xA3;
        self.dev.write(&pkt).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Switch firmware back to the key_colors (static WebGUI) effect via
    /// Raw HID 0x02/0xA4.
    pub fn activate_key_colors(&self) -> Result<(), String> {
        let mut pkt = [0u8; 33];
        pkt[1] = 0x02;
        pkt[2] = 0xA4;
        self.dev.write(&pkt).map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Send one full frame (list of (r,g,b) per LED) via 0x42 FASTSET,
    /// chunked LEDS_PER_PKT at a time.
    pub fn send_frame(&self, rgb: &[(u8, u8, u8)]) -> Result<(), String> {
        for (chunk_i, chunk) in rgb.chunks(LEDS_PER_PKT).enumerate() {
            let start = chunk_i * LEDS_PER_PKT;
            let mut pkt = [0u8; 33];
            pkt[1] = VIA_LIGHTING_SET;
            pkt[2] = FASTSET;
            pkt[3] = (start & 0xFF) as u8;
            pkt[4] = ((start >> 8) & 0xFF) as u8;
            pkt[5] = chunk.len() as u8;
            for (i, &(r, g, b)) in chunk.iter().enumerate() {
                let (h, s, v) = rgb_to_qhsv(r, g, b);
                pkt[6 + i * 3] = h;
                pkt[7 + i * 3] = s;
                pkt[8 + i * 3] = v;
            }
            self.dev.write(&pkt).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Current onboard RGB Matrix state: effect id (VialRGB numbering, see
    /// `RGB_EFFECT_NAMES`), speed, and HSV (val = brightness).
    pub fn get_rgb_matrix_mode(&self) -> Result<RgbMatrixMode, String> {
        let r = self.wr(&[VIA_LIGHTING_GET, RGB_MODE])?;
        Ok(RgbMatrixMode {
            effect: r[2] as u16 | ((r[3] as u16) << 8),
            speed: r[4],
            hue: r[5],
            sat: r[6],
            val: r[7],
        })
    }

    /// Sets effect+speed+HSV in one shot -- that's how the firmware's
    /// vialrgb_set_mode takes them, no way to change just one field without
    /// echoing the rest (see get_rgb_matrix_mode first).
    pub fn set_rgb_matrix_mode(&self, m: &RgbMatrixMode) -> Result<(), String> {
        self.wr(&[
            VIA_LIGHTING_SET,
            RGB_MODE,
            (m.effect & 0xFF) as u8,
            (m.effect >> 8) as u8,
            m.speed,
            m.hue,
            m.sat,
            m.val,
        ])?;
        Ok(())
    }

    /// Effect ids this firmware build actually has compiled in (VialRGB's
    /// GET_SUPPORTED is paginated -- up to 15 ids per request, "ids greater
    /// than the last one seen" -- so this loops until a round adds nothing
    /// new). A firmware bug that never terminates is capped at 20 rounds
    /// (300 ids, comfortably above the ~45 stock effects that exist).
    pub fn get_supported_rgb_effects(&self) -> Result<Vec<u16>, String> {
        let mut ids = Vec::new();
        let mut gt: u16 = 0;
        for _ in 0..20 {
            let r = self.wr(&[VIA_LIGHTING_GET, RGB_SUPPORTED, (gt & 0xFF) as u8, (gt >> 8) as u8])?;
            let mut max_id = gt;
            let mut got_any = false;
            for pair in r[2..].chunks_exact(2) {
                let id = pair[0] as u16 | ((pair[1] as u16) << 8);
                if id == 0xFFFF {
                    break;
                }
                ids.push(id);
                max_id = max_id.max(id);
                got_any = true;
            }
            if !got_any || max_id <= gt {
                break;
            }
            gt = max_id;
        }
        Ok(ids)
    }

    fn vial_cmd(&self, cmd: u8, param: u32) -> Result<[u8; 32], String> {
        let mut data = [0u8; 6];
        data[0] = VIAL_PREFIX;
        data[1] = cmd;
        data[2..6].copy_from_slice(&param.to_le_bytes());
        self.wr(&data)
    }

    /// Fetches the board's own embedded (xz-compressed) vial.json over HID
    /// and returns its rgb_matrix.layout as (x, y, flags) per LED, in LED
    /// order. flags follows QMK's LED_FLAG_* bitmask (KEYLIGHT=4,
    /// UNDERGLOW=2, ...). Requires a Vial-protocol keyboard (not plain VIA).
    pub fn fetch_led_layout(&self) -> Result<Vec<(u16, u16, u8)>, String> {
        let resp = self.vial_cmd(VIAL_CMD_SIZE, 0)?;
        let size = u32::from_le_bytes([resp[0], resp[1], resp[2], resp[3]]) as usize;
        if size == 0 {
            return Err("device reports empty VIAL definition (plain VIA, not Vial?)".into());
        }

        let mut compressed = Vec::with_capacity(size);
        let mut block: u32 = 0;
        while compressed.len() < size {
            let resp = self.vial_cmd(VIAL_CMD_DEF, block)?;
            let n = (size - compressed.len()).min(32);
            compressed.extend_from_slice(&resp[..n]);
            block += 1;
        }

        let mut decompressed = Vec::new();
        lzma_rs::xz_decompress(&mut std::io::Cursor::new(&compressed), &mut decompressed)
            .map_err(|e| format!("xz decompress failed: {e}"))?;
        let json_str = String::from_utf8(decompressed).map_err(|e| e.to_string())?;
        let json: serde_json::Value = serde_json::from_str(&json_str).map_err(|e| e.to_string())?;
        parse_rgb_matrix_layout(&json)
    }
}

/// Shared by `fetch_led_layout` (device's embedded vial.json) and
/// `load_led_layout_file` (a local `qmk info -f json` export) -- both are
/// the same `rgb_matrix.layout` shape at the same JSON path, see
/// qmk-modules/docs/vial-json-rgb-matrix-layout.md.
fn parse_rgb_matrix_layout(json: &serde_json::Value) -> Result<Vec<(u16, u16, u8)>, String> {
    let layout = json["rgb_matrix"]["layout"]
        .as_array()
        .ok_or("no rgb_matrix.layout in this JSON")?;
    Ok(layout
        .iter()
        .map(|e| {
            let x = e["x"].as_u64().unwrap_or(0) as u16;
            let y = e["y"].as_u64().unwrap_or(0) as u16;
            let flags = e["flags"].as_u64().unwrap_or(0) as u8;
            (x, y, flags)
        })
        .collect())
}

/// Fallback for boards whose device-embedded vial.json is missing
/// rgb_matrix.layout (the common case -- see
/// qmk-modules/docs/vial-json-rgb-matrix-layout.md) or that aren't a Vial
/// keyboard at all (plain VIA never transmits it either way). Loads a
/// locally exported keyboard definition instead, e.g. via:
///   qmk info -kb <keyboard> -km <keymap> -f json > keyboard_info.json
pub fn load_led_layout_file(path: &std::path::Path) -> Result<Vec<(u16, u16, u8)>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("invalid JSON: {e}"))?;
    parse_rgb_matrix_layout(&json)
}
