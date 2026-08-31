// VialRGB FASTSET protocol — port of python/viz_hid.py (viz-frame-tools).
use hidapi::{HidApi, HidDevice};

const USAGE_PAGE: u16 = 0xFF60;
const USAGE: u16 = 0x61;
const LEDS_PER_PKT: usize = 9;

const VIA_LIGHTING_SET: u8 = 0x07;
const VIA_LIGHTING_GET: u8 = 0x08;
const GET_INFO: u8 = 0x40;
const FASTSET: u8 = 0x42;

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

    fn wr(&self, data: &[u8]) -> Result<[u8; 32], String> {
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
