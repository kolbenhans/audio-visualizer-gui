// Audio capture + FFT band extraction. Port of viz_gui.py's VizWorker._calc_frame,
// deliberately reusing `parec` via subprocess instead of a native PipeWire
// binding -- same pragmatic choice the Python version already made, no new
// native audio API to learn for this prototype.
//
// Windows has no `parec`/PulseAudio, so that platform captures via WASAPI
// loopback (wasapi crate) instead -- see the `windows` submodule below. Both
// paths converge on the same `run_capture` (it only needs an `impl Read`
// yielding interleaved s16le stereo bytes), so the FFT/banding logic here is
// written once and shared.
use rustfft::num_complex::Complex32;
use rustfft::FftPlanner;
use std::io::Read;
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::process::{Child, Command, Stdio};

const SAMPLE_RATE: f32 = 48000.0;
const FFT_SIZE: usize = 2048;
const AUTO_DECAY: f32 = 0.995;
const AUTO_FLOOR: f32 = 80.0;
const AUTO_LEVEL: f32 = 0.85;
const DECAY: f32 = 15.0;

#[cfg(windows)]
mod win {
    // WASAPI loopback capture. Requests f32 stereo at the same SAMPLE_RATE
    // the FFT side assumes, with `autoconvert: true` so WASAPI's own shared-
    // mode resampler/mixer handles whatever the device's real mix format is
    // -- same trick the crate's own examples rely on. Converts to i16le
    // stereo bytes on the fly so the *rest* of the pipeline (run_capture)
    // doesn't need to know two audio backends exist.
    use super::SAMPLE_RATE;
    use std::io::Read;
    use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
    use std::sync::{Arc, Mutex};
    use wasapi::{Direction, SampleType, StreamMode, WaveFormat};

    /// Render (playback) device id -> friendly name, i.e. the things you can
    /// loopback-capture from. Mirrors list_audio_sources' (name, description).
    pub fn list_audio_sources() -> Result<Vec<(String, String)>, String> {
        let enumerator =
            wasapi::DeviceEnumerator::new().map_err(|e| format!("wasapi init failed: {e}"))?;
        let collection = enumerator
            .get_device_collection(&Direction::Render)
            .map_err(|e| format!("enumerating render devices failed: {e}"))?;
        let n = collection
            .get_nbr_devices()
            .map_err(|e| format!("device count failed: {e}"))?;
        let mut out = Vec::new();
        for i in 0..n {
            let dev = collection
                .get_device_at_index(i)
                .map_err(|e| format!("device {i} failed: {e}"))?;
            let id = dev.get_id().unwrap_or_default();
            let name = dev.get_friendlyname().unwrap_or_else(|_| id.clone());
            out.push((id, name));
        }
        Ok(out)
    }

    pub fn default_monitor_source() -> Option<String> {
        let enumerator = wasapi::DeviceEnumerator::new().ok()?;
        let dev = enumerator.get_default_device(&Direction::Render).ok()?;
        dev.get_id().ok()
    }

    /// A pipe-like `Read` fed by a background capture thread, so the shared
    /// `run_capture` (byte-oriented, backend-agnostic) can consume it exactly
    /// like `parec`'s stdout.
    pub struct ChannelReader {
        rx: Receiver<Vec<u8>>,
        leftover: Vec<u8>,
        pos: usize,
    }

