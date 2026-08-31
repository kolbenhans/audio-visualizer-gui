// Rust/egui port of viz_gui.py (viz-frame-tools) -- device connect, all 5
// render modes, palette editor + presets, effect switch (KeyColors vs
// Visualizer), audio source selection, and all 3 screen-capture palette
// sources (Color-Shot/WPWatch/Ambient) + monitor picker + gamma slider.
mod audio;
mod hid;
mod palette;
mod render;

use eframe::egui;
use hidapi::HidApi;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const MODE_NAMES: [&str; 5] = ["Bars", "Dots", "Center", "Kitt", "Waterdrop"];

// Continuous text+button scaling for the Effect/Render Mode/Palette Source/
// Palette Colors sections (vs. horizontal_wrapped's discrete "wrap to a new
// line" fallback, which stays in place alongside this as a safety net for
// extreme window widths).
const SCALE_BASE_WIDTH: f32 = 700.0; // reference width = scale 1.0
const SCALE_MIN: f32 = 0.75;
const SCALE_MAX: f32 = 1.3;

// Shared label column width so the Monitor and Gamma rows' controls line up
// and end up the same width (Gamma's slider used to default to a much
// narrower natural width than the Monitor combo+reload row).
const SETTINGS_LABEL_WIDTH: f32 = 130.0;

/// Runs `add_contents` in a scope where all text sizes are multiplied by a
/// scale factor derived from how the current available width compares to
/// `SCALE_BASE_WIDTH`, clamped to [SCALE_MIN, SCALE_MAX]. Callers still need
/// to scale their own button min_size by the same factor (egui's text-style
/// scoping doesn't reach into widget size args).
fn scaled_section(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui, f32)) {
    let scale = (ui.available_width() / SCALE_BASE_WIDTH).clamp(SCALE_MIN, SCALE_MAX);
    ui.scope(|ui| {
        for font_id in ui.style_mut().text_styles.values_mut() {
            font_id.size *= scale;
        }
        add_contents(ui, scale);
    });
}

#[derive(PartialEq, Clone, Copy)]
enum Effect {
    KeyLighting,
    PythonViz,
}

/// State the background render thread reads live, so mode/palette changes
/// apply immediately without restarting -- matches viz_gui.py's
/// VizWorker.set_palette/set_render_mode.
struct Shared {
    palette: Mutex<render::Palette>,
    mode: Mutex<usize>,
}

#[derive(PartialEq, Clone, Copy)]
enum PaletteSourceKind {
    WpWatch,
    Ambient,
}

/// A running WPWatch or Ambient background worker. Color-Shot is a one-shot
/// fire-and-forget thread instead (matches viz_gui.py: only WPWatch/Ambient
/// are toggleable QThreads, Color-Shot is a plain daemon thread).
struct PaletteWorker {
    kind: PaletteSourceKind,
    stop: Arc<Mutex<bool>>,
    child: Option<Arc<Mutex<Option<Child>>>>, // WPWatch's inotifywait, killed on stop to unblock its reader
    thread: Option<JoinHandle<()>>,
}

struct App {
    api: HidApi,
    devices: Vec<hid::DeviceInfo>,
    selected: usize,
    kb: Option<Arc<Mutex<hid::VizKeyboard>>>,
    audio: Option<audio::AudioBands>,
    led_pos: Vec<(u16, u16, u8)>,
    layout: Arc<render::LedLayout>,
    n_bands: usize,
    shared: Arc<Shared>,
    effect: Effect,
    render_mode: usize,
    palette: render::Palette,
    preset_idx: usize,
    audio_sources: Vec<(String, String)>,
    selected_source: usize,
    render_stop: Option<Arc<Mutex<bool>>>,
    render_thread: Option<JoinHandle<()>>,
    status: String,

