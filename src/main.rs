mod engine;
mod subtitle;
mod updater;

use eframe::egui;
use engine::AlignmentResult;
use std::{
    env,
    path::PathBuf,
    sync::mpsc,
    time::Duration,
};
use updater::{AppVersion, UpdateInfo, UpdateUrgency};

fn current_app_version() -> AppVersion {
    updater::AppVersion::with_commit_count_minor(
        env!("CARGO_PKG_VERSION"),
        env!("CHRONOSUB_COMMIT_COUNT"),
    )
    .unwrap_or_else(|| AppVersion::parse(env!("CARGO_PKG_VERSION")).unwrap_or(AppVersion {
        major: 0,
        minor: 0,
        patch: 0,
    }))
}

fn parse_repository_owner_and_name() -> Option<(String, String)> {
    let repo_url = env!("CARGO_PKG_REPOSITORY");
    let parts: Vec<&str> = repo_url.trim_end_matches('/').split('/').collect();
    if parts.len() < 2 {
        return None;
    }
    let owner = parts[parts.len() - 2].to_string();
    let repo = parts[parts.len() - 1].to_string();
    Some((owner, repo))
}

fn version_string(version: AppVersion) -> String {
    format!("{}.{}.{}", version.major, version.minor, version.patch)
}

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

struct ChronoSubApp {
    video_path: Option<PathBuf>,
    sub_path: Option<PathBuf>,
    status: String,
    offset_found: Option<f32>,
    audio_energy: Vec<f32>,
    sub_signal: Vec<f32>,
    processing: bool,
    result_rx: Option<mpsc::Receiver<Result<AlignmentResult, String>>>,
    save_status: Option<String>,
    current_version: AppVersion,
    update_rx: Option<mpsc::Receiver<Result<UpdateInfo, String>>>,
    available_update: Option<UpdateInfo>,
    update_error: Option<String>,
}

impl Default for ChronoSubApp {
    fn default() -> Self {
        let current_version = current_app_version();
        Self {
            video_path: None,
            sub_path: None,
            status: "Drag & drop a video file and an SRT subtitle file here.".to_string(),
            offset_found: None,
            audio_energy: Vec::new(),
            sub_signal: Vec::new(),
            processing: false,
            result_rx: None,
            save_status: None,
            current_version,
            update_rx: parse_repository_owner_and_name().map(|(owner, repo)| {
                updater::spawn_update_check(
                    owner,
                    repo,
                    current_version,
                    env!("CARGO_PKG_NAME").to_string(),
                    env!("CARGO_PKG_VERSION").to_string(),
                )
            }),
            available_update: None,
            update_error: None,
        }
    }
}

// ---------------------------------------------------------------------------
// eframe::App impl
// ---------------------------------------------------------------------------

