// Audio capture + FFT band extraction. Port of viz_gui.py's VizWorker._calc_frame,
// deliberately reusing `parec` via subprocess instead of a native PipeWire
// binding -- same pragmatic choice the Python version already made, no new
// native audio API to learn for this prototype.
use rustfft::num_complex::Complex32;
use rustfft::FftPlanner;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

const SAMPLE_RATE: f32 = 48000.0;
const FFT_SIZE: usize = 2048;
const AUTO_DECAY: f32 = 0.995;
const AUTO_FLOOR: f32 = 80.0;
const AUTO_LEVEL: f32 = 0.85;
const DECAY: f32 = 15.0;

/// Default PulseAudio/PipeWire sink monitor source, via `pactl info`.
pub fn default_monitor_source() -> Option<String> {
    let out = Command::new("pactl").arg("info").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let sink = text
        .lines()
        .find_map(|l| l.strip_prefix("Default Sink: "))?
        .trim();
    Some(format!("{sink}.monitor"))
}

/// All PulseAudio/PipeWire sources (includes sink monitors), as (name, description).
/// Err surfaces *why* (pactl missing vs. it ran but returned nothing) instead
/// of silently reading as an empty list either way -- callers previously had
/// no way to tell "no PipeWire" from "no sources" from "pactl not installed".
pub fn list_audio_sources() -> Result<Vec<(String, String)>, String> {
    let out = Command::new("pactl").args(["list", "sources"]).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            // Fedora ships PipeWire by default but not its pulse CLI --
            // that's a separate package, unlike Debian/Ubuntu where
            // pipewire-pulse pulls it in automatically.
            "pactl not found -- on Fedora: sudo dnf install pulseaudio-utils".to_string()
        } else {
            format!("pactl not found: {e}")
        }
    })?;
    if !out.status.success() {
        return Err(format!(
            "pactl exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut sources = Vec::new();
    let mut name: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(n) = line.strip_prefix("Name:") {
            name = Some(n.trim().to_string());
        } else if let Some(d) = line.strip_prefix("Description:") {
            if let Some(n) = name.take() {
                sources.push((n, d.trim().to_string()));
            }
        }
    }
    Ok(sources)
}

pub struct AudioBands {
    pub bands: Arc<Mutex<Vec<f32>>>,
    child: Child,
    stop: Arc<Mutex<bool>>,
}

impl AudioBands {
    /// n_bands: how many bands to split the spectrum into -- driven by the
    /// connected board's actual column count (render::column_count), not a
    /// fixed constant.
    pub fn start(monitor_source: &str, n_bands: usize) -> Result<Self, String> {
        let mut child = Command::new("parec")
            .args([
                "-d",
                monitor_source,
                "--format=s16le",
                "--rate=48000",
                "--channels=2",
                "--latency-msec=20",
            ])
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed to start parec: {e}"))?;

        let stdout = child.stdout.take().ok_or("no parec stdout")?;
        let bands = Arc::new(Mutex::new(vec![0f32; n_bands]));
        let stop = Arc::new(Mutex::new(false));

        let bands_thread = bands.clone();
        let stop_thread = stop.clone();
        std::thread::spawn(move || run_capture(stdout, n_bands, bands_thread, stop_thread));

        Ok(Self { bands, child, stop })
    }
}

impl Drop for AudioBands {
    fn drop(&mut self) {
        *self.stop.lock().unwrap() = true;
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn band_edges(n_bands: usize) -> Vec<f32> {
    // geomspace(60, 12000, n_bands + 1)
    let (lo, hi) = (60f32, 12000f32);
    let ratio = (hi / lo).powf(1.0 / n_bands as f32);
    let mut edges = Vec::with_capacity(n_bands + 1);
    let mut v = lo;
    for _ in 0..=n_bands {
        edges.push(v);
        v *= ratio;
    }
    edges
}

fn run_capture(
    mut stdout: impl Read,
    n_bands: usize,
    bands: Arc<Mutex<Vec<f32>>>,
    stop: Arc<Mutex<bool>>,
) {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let edges = band_edges(n_bands);

    let mut auto_max = vec![AUTO_FLOOR; n_bands];
    let mut display = vec![0f32; n_bands];
    let mut raw = vec![0f32; n_bands];

    // s16le stereo -> read enough bytes for FFT_SIZE mono samples
    let mut byte_buf = vec![0u8; FFT_SIZE * 2 * 2]; // frames * channels * i16-bytes

    loop {
        if *stop.lock().unwrap() {
            return;
        }
        if stdout.read_exact(&mut byte_buf).is_err() {
            return; // parec exited/closed
        }

        // Interleaved stereo i16 -> mono f32, raw int16 range (NOT normalized
        // to [-1,1] -- matches viz_gui.py's np.frombuffer(...int16).mean(),
        // AUTO_FLOOR below is tuned to that scale).
        let mut fft_buf: Vec<Complex32> = Vec::with_capacity(FFT_SIZE);
        for i in 0..FFT_SIZE {
            let l = i16::from_le_bytes([byte_buf[i * 4], byte_buf[i * 4 + 1]]) as f32;
            let r = i16::from_le_bytes([byte_buf[i * 4 + 2], byte_buf[i * 4 + 3]]) as f32;
            let mono = (l + r) / 2.0;
            // Hann window
            let w =
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos();
            fft_buf.push(Complex32::new(mono * w, 0.0));
        }

        fft.process(&mut fft_buf);

        let bin_hz = SAMPLE_RATE / FFT_SIZE as f32;
        for (b, raw_b) in raw.iter_mut().enumerate() {
            let lo_bin = (edges[b] / bin_hz).round() as usize;
            let hi_bin = ((edges[b + 1] / bin_hz).round() as usize).max(lo_bin + 1);
            let hi_bin = hi_bin.min(FFT_SIZE / 2);
            // sqrt(mean(|FFT|)) -- magnitude, not power -- matches viz_gui.py exactly.
            let mut sum_abs = 0f32;
            let mut n = 0usize;
            for bin in lo_bin..hi_bin {
                sum_abs += fft_buf[bin].norm();
                n += 1;
            }
            *raw_b = if n > 0 {
                (sum_abs / n as f32).sqrt()
            } else {
                0.0
            };
        }

        for b in 0..n_bands {
            auto_max[b] = (auto_max[b] * AUTO_DECAY).max(raw[b]).max(AUTO_FLOOR);
            let normalized = (raw[b] / auto_max[b] * 255.0 * AUTO_LEVEL).clamp(0.0, 255.0);
            display[b] = if normalized > display[b] {
                normalized
            } else {
                (display[b] - DECAY).max(normalized)
            };
        }

        *bands.lock().unwrap() = display.clone();
    }
}