    // Screen-capture palette sources (Hyprland/grim-specific).
    gamma: f32,
    shared_gamma: Arc<Mutex<f32>>,
    ambient_interval_ms: u32, // GUI-bound; live-adjustable while Ambient runs via shared_ambient_interval_ms
    shared_ambient_interval_ms: Arc<Mutex<u32>>,
    monitors: Vec<(String, String)>, // (hyprctl output name, display label)
    selected_monitor: usize,         // 0 = "All monitors"
    hyprland_available: bool,        // gates Color-Shot/Ambient/monitor picker in the UI
    pending_palette: Arc<Mutex<Option<render::Palette>>>,
    palette_worker: Option<PaletteWorker>,

    // User-editable override for WPWatch's wallpaper state file (Settings menu).
    wallpaper_path: Arc<Mutex<PathBuf>>,
    wallpaper_path_edit: String,
    // WPWatch's background thread has no direct &mut self access, so it
    // reports what it's doing/why it failed through here instead of
    // silently swallowing errors -- polled into self.status each frame.
    pending_wallpaper_status: Arc<Mutex<Option<String>>>,

    // Manual LED-layout override (Settings menu) -- fallback for boards
    // whose vial.json is missing rgb_matrix.layout (the common case, see
    // qmk-modules/docs/vial-json-rgb-matrix-layout.md) or plain-VIA boards
    // that never transmit it at all. Path to a `qmk info -f json` export.
    led_layout_path_edit: String,
}

impl App {
    fn new() -> Self {
        let api = HidApi::new().expect("hidapi init failed");
        let devices = hid::list_devices(&api);
        let n_bands = render::column_count(render::LED_POS_ID75);
        let layout = Arc::new(render::build_layout(render::LED_POS_ID75, n_bands));
        // Surfaced into the initial status line below instead of silently
        // reading as "(none found)" with no clue why -- distinguishes
        // "pactl missing/failed" from "ran fine, zero sources".
        let (audio_sources, audio_note) = match audio::list_audio_sources() {
            Ok(s) => {
                let note = format!("{} audio source(s) found", s.len());
                (s, note)
            }
            Err(e) => (Vec::new(), format!("audio sources unavailable: {e}")),
        };
        let default = audio::default_monitor_source();
        let selected_source = default
            .and_then(|d| audio_sources.iter().position(|(n, _)| *n == d))
            .unwrap_or(0);

        let monitors = palette::list_monitors();
        let monitor_note = if monitors.is_empty() {
            "monitor picker needs Hyprland (hyprctl not found/empty)".to_string()
        } else {
            format!("{} monitor(s) found", monitors.len())
        };

        Self {
            api,
            devices,
            selected: 0,
            kb: None,
            audio: None,
            led_pos: render::LED_POS_ID75.to_vec(),
            layout,
            n_bands,
            shared: Arc::new(Shared {
                palette: Mutex::new(render::DEFAULT_PALETTE),
                mode: Mutex::new(0),
            }),
            effect: Effect::KeyLighting,
            render_mode: 0,
            palette: render::DEFAULT_PALETTE,
            preset_idx: 0,
            audio_sources,
            selected_source,
            render_stop: None,
            render_thread: None,
            status: format!("not connected — {audio_note} — {monitor_note}"),

            gamma: 1.6,
            shared_gamma: Arc::new(Mutex::new(1.6)),
            ambient_interval_ms: 200,
            shared_ambient_interval_ms: Arc::new(Mutex::new(200)),
            monitors,
            hyprland_available: palette::is_hyprland(),
            selected_monitor: 0,
            pending_palette: Arc::new(Mutex::new(None)),
            palette_worker: None,

            wallpaper_path_edit: palette::detect_wallpaper_state_file_path().display().to_string(),
            wallpaper_path: Arc::new(Mutex::new(palette::detect_wallpaper_state_file_path())),
            pending_wallpaper_status: Arc::new(Mutex::new(None)),
            led_layout_path_edit: String::new(),
        }
    }