    impl Read for ChannelReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.leftover.len() {
                match self.rx.recv() {
                    Ok(chunk) => {
                        self.leftover = chunk;
                        self.pos = 0;
                    }
                    Err(_) => return Ok(0), // capture thread gone
                }
            }
            let n = (self.leftover.len() - self.pos).min(buf.len());
            buf[..n].copy_from_slice(&self.leftover[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    /// Spawns the loopback capture thread and returns a Read end for it.
    /// `device_id`: empty = default render device, else a `get_id()` string.
    pub fn start_loopback(
        device_id: &str,
        stop: Arc<Mutex<bool>>,
    ) -> Result<ChannelReader, String> {
        let (tx, rx) = sync_channel::<Vec<u8>>(8);
        let device_id = device_id.to_string();
        std::thread::spawn(move || {
            if let Err(e) = capture_loop(&device_id, tx, stop) {
                eprintln!("wasapi loopback capture failed: {e}");
            }
        });
        Ok(ChannelReader {
            rx,
            leftover: Vec::new(),
            pos: 0,
        })
    }

    fn capture_loop(
        device_id: &str,
        tx: SyncSender<Vec<u8>>,
        stop: Arc<Mutex<bool>>,
    ) -> Result<(), String> {
        wasapi::initialize_mta().ok().map_err(|e| format!("{e:?}"))?;

        let enumerator =
            wasapi::DeviceEnumerator::new().map_err(|e| format!("wasapi init failed: {e}"))?;
        let device = if device_id.is_empty() {
            enumerator
                .get_default_device(&Direction::Render)
                .map_err(|e| format!("no default render device: {e}"))?
        } else {
            enumerator
                .get_device(device_id)
                .map_err(|e| format!("device {device_id} not found: {e}"))?
        };

        let mut audio_client = device
            .get_iaudioclient()
            .map_err(|e| format!("get_iaudioclient failed: {e}"))?;
        let desired_format = WaveFormat::new(32, 32, &SampleType::Float, SAMPLE_RATE as usize, 2, None);
        let (_def_time, min_time) = audio_client
            .get_device_period()
            .map_err(|e| format!("get_device_period failed: {e}"))?;
        let mode = StreamMode::EventsShared {
            autoconvert: true,
            buffer_duration_hns: min_time,
        };
        // direction here means "what we want to do" (Capture); the crate
        // sees the *device* is Render and turns on AUDCLNT_STREAMFLAGS_LOOPBACK
        // for us -- that's what actually makes this a loopback capture.
        audio_client
            .initialize_client(&desired_format, &Direction::Capture, &mode)
            .map_err(|e| format!("initialize_client failed: {e}"))?;
        let h_event = audio_client
            .set_get_eventhandle()
            .map_err(|e| format!("set_get_eventhandle failed: {e}"))?;
        let capture_client = audio_client
            .get_audiocaptureclient()
            .map_err(|e| format!("get_audiocaptureclient failed: {e}"))?;

        audio_client
            .start_stream()
            .map_err(|e| format!("start_stream failed: {e}"))?;

        let mut sample_queue: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
        loop {
            if *stop.lock().unwrap() {
                let _ = audio_client.stop_stream();
                return Ok(());
            }
            capture_client
                .read_from_device_to_deque(&mut sample_queue)
                .map_err(|e| format!("read_from_device failed: {e}"))?;

            // f32 stereo (8 bytes/frame) -> i16le stereo (4 bytes/frame),
            // same scale run_capture already expects from parec's s16le.
            let frame_bytes = 8;
            let n_frames = sample_queue.len() / frame_bytes;
            if n_frames > 0 {
                let mut out = Vec::with_capacity(n_frames * 4);
                for _ in 0..n_frames {
                    let bytes: Vec<u8> = (0..frame_bytes)
                        .map(|_| sample_queue.pop_front().unwrap())
                        .collect();
                    let l = f32::from_le_bytes(bytes[0..4].try_into().unwrap());
                    let r = f32::from_le_bytes(bytes[4..8].try_into().unwrap());
                    out.extend_from_slice(&((l.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
                    out.extend_from_slice(&((r.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
                }
                if tx.send(out).is_err() {
                    let _ = audio_client.stop_stream();
                    return Ok(()); // reader dropped
                }
            }

            if h_event.wait_for_event(3000).is_err() {
                let _ = audio_client.stop_stream();
                return Err("wasapi event wait timed out".to_string());
            }
        }
    }
}

/// Default PulseAudio/PipeWire sink monitor source, via `pactl info`.
#[cfg(unix)]
pub fn default_monitor_source() -> Option<String> {
    let out = Command::new("pactl").arg("info").output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let sink = text
        .lines()
        .find_map(|l| l.strip_prefix("Default Sink: "))?
        .trim();
    Some(format!("{sink}.monitor"))
}

#[cfg(windows)]
pub use win::default_monitor_source;

/// All PulseAudio/PipeWire sources (includes sink monitors), as (name, description).
/// Err surfaces *why* (pactl missing vs. it ran but returned nothing) instead
/// of silently reading as an empty list either way -- callers previously had
/// no way to tell "no PipeWire" from "no sources" from "pactl not installed".
#[cfg(unix)]
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

#[cfg(windows)]
pub use win::list_audio_sources;

pub struct AudioBands {
    pub bands: Arc<Mutex<Vec<f32>>>,
    #[cfg(unix)]
    child: Child,
    stop: Arc<Mutex<bool>>,
}

impl AudioBands {
    /// n_bands: how many bands to split the spectrum into -- driven by the
    /// connected board's actual column count (render::column_count), not a
    /// fixed constant. `monitor_source`: a name from list_audio_sources()
    /// (a pactl source name on Linux, a wasapi device id on Windows).
    #[cfg(unix)]
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

    #[cfg(windows)]
    pub fn start(monitor_source: &str, n_bands: usize) -> Result<Self, String> {
        let bands = Arc::new(Mutex::new(vec![0f32; n_bands]));
        let stop = Arc::new(Mutex::new(false));

        let reader = win::start_loopback(monitor_source, stop.clone())?;
        let bands_thread = bands.clone();
        let stop_thread = stop.clone();
        std::thread::spawn(move || run_capture(reader, n_bands, bands_thread, stop_thread));

        Ok(Self { bands, stop })
    }
}

impl Drop for AudioBands {
    fn drop(&mut self) {
        *self.stop.lock().unwrap() = true;
        #[cfg(unix)]
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
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