impl eframe::App for ChronoSubApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // ----------------------------------------------------------------
        // 1. Poll the background thread for results
        // ----------------------------------------------------------------
        if let Some(ref rx) = self.result_rx {
            match rx.try_recv() {
                Ok(Ok(result)) => {
                    self.offset_found = Some(result.offset_secs);
                    self.audio_energy = result.audio_energy;
                    self.sub_signal = result.sub_signal;
                    self.status = format!(
                        "Synchronization complete! Detected offset: {:.3} s",
                        result.offset_secs
                    );
                    self.processing = false;
                    self.result_rx = None;
                }
                Ok(Err(e)) => {
                    self.status = format!("Error: {}", e);
                    self.processing = false;
                    self.result_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // Still processing — request a repaint so we keep polling
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.status = "Processing thread disconnected unexpectedly.".to_string();
                    self.processing = false;
                    self.result_rx = None;
                }
            }
        }

        if let Some(ref rx) = self.update_rx {
            match rx.try_recv() {
                Ok(Ok(update)) => {
                    if update.urgency != UpdateUrgency::None {
                        self.available_update = Some(update);
                    }
                    self.update_error = None;
                    self.update_rx = None;
                }
                Ok(Err(e)) => {
                    self.update_error = Some(e);
                    self.update_rx = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.update_error = Some("Update checker disconnected unexpectedly.".to_string());
                    self.update_rx = None;
                }
            }
        }

        // ----------------------------------------------------------------
        // 2. Handle drag-and-drop
        // ----------------------------------------------------------------
        if !ctx.input(|i| i.raw.dropped_files.is_empty()) {
            for file in ctx.input(|i| i.raw.dropped_files.clone()) {
                if let Some(path) = file.path {
                    let ext = path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or_default()
                        .to_lowercase();
                    match ext.as_str() {
                        "mp4" | "mkv" | "avi" | "mov" | "webm" | "m4v" => {
                            self.video_path = Some(path);
                            self.status =
                                "Video file loaded. Drop an SRT file to continue.".to_string();
                        }
                        "srt" => {
                            self.sub_path = Some(path);
                            self.status =
                                "Subtitle file loaded. Drop a video file to continue.".to_string();
                        }
                        _ => {}
                    }
                }
            }
            // If both are loaded, update status
            if self.video_path.is_some() && self.sub_path.is_some() {
                self.status =
                    "Both files loaded. Click ⚡ Synchronize Subtitles to begin.".to_string();
            }
        }

        // ----------------------------------------------------------------
        // 3. Render UI
        // ----------------------------------------------------------------
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("ChronoSub ⚡");
            ui.label("Blazing-fast subtitle synchronization — no Python, no FFmpeg.");
            ui.label(format!(
                "Version {}",
                version_string(self.current_version)
            ));
            if let Some(update) = &self.available_update {
                let urgency_text = match update.urgency {
                    UpdateUrgency::Major => "Major update available",
                    UpdateUrgency::Minor => "Significant update available",
                    UpdateUrgency::None => "Up to date",
                };
                ui.colored_label(
                    if update.urgency == UpdateUrgency::Major {
                        egui::Color32::RED
                    } else {
                        egui::Color32::YELLOW
                    },
                    format!(
                        "{urgency_text}: {}",
                        version_string(update.latest_version)
                    ),
                );
                ui.label(&update.instructions);
                ui.hyperlink_to("Open release page", &update.html_url);
            } else if self.update_rx.is_some() {
                ui.label("Checking for updates…");
            } else if let Some(err) = &self.update_error {
                ui.label(format!("Update check unavailable: {err}"));
            }
            ui.separator();

            // File status
            egui::Grid::new("files_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("🎬 Video:");
                    if let Some(ref p) = self.video_path {
                        ui.label(
                            egui::RichText::new(
                                p.file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .as_ref(),
                            )
                            .color(egui::Color32::LIGHT_GREEN),
                        );
                    } else {
                        ui.label(egui::RichText::new("Not loaded").color(egui::Color32::GRAY));
                    }
                    ui.end_row();

                    ui.label("📝 Subtitles:");
                    if let Some(ref p) = self.sub_path {
                        ui.label(
                            egui::RichText::new(
                                p.file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .as_ref(),
                            )
                            .color(egui::Color32::LIGHT_GREEN),
                        );
                    } else {
                        ui.label(egui::RichText::new("Not loaded").color(egui::Color32::GRAY));
                    }
                    ui.end_row();
                });

            ui.add_space(12.0);

            // Action button
            let can_run = self.video_path.is_some()
                && self.sub_path.is_some()
                && !self.processing;

            ui.add_enabled_ui(can_run, |ui| {
                if ui
                    .button(egui::RichText::new("⚡ Synchronize Subtitles").size(16.0))
                    .clicked()
                {
                    self.start_alignment(ctx.clone());
                }
            });

            ui.add_space(8.0);

            // Status line
            if self.processing {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(&self.status);
                });
            } else {
                ui.label(&self.status);
            }

            // ----------------------------------------------------------------
            // Result panel
            // ----------------------------------------------------------------
            if let Some(offset) = self.offset_found {
                ui.add_space(16.0);
                ui.separator();

                ui.group(|ui| {
                    ui.label(
                        egui::RichText::new(format!("Detected Offset: {:.3} seconds", offset))
                            .color(egui::Color32::GREEN)
                            .size(18.0)
                            .strong(),
                    );
                    if offset > 0.0 {
                        ui.label("Subtitles are ahead of the audio → will be shifted back.");
                    } else if offset < 0.0 {
                        ui.label("Subtitles are behind the audio → will be advanced.");
                    } else {
                        ui.label("Subtitles are already in sync.");
                    }
                });

                ui.add_space(8.0);

                // Waveform visualisation
                if !self.audio_energy.is_empty() {
                    ui.label("Audio energy vs subtitle timing (green = audio, blue = subtitle):");
                    self.draw_waveform(ui);
                    ui.add_space(8.0);
                }

                // Save button
                let can_save =
                    self.sub_path.is_some() && !self.processing;
                ui.add_enabled_ui(can_save, |ui| {
                    if ui.button("💾 Save Synced SRT").clicked() {
                        self.save_synced_srt();
                    }
                });

                if let Some(ref msg) = self.save_status {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(msg)
                            .color(if msg.starts_with("Saved") {
                                egui::Color32::GREEN
                            } else {
                                egui::Color32::RED
                            }),
                    );
                }
            }

            // Drop-zone hint when nothing loaded
            if self.video_path.is_none() || self.sub_path.is_none() {
                ui.add_space(20.0);
                let drop_rect = ui.allocate_space(egui::vec2(ui.available_width(), 60.0)).1;
                let painter = ui.painter_at(drop_rect);
                painter.rect_stroke(
                    drop_rect,
                    8.0,
                    egui::Stroke::new(1.5, egui::Color32::from_gray(100)),
                );
                painter.text(
                    drop_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "⬇  Drop video & subtitle files here",
                    egui::FontId::proportional(14.0),
                    egui::Color32::from_gray(140),
                );
            }
        });

        if let Some(update) = &self.available_update {
            if update.urgency == UpdateUrgency::Major {
                egui::Window::new("Major update required")
                    .collapsible(false)
                    .resizable(false)
                    .show(ctx, |ui| {
                        ui.label(format!(
                            "A major release ({}) is available and contains critical changes that should be installed.",
                            version_string(update.latest_version)
                        ));
                        ui.label(&update.instructions);
                        ui.hyperlink_to("Open release page", &update.html_url);
                    });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helper methods
// ---------------------------------------------------------------------------

impl ChronoSubApp {
    /// Spawn a background thread to run the alignment pipeline and store the
    /// receiving end so `update()` can poll for the result.
    fn start_alignment(&mut self, ctx: egui::Context) {
        let video_path = self.video_path.clone().unwrap();
        let sub_path = self.sub_path.clone().unwrap();

        let (tx, rx) = mpsc::channel();
        self.result_rx = Some(rx);
        self.processing = true;
        self.offset_found = None;
        self.audio_energy.clear();
        self.sub_signal.clear();
        self.save_status = None;
        self.status = "Analysing audio with Symphonia & computing FFT cross-correlation…"
            .to_string();

        std::thread::spawn(move || {
            let result = engine::run_alignment(&video_path, &sub_path);
            let _ = tx.send(result);
            // Wake the UI
            ctx.request_repaint();
        });
    }

    /// Apply the detected offset and write `<name>_synced.srt` beside the
    /// original subtitle file.
    fn save_synced_srt(&mut self) {
        let offset = match self.offset_found {
            Some(o) => o,
            None => return,
        };
        let sub_path = match self.sub_path.as_deref() {
            Some(p) => p,
            None => return,
        };
        let out_path = engine::default_output_path(sub_path);
        match engine::apply_offset_and_save(sub_path, offset, &out_path) {
            Ok(()) => {
                self.save_status = Some(format!(
                    "Saved → {}",
                    out_path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                ));
            }
            Err(e) => {
                self.save_status = Some(format!("Save failed: {}", e));
            }
        }
    }

    /// Draw a simple waveform overview of the audio energy and subtitle signal.
    fn draw_waveform(&self, ui: &mut egui::Ui) {
        const HEIGHT: f32 = 80.0;
        const PREVIEW_SAMPLES: usize = 2000;

        let available_width = ui.available_width();
        let (_, rect) = ui.allocate_space(egui::vec2(available_width, HEIGHT));
        let painter = ui.painter_at(rect);

        // Background
        painter.rect_filled(rect, 4.0, egui::Color32::from_gray(20));

        let audio = &self.audio_energy;
        let subs = &self.sub_signal;
        let total = audio.len().max(subs.len());
        if total == 0 {
            return;
        }

        // Downsample to at most PREVIEW_SAMPLES bars
        let step = ((total as f32 / available_width).ceil() as usize).max(1);
        let bar_count = (total / step).min(PREVIEW_SAMPLES);
        if bar_count == 0 {
            return;
        }
        let bar_w = available_width / bar_count as f32;

        for i in 0..bar_count {
            let idx = i * step;
            let x = rect.left() + i as f32 * bar_w;

            // Audio energy bar (green)
            if let Some(&e) = audio.get(idx) {
                let bar_h = e * HEIGHT;
                let bar_rect = egui::Rect::from_min_size(
                    egui::pos2(x, rect.bottom() - bar_h),
                    egui::vec2(bar_w.max(1.0), bar_h),
                );
                painter.rect_filled(bar_rect, 0.0, egui::Color32::from_rgba_unmultiplied(40, 200, 80, 180));
            }

            // Subtitle timing overlay (blue)
            if let Some(&s) = subs.get(idx) {
                if s > 0.0 {
                    let marker_h = HEIGHT * 0.25;
                    let marker_rect = egui::Rect::from_min_size(
                        egui::pos2(x, rect.top()),
                        egui::vec2(bar_w.max(1.0), marker_h),
                    );
                    painter.rect_filled(
                        marker_rect,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(60, 120, 255, 200),
                    );
                }
            }
        }

        // Border
        painter.rect_stroke(rect, 4.0, egui::Stroke::new(1.0, egui::Color32::from_gray(60)));
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() -> eframe::Result<()> {
    if env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("{}", version_string(current_app_version()));
        return Ok(());
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([600.0, 480.0])
            .with_title("ChronoSub — Subtitle Synchronizer")
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "ChronoSub — Subtitle Synchronizer",
        options,
        Box::new(|_cc| Box::new(ChronoSubApp::default())),
    )
}