    fn connect(&mut self) {
        let Some(path) = self.devices.get(self.selected).map(|d| d.path.clone()) else {
            self.status = "no device selected".into();
            return;
        };
        self.stop_bars();
        match hid::VizKeyboard::open(&self.api, &path) {
            Ok(kb) => {
                match kb.get_info() {
                    Ok((ver, max_br)) => {
                        self.status = format!("connected — VialRGB v{ver}, max_brightness={max_br}")
                    }
                    Err(e) => self.status = format!("connected, but get_info failed: {e}"),
                }

                // Read the real LED layout BEFORE activate_*() -- those are
                // fire-and-forget (no read), so doing them first would leave
                // an unread ack sitting in the HID queue for the fetch's own
                // read() to pick up by mistake (garbage size -> bad xz).
                match kb.fetch_led_layout() {
                    Ok(layout) if !layout.is_empty() => {
                        self.n_bands = render::column_count(&layout);
                        self.status = format!(
                            "{} — {} LEDs, {} bands read from device",
                            self.status,
                            layout.len(),
                            self.n_bands
                        );
                        self.layout = Arc::new(render::build_layout(&layout, self.n_bands));
                        self.led_pos = layout;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        // Reset to empty rather than guessing a specific
                        // board's shape (used to hardcode an ID75 fallback
                        // here) -- a guessed layout can look plausible while
                        // being wrong for whatever's actually connected
                        // (wrong LED count/order -> FASTSET frames addressed
                        // to the wrong LEDs, looks like random flicker).
                        // Empty is safe (render.rs treats 0 LEDs as a no-op)
                        // and honest: nothing lights up spatially until the
                        // real layout is loaded via ☰ -> LED layout override.
                        self.n_bands = render::column_count(&[]);
                        self.layout = Arc::new(render::build_layout(&[], self.n_bands));
                        self.led_pos = Vec::new();
                        self.status = format!(
                            "{} (layout fetch failed: {e} — load the real layout via ☰ → LED layout override)",
                            self.status
                        );
                    }
                }

                let _ = kb.activate_key_colors();
                self.effect = Effect::KeyLighting;
                self.kb = Some(Arc::new(Mutex::new(kb)));
            }
            Err(e) if e.contains("Permission denied") => {
                self.status = format!(
                    "connect failed: {e} — missing udev rule, see packaging/70-audio-visualizer-gui-hidraw.rules"
                )
            }
            Err(e) => self.status = format!("connect failed: {e}"),
        }
    }

    fn set_effect(&mut self, effect: Effect) {
        let Some(kb) = self.kb.clone() else { return };
        self.effect = effect;
        match effect {
            Effect::KeyLighting => {
                self.stop_bars();
                if let Ok(kb) = kb.lock() {
                    let _ = kb.activate_key_colors();
                }
            }
            Effect::PythonViz => {
                if let Ok(kb) = kb.lock() {
                    let _ = kb.activate_audio_visualizer();
                }
                self.start_bars();
            }
        }
    }

    fn start_bars(&mut self) {
        let Some(kb) = self.kb.clone() else {
            self.status = "connect first".into();
            return;
        };
        let Some((source, _)) = self.audio_sources.get(self.selected_source).cloned() else {
            self.status = "no audio source selected".into();
            return;
        };
        let audio = match audio::AudioBands::start(&source, self.n_bands) {
            Ok(a) => a,
            Err(e) => {
                self.status = format!("audio start failed: {e}");
                return;
            }
        };
        let bands = audio.bands.clone();
        let layout = self.layout.clone();
        let led_pos = self.led_pos.clone();
        let shared = self.shared.clone();
        let stop = Arc::new(Mutex::new(false));
        let stop_thread = stop.clone();

        let handle = std::thread::spawn(move || {
            let mut waterdrop = render::WaterdropState::new();
            let mut last_mode = usize::MAX;
            loop {
                if *stop_thread.lock().unwrap() {
                    return;
                }
                let snapshot = bands.lock().unwrap().clone();
                let (pal, mode) = {
                    (
                        *shared.palette.lock().unwrap(),
                        *shared.mode.lock().unwrap(),
                    )
                };
                if mode == 4 && last_mode != 4 {
                    waterdrop = render::WaterdropState::new();
                }
                last_mode = mode;

                let mut frame = match mode {
                    1 => render::render_bars(&snapshot, &pal, &layout, true),
                    2 => render::render_center(&snapshot, &pal, &layout, false),
                    3 => render::render_center(&snapshot, &pal, &layout, true),
                    4 => {
                        render::render_waterdrop(&snapshot, &pal, &layout, &led_pos, &mut waterdrop)
                    }
                    _ => render::render_bars(&snapshot, &pal, &layout, false),
                };
                render::apply_mirror(&mut frame, &layout);

                if let Ok(kb) = kb.lock() {
                    let _ = kb.send_frame(&frame);
                }
                std::thread::sleep(Duration::from_millis(33));
            }
        });

        self.render_stop = Some(stop);
        self.render_thread = Some(handle);
        self.status = format!("bars running — capturing {source}");
        self.audio = Some(audio);
    }

    fn stop_bars(&mut self) {
        if let Some(stop) = self.render_stop.take() {
            *stop.lock().unwrap() = true;
        }
        if let Some(h) = self.render_thread.take() {
            let _ = h.join();
        }
        if self.audio.is_some() {
            self.audio = None; // Drop kills the parec child + capture thread
            if let Some(kb) = &self.kb {
                if let Ok(kb) = kb.lock() {
                    let _ = kb.send_frame(&vec![(0, 0, 0); self.layout.led_count]);
                }
            }
            if !self.status.ends_with("(stopped)") {
                self.status = format!("{} (stopped)", self.status);
            }
        }
    }

    fn stop_palette_worker(&mut self) {
        if let Some(w) = self.palette_worker.take() {
            *w.stop.lock().unwrap() = true;
            if let Some(child_slot) = &w.child {
                if let Some(mut c) = child_slot.lock().unwrap().take() {
                    let _ = c.kill();
                }
            }
            if let Some(t) = w.thread {
                let _ = t.join();
            }
        }
    }

    /// One-shot: extract palette from the active monitor. Not a toggle --
    /// matches viz_gui.py's Color-Shot button (plain daemon thread, no
    /// worker state to track).
    fn color_shot(&mut self) {
        let gamma = *self.shared_gamma.lock().unwrap();
        let pending = self.pending_palette.clone();
        std::thread::spawn(move || {
            if let Ok(img) = palette::grab_active_monitor() {
                let pixels = palette::filter_pixels(&palette::image_to_pixels(&img, 96));
                *pending.lock().unwrap() = Some(palette::extract_dominant_palette(&pixels, gamma));
            }
        });
    }

    fn toggle_wpwatch(&mut self) {
        if matches!(self.palette_worker.as_ref().map(|w| w.kind), Some(PaletteSourceKind::WpWatch)) {
            self.stop_palette_worker();
            return;
        }
        self.stop_palette_worker();

        let gamma = self.shared_gamma.clone();
        let pending = self.pending_palette.clone();
        let wp_status = self.pending_wallpaper_status.clone();
        let stop = Arc::new(Mutex::new(false));
        let child_slot: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(None));

        // Snapshot the state-file path for this worker's lifetime -- if the
        // user edits it in the Settings menu while WPWatch is running, that
        // Apply restarts the worker (see wallpaper settings UI below) rather
        // than this thread picking it up mid-flight.
        let state_file = self.wallpaper_path.lock().unwrap().clone();

        let stop_thread = stop.clone();
        let child_thread = child_slot.clone();
        let thread = std::thread::spawn(move || {
            let fire = || match palette::current_wallpaper_path(&state_file) {
                Ok(path) => match palette::load_image_rgb(&path) {
                    Ok(img) => {
                        let g = *gamma.lock().unwrap();
                        *pending.lock().unwrap() = Some(palette::pick_palette(palette::sample_zones(&img), g));
                        *wp_status.lock().unwrap() = Some(format!("WPWatch: reading {}", path.display()));
                    }
                    Err(e) => *wp_status.lock().unwrap() = Some(format!("WPWatch: image load failed: {e}")),
                },
                Err(e) => *wp_status.lock().unwrap() = Some(format!("WPWatch: {e}")),
            };
            fire();

            let Ok(mut child) = Command::new("inotifywait")
                .args(["-m", "-e", "modify,attrib,move,create,delete", &state_file.to_string_lossy()])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            else {
                *wp_status.lock().unwrap() = Some("WPWatch: failed to start inotifywait (is it installed?)".into());
                return;
            };
            let Some(stdout) = child.stdout.take() else { return };
            *child_thread.lock().unwrap() = Some(child);

            for line in BufReader::new(stdout).lines() {
                if line.is_err() || *stop_thread.lock().unwrap() {
                    break;
                }
                fire();
            }
            if let Some(mut c) = child_thread.lock().unwrap().take() {
                let _ = c.kill();
                let _ = c.wait();
            }
        });

        self.palette_worker = Some(PaletteWorker {
            kind: PaletteSourceKind::WpWatch,
            stop,
            child: Some(child_slot),
            thread: Some(thread),
        });
    }

    fn toggle_ambient(&mut self) {
        if matches!(self.palette_worker.as_ref().map(|w| w.kind), Some(PaletteSourceKind::Ambient)) {
            self.stop_palette_worker();
            return;
        }
        self.stop_palette_worker();

        let output = if self.selected_monitor == 0 {
            None
        } else {
            self.monitors.get(self.selected_monitor - 1).map(|(name, _)| name.clone())
        };
        let gamma = self.shared_gamma.clone();
        let interval_ms = self.shared_ambient_interval_ms.clone();
        let pending = self.pending_palette.clone();
        let stop = Arc::new(Mutex::new(false));
        let stop_thread = stop.clone();

        let thread = std::thread::spawn(move || {
            let mut last: Option<render::Palette> = None;
            loop {
                if *stop_thread.lock().unwrap() {
                    return;
                }
                let t0 = std::time::Instant::now();
                if let Ok(img) = palette::capture_tiny(0.15, output.as_deref()) {
                    let g = *gamma.lock().unwrap();
                    let pal = palette::pick_palette(palette::sample_zones(&img), g);
                    let changed = match &last {
                        None => true,
                        Some(l) => palette::palette_distance(l, &pal) >= 8.0,
                    };
                    if changed {
                        last = Some(pal);
                        *pending.lock().unwrap() = Some(pal);
                    }
                }
                let target = Duration::from_millis(*interval_ms.lock().unwrap() as u64);
                let remaining = target.saturating_sub(t0.elapsed());
                // Sleep in short steps (capped so a very low interval doesn't
                // itself become the poll granularity) so Stop reacts quickly
                // instead of blocking through one full interval.
                let step_ms = 50.min(target.as_millis().max(1) as u64);
                let steps = (remaining.as_millis() as u64 / step_ms).max(1);
                for _ in 0..steps {
                    if *stop_thread.lock().unwrap() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(step_ms));
                }
            }
        });

        self.palette_worker = Some(PaletteWorker {
            kind: PaletteSourceKind::Ambient,
            stop,
            child: None,
            thread: Some(thread),
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().request_repaint_after(Duration::from_millis(100));

        // Applied here (not where they're produced) since Color-Shot/WPWatch/
        // Ambient run on background threads -- matches viz_gui.py's
        // _apply_palette, just polled instead of Qt-signal-driven.
        if let Some(pal) = self.pending_palette.lock().unwrap().take() {
            self.palette = pal;
            *self.shared.palette.lock().unwrap() = pal;
        }
        if let Some(msg) = self.pending_wallpaper_status.lock().unwrap().take() {
            self.status = msg;
        }

        // ── Header: ☰ menu on its own row, centered bold title below ────────
        // (the logo used to sit here too -- it's now the background
        // watermark painted just inside the Frame below instead.)
        ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
            // Plain `ui.menu_button` uses egui's deprecated menu system,
            // whose default close behavior is "close on ANY click" --
            // including clicking into a TextEdit inside it just to place
            // the cursor, which made the path fields below unusable. This
            // (non-deprecated) MenuButton API lets us ask for
            // close-outside-only instead.
            egui::containers::menu::MenuButton::new("☰")
                .config(egui::containers::menu::MenuConfig::new().close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside))
                .ui(ui, |ui| {
                    ui.set_min_width(320.0);
                    ui.label("Wallpaper state file (WPWatch)");
                    ui.text_edit_singleline(&mut self.wallpaper_path_edit);
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            let path = palette::expand_home(&self.wallpaper_path_edit);
                            self.wallpaper_path_edit = path.display().to_string();
                            *self.wallpaper_path.lock().unwrap() = path;
                            // Restart WPWatch so inotifywait picks up the new file.
                            if matches!(
                                self.palette_worker.as_ref().map(|w| w.kind),
                                Some(PaletteSourceKind::WpWatch)
                            ) {
                                self.toggle_wpwatch();
                                self.toggle_wpwatch();
                            }
                        }
                        if ui.button("Reset to default").clicked() {
                            let path = palette::detect_wallpaper_state_file_path();
                            self.wallpaper_path_edit = path.display().to_string();
                            *self.wallpaper_path.lock().unwrap() = path;
                        }
                    });

                    ui.separator();
                    ui.label("LED layout override");
                    ui.label(egui::RichText::new(
                        "if the connected board's vial.json is missing rgb_matrix.layout \
                         (common — see qmk-modules/docs), point this at a local export instead:\n\
                         qmk info -kb <keyboard> -km <keymap> -f json > keyboard_info.json",
                    ).weak().small());
                    ui.text_edit_singleline(&mut self.led_layout_path_edit);
                    if ui.button("Load").clicked() {
                        let path = palette::expand_home(&self.led_layout_path_edit);
                        match hid::load_led_layout_file(&path) {
                            Ok(layout) if !layout.is_empty() => {
                                self.n_bands = render::column_count(&layout);
                                self.status = format!(
                                    "LED layout loaded from {} — {} LEDs, {} bands",
                                    path.display(),
                                    layout.len(),
                                    self.n_bands
                                );
                                self.layout = Arc::new(render::build_layout(&layout, self.n_bands));
                                self.led_pos = layout;
                            }
                            Ok(_) => self.status = format!("{}: rgb_matrix.layout is empty", path.display()),
                            Err(e) => self.status = format!("LED layout load failed: {e}"),
                        }
                    }
                });
            });
        ui.vertical_centered(|ui| {
            ui.label(egui::RichText::new("Audiovisualizer").heading().strong());
        });

        egui::Frame::new().inner_margin(16.0).show(ui, |ui| {
            // Logo as a faint centered watermark behind the rest of the
            // content -- painted first so later widgets draw on top of it.
            let rect = ui.max_rect();
            let logo = egui::Image::new(egui::include_image!("../assets/logo.png"))
                .tint(egui::Color32::from_white_alpha(18)); // ~7% opacity, starting point
            let size = rect.size().min_elem() * 0.5;
            let logo_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(size, size));
            logo.paint_at(ui, logo_rect);

            ui.separator();

            // ── Keyboard + Audio source, side by side to save vertical space ───
            ui.columns(2, |cols| {
                cols[0].vertical(|ui| {
                    ui.strong("Keyboard");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("kb")
                            .width(ui.available_width())
                            .wrap_mode(egui::TextWrapMode::Truncate)
                            .selected_text(
                                self.devices
                                    .get(self.selected)
                                    .map(|d| d.label.clone())
                                    .unwrap_or_else(|| "(none found)".into()),
                            )
                            .show_ui(ui, |ui| {
                                for (i, d) in self.devices.iter().enumerate() {
                                    ui.selectable_value(&mut self.selected, i, &d.label);
                                }
                            });
                    });
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Rescan").clicked() {
                            let _ = self.api.refresh_devices();
                            self.devices = hid::list_devices(&self.api);
                        }
                        if ui.button("Connect").clicked() {
                            self.connect();
                        }
                    });
                });

                cols[1].vertical(|ui| {
                    ui.strong("Audio Source");
                    let current = self
                        .audio_sources
                        .get(self.selected_source)
                        .map(|(_, d)| d.clone())
                        .unwrap_or_else(|| "(none found)".into());
                    let mut changed = false;
                    egui::ComboBox::from_id_salt("audio")
                        .width(ui.available_width())
                        .wrap_mode(egui::TextWrapMode::Truncate)
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for (i, (_, desc)) in self.audio_sources.iter().enumerate() {
                                if ui
                                    .selectable_value(&mut self.selected_source, i, desc)
                                    .clicked()
                                {
                                    changed = true;
                                }
                            }
                        });
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("↺ Reload").clicked() {
                            match audio::list_audio_sources() {
                                Ok(s) => {
                                    self.status = format!("{} audio source(s) found", s.len());
                                    self.audio_sources = s;
                                }
                                Err(e) => self.status = format!("audio sources unavailable: {e}"),
                            }
                        }
                    });
                    if changed && self.render_thread.is_some() {
                        self.stop_bars();
                        self.start_bars();
                    }
                });
            });

            ui.separator();

            // ── Effect ───────────────────────────────────────────────────────
            // scaled_section: text+button size scale continuously with window
            // width (clamped, see SCALE_MIN/MAX). horizontal_wrapped stays as
            // a fallback for extreme widths where even scaled-down buttons
            // don't all fit on one line.
            ui.strong("Effect");
            ui.add_enabled_ui(self.kb.is_some(), |ui| {
                scaled_section(ui, |ui, scale| {
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add(egui::Button::new("KeyColors").min_size(egui::vec2(100.0 * scale, 28.0 * scale)).selected(self.effect == Effect::KeyLighting))
                            .clicked()
                        {
                            self.set_effect(Effect::KeyLighting);
                        }
                        if ui
                            .add(egui::Button::new("Visualizer").min_size(egui::vec2(100.0 * scale, 28.0 * scale)).selected(self.effect == Effect::PythonViz))
                            .clicked()
                        {
                            self.set_effect(Effect::PythonViz);
                        }
                    });
                });
            });

            ui.separator();

            // ── Palette ──────────────────────────────────────────────────────
            ui.horizontal(|ui| {
                ui.strong("Palette");
                ui.label("(click swatch to edit)");
                ui.label("Preset:");
                let preset_name = render::PRESETS[self.preset_idx].0;
                egui::ComboBox::from_id_salt("preset")
                    .selected_text(preset_name)
                    .show_ui(ui, |ui| {
                        for (i, (name, pal)) in render::PRESETS.iter().enumerate() {
                            if ui
                                .selectable_value(&mut self.preset_idx, i, *name)
                                .clicked()
                            {
                                self.palette = *pal;
                                *self.shared.palette.lock().unwrap() = self.palette;
                            }
                        }
                    });
            });
            scaled_section(ui, |ui, _scale| {
                ui.horizontal_wrapped(|ui| {
                    let level_labels = ["Low (0)", "1", "2", "3", "High (4)"];
                    for (i, label) in level_labels.iter().enumerate() {
                        ui.vertical(|ui| {
                            let (r, g, b) = self.palette[i];
                            let mut rgb = [r, g, b];
                            if egui::color_picker::color_edit_button_srgb(ui, &mut rgb).changed() {
                                self.palette[i] = (rgb[0], rgb[1], rgb[2]);
                                *self.shared.palette.lock().unwrap() = self.palette;
                            }
                            ui.label(*label);
                        });
                    }
                });
            });

            ui.separator();

            // ── Render mode ──────────────────────────────────────────────────
            ui.strong("Render Mode");
            scaled_section(ui, |ui, scale| {
                ui.horizontal_wrapped(|ui| {
                    for (i, name) in MODE_NAMES.iter().enumerate() {
                        if ui
                            .add(egui::Button::new(*name).min_size(egui::vec2(90.0 * scale, 28.0 * scale)).selected(self.render_mode == i))
                            .clicked()
                        {
                            self.render_mode = i;
                            *self.shared.mode.lock().unwrap() = i;
                        }
                    }
                });
            });

            ui.separator();

            // ── Palette source ──────────────────────────────────────────────
            ui.strong("Palette Source");
            scaled_section(ui, |ui, scale| {
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(
                            self.hyprland_available,
                            egui::Button::new("Color-Shot").min_size(egui::vec2(100.0 * scale, 28.0 * scale)),
                        )
                        .on_disabled_hover_text("needs Hyprland (hyprctl/grim)")
                        .clicked()
                    {
                        self.color_shot();
                    }
                    let wpwatch_on = matches!(self.palette_worker.as_ref().map(|w| w.kind), Some(PaletteSourceKind::WpWatch));
                    if ui
                        .add(egui::Button::new("WPWatch").min_size(egui::vec2(100.0 * scale, 28.0 * scale)).selected(wpwatch_on))
                        .clicked()
                    {
                        self.toggle_wpwatch();
                    }
                    let ambient_on = matches!(self.palette_worker.as_ref().map(|w| w.kind), Some(PaletteSourceKind::Ambient));
                    if ui
                        .add_enabled(
                            self.hyprland_available,
                            egui::Button::new("Ambient").selected(ambient_on).min_size(egui::vec2(100.0 * scale, 28.0 * scale)),
                        )
                        .on_disabled_hover_text("needs Hyprland (hyprctl/grim)")
                        .clicked()
                    {
                        self.toggle_ambient();
                    }
                });
            });

            ui.add_enabled_ui(self.hyprland_available, |ui| {
                ui.horizontal(|ui| {
                    ui.add_sized([SETTINGS_LABEL_WIDTH, 0.0], egui::Label::new("Monitor (Ambient)"));
                    let current = if self.selected_monitor == 0 {
                        "All monitors".to_string()
                    } else {
                        self.monitors.get(self.selected_monitor - 1).map(|(_, l)| l.clone()).unwrap_or_default()
                    };
                    let ctrl_width = ui.available_width() - 40.0; // room for the ↺ button
                    egui::ComboBox::from_id_salt("monitor")
                        .width(ctrl_width)
                        .wrap_mode(egui::TextWrapMode::Truncate)
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.selected_monitor, 0, "All monitors");
                            for (i, (_, label)) in self.monitors.iter().enumerate() {
                                ui.selectable_value(&mut self.selected_monitor, i + 1, label);
                            }
                        });
                    if ui.button("↺").clicked() {
                        self.monitors = palette::list_monitors();
                    }
                });
                ui.horizontal(|ui| {
                    ui.add_sized([SETTINGS_LABEL_WIDTH, 0.0], egui::Label::new("Capture Interval"));
                    ui.spacing_mut().slider_width = ui.available_width() - 60.0;
                    if ui
                        .add(egui::Slider::new(&mut self.ambient_interval_ms, 50..=1000).suffix("ms"))
                        .changed()
                    {
                        *self.shared_ambient_interval_ms.lock().unwrap() = self.ambient_interval_ms;
                    }
                });
            })
            .response
            .on_disabled_hover_text("needs Hyprland (hyprctl)");

            ui.horizontal(|ui| {
                ui.add_sized([SETTINGS_LABEL_WIDTH, 0.0], egui::Label::new("Gamma"));
                // Slider has no per-widget width builder -- its rail width
                // comes from the style's slider_width, so stretch that to
                // match the Monitor row's control width for this one row.
                ui.spacing_mut().slider_width = ui.available_width() - 60.0; // room for the value text
                if ui.add(egui::Slider::new(&mut self.gamma, 0.8..=3.0)).changed() {
                    *self.shared_gamma.lock().unwrap() = self.gamma;
                }
            });

            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.weak(concat!("v", env!("CARGO_PKG_VERSION")));
                ui.weak("|");
                ui.label(&self.status);
            });
        }); // Frame::inner_margin
    }
}

fn main() -> eframe::Result<()> {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/logo.png"))
        .expect("bundled logo.png should decode");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([780.0, 620.0])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        concat!("Audiovisualizer v", env!("CARGO_PKG_VERSION")),
        options,
        Box::new(|cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            // egui's default widget sizes are noticeably smaller than Qt's --
            // bump the whole UI's scale instead of hand-tuning every widget.
            cc.egui_ctx.set_pixels_per_point(1.3);
            cc.egui_ctx.global_style_mut(|s| {
                s.spacing.item_spacing = egui::vec2(8.0, 8.0);
                s.spacing.button_padding = egui::vec2(10.0, 6.0);
            });
            Ok(Box::new(App::new()))
        }),
    )
}
