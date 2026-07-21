//! Floppy disk visualizer window backed by the Fluxfox visualization API
//!
//! Note: We deliberately do not depend on Fluxfox's `ff_egui_lib` to
//! avoid egui version conflict nightmares.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Result;
use eframe::egui;
use snow_floppy::fluxfox::floppy_image_to_fluxfox;
use snow_floppy::{Floppy, FloppyImage, FloppyType};

use fluxfox::DiskImage;
use fluxfox::visualization::prelude::*;
use fluxfox::visualization::{
    CommonVizParams, RenderMaskType, RenderRasterizationParams, RenderTrackDataParams,
    RenderTrackMetadataParams, TurningDirection, rasterize_track_data, render_track_mask,
};
use fluxfox_tiny_skia::render_display_list::render_display_list;
use fluxfox_tiny_skia::styles::default_skia_styles;
use tiny_skia::{Paint, Pixmap};

/// On-screen side length of each rendered disk in pixels.
const DISPLAY_SIZE: u32 = 480;

/// How often we re-request a snapshot from the emulator while "Live" is on.
const LIVE_REFRESH_INTERVAL: Duration = Duration::from_millis(500);

/// Inner radius ratio used by both the visualizer rendering and the head
/// overlay. Must match the value passed to Fluxfox's `CommonVizParams`.
const MIN_RADIUS_RATIO: f32 = 0.30;

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct LayerFlags {
    pub data: bool,
    pub metadata: bool,
    pub weak: bool,
    pub errors: bool,
}

impl Default for LayerFlags {
    fn default() -> Self {
        Self {
            data: true,
            metadata: true,
            weak: false,
            errors: false,
        }
    }
}

#[derive(Clone, Debug)]
enum Source {
    /// Live drive in the emulator. Refresh requests will be issued.
    Drive(usize),
    /// One-shot file preview. No refresh polling.
    File(PathBuf),
    /// Nothing selected
    None,
}

struct RenderRequest {
    image: Box<FloppyImage>,
    layers: LayerFlags,
    size: u32,
    /// Cloned so the worker can wake the UI when a side is ready.
    ctx: egui::Context,
}

/// Shared single-slot mailbox between the UI and the render worker.
#[derive(Default)]
struct WorkerSlot {
    request: Mutex<Option<RenderRequest>>,
    request_cv: Condvar,
    results: Mutex<[Option<egui::ColorImage>; 2]>,
}

/// Non-modal floppy disk visualizer window.
pub struct FloppyVisualizerDialog {
    open: bool,
    source: Source,
    /// Latest image we have for the current source. Owned so we can
    /// re-render when the user toggles layers without round-tripping the
    /// emulator.
    image: Option<Box<FloppyImage>>,
    layers: LayerFlags,
    /// Per-side textures from the most recent successful render.
    side_textures: [Option<egui::TextureHandle>; 2],
    /// Last drive head track per side
    head_track: [Option<usize>; 2],

    /// Live polling state (drive source only).
    live: bool,
    last_live_request: Option<Instant>,
    pending_refresh: Option<usize>,

    worker: Arc<WorkerSlot>,
    /// Render differs from requested parameters
    dirty: bool,
}

impl Default for FloppyVisualizerDialog {
    fn default() -> Self {
        let worker = Arc::new(WorkerSlot::default());
        let worker_thread = Arc::clone(&worker);
        thread::Builder::new()
            .name("floppy-visualizer".into())
            .spawn(move || render_worker(&worker_thread))
            .expect("failed to spawn floppy-visualizer worker thread");

        Self {
            open: false,
            source: Source::None,
            image: None,
            layers: LayerFlags::default(),
            side_textures: [None, None],
            head_track: [None, None],
            live: false,
            last_live_request: None,
            pending_refresh: None,
            worker,
            dirty: false,
        }
    }
}

impl FloppyVisualizerDialog {
    /// Opens the dialog for a live drive image
    pub fn open_drive(&mut self, drive: usize) {
        self.open = true;
        self.source = Source::Drive(drive);
        self.image = None;
        self.invalidate();
        self.pending_refresh = Some(drive);
        self.last_live_request = None;
    }

    /// Opens the dialog from a file
    pub fn open_file<P: AsRef<Path>>(&mut self, path: P, image: Box<FloppyImage>) {
        self.open = true;
        self.source = Source::File(path.as_ref().to_path_buf());
        self.image = Some(image);
        self.live = false;
        self.invalidate();
    }

    /// Pushes a fresh drive snapshot
    pub fn set_drive_image(&mut self, drive: usize, image: Option<Box<FloppyImage>>) {
        if !matches!(self.source, Source::Drive(d) if d == drive) {
            return;
        }
        self.image = image;
        self.invalidate();
    }

    /// Returns the drive index for which the caller should refresh the image
    pub fn take_refresh_request(&mut self) -> Option<usize> {
        self.pending_refresh.take()
    }

    /// Updates the live head track for a side.
    pub fn set_head_track(&mut self, drive: usize, side: usize, track: usize) {
        if !matches!(self.source, Source::Drive(d) if d == drive) {
            return;
        }
        if side < 2 {
            self.head_track[side] = Some(track);
        }
    }

    pub fn drive(&self) -> Option<usize> {
        if let Source::Drive(d) = self.source {
            Some(d)
        } else {
            None
        }
    }

    fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub fn update(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }

        if self.live
            && let Source::Drive(drive) = self.source
        {
            let now = Instant::now();
            let due = self
                .last_live_request
                .is_none_or(|t| now.duration_since(t) >= LIVE_REFRESH_INTERVAL);
            let worker_idle = !self.dirty && self.worker.request.lock().unwrap().is_none();
            if due && worker_idle {
                // Request new update for live mode
                self.pending_refresh = Some(drive);
                self.last_live_request = Some(now);
            }
            ctx.request_repaint_after(LIVE_REFRESH_INTERVAL);
        }

        // Pick up any completed sides from the worker.
        {
            let mut results = self.worker.results.lock().unwrap();
            for (side, slot) in results.iter_mut().enumerate() {
                if let Some(image) = slot.take() {
                    let name = format!("floppy_viz_side_{}", side);
                    let tex = ctx.load_texture(name, image, egui::TextureOptions::LINEAR);
                    self.side_textures[side] = Some(tex);
                }
            }
        }

        if self.dirty
            && let Some(img) = self.image.as_ref()
        {
            // Submit render request to worker
            let req = RenderRequest {
                image: img.clone(),
                layers: self.layers,
                size: DISPLAY_SIZE,
                ctx: ctx.clone(),
            };
            *self.worker.request.lock().unwrap() = Some(req);
            self.worker.request_cv.notify_one();
            self.dirty = false;
        }

        let mut open = self.open;
        egui::Window::new("Floppy visualizer")
            .open(&mut open)
            .resizable([true, true])
            .default_size([1100.0, 620.0])
            .show(ctx, |ui| self.draw_body(ui));
        self.open = open;
    }

    fn draw_body(&mut self, ui: &mut egui::Ui) {
        // TODO Fluxfox only has deep inspection for MFM disks
        let mfm_layers_enabled = self
            .image
            .as_deref()
            .is_none_or(|img| matches!(img.get_type(), FloppyType::Mfm144M));

        ui.horizontal(|ui| {
            match &self.source {
                Source::Drive(d) => ui.label(format!("Drive #{}", d + 1)),
                Source::File(p) => ui.label(format!(
                    "File: {}",
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                )),
                Source::None => ui.label("(no source)"),
            };
            ui.separator();

            let mut layers_changed = false;
            layers_changed |= ui.checkbox(&mut self.layers.data, "Data").changed();
            ui.add_enabled_ui(mfm_layers_enabled, |ui| {
                layers_changed |= ui.checkbox(&mut self.layers.metadata, "Metadata").changed();
                layers_changed |= ui.checkbox(&mut self.layers.weak, "Weak").changed();
                layers_changed |= ui.checkbox(&mut self.layers.errors, "Errors").changed();
            });
            if layers_changed {
                self.invalidate();
            }

            ui.separator();

            let is_drive = matches!(self.source, Source::Drive(_));
            ui.add_enabled_ui(is_drive, |ui| {
                if ui.button("Refresh").clicked()
                    && let Source::Drive(drive) = self.source
                {
                    self.pending_refresh = Some(drive);
                }
                if ui.checkbox(&mut self.live, "Live").changed() {
                    self.last_live_request = None;
                }
            });
        });

        ui.separator();

        let Some(image) = self.image.as_deref() else {
            ui.weak("Waiting for floppy image");
            return;
        };

        ui.horizontal(|ui| {
            ui.label(format!("{}", image.get_type()));
            ui.separator();
            ui.label(format!("Title: {}", image.get_title()));
            ui.separator();
            ui.label(format!("Sides: {}", image.get_side_count()));
            ui.separator();
            ui.label(format!("Tracks: {}", image.get_track_count()));
        });

        ui.separator();

        let side_count = image.get_side_count();
        let track_count = image.get_track_count();

        ui.horizontal_wrapped(|ui| {
            for side in 0..side_count {
                ui.vertical(|ui| {
                    ui.label(format!("Side {}", side));
                    let display = egui::vec2(DISPLAY_SIZE as f32, DISPLAY_SIZE as f32);
                    let (rect, _resp) = ui.allocate_exact_size(display, egui::Sense::hover());
                    let painter = ui.painter_at(rect);
                    if let Some(tex) = self.side_textures[side].as_ref() {
                        painter.image(
                            tex.into(),
                            rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            egui::Color32::WHITE,
                        );
                    } else {
                        painter.rect_filled(
                            rect,
                            egui::CornerRadius::ZERO,
                            egui::Color32::from_gray(20),
                        );
                        painter.text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "Please wait...",
                            egui::FontId::proportional(14.0),
                            egui::Color32::WHITE,
                        );
                    }

                    if let Some(track) = self.head_track[side] {
                        draw_head_overlay(&painter, rect, track, track_count);
                    }
                });
            }
        });
    }
}

/// Draw a highlight on the track the head is currently over
fn draw_head_overlay(painter: &egui::Painter, rect: egui::Rect, track: usize, track_count: usize) {
    if track_count == 0 || track >= track_count {
        return;
    }
    let center = rect.center();
    let r_outer = rect.width().min(rect.height()) / 2.0 - 1.0;
    let r_inner = r_outer * MIN_RADIUS_RATIO;
    let band = r_outer - r_inner;
    let r_track_outer = r_inner + band * (1.0 - track as f32 / track_count as f32);
    let r_track_inner = r_inner + band * (1.0 - (track + 1) as f32 / track_count as f32);
    let r_mid = (r_track_outer + r_track_inner) * 0.5;
    let thickness = (r_track_outer - r_track_inner).max(1.5);

    painter.circle_stroke(
        center,
        r_mid,
        egui::Stroke::new(
            thickness,
            egui::Color32::from_rgba_unmultiplied(80, 160, 255, 80),
        ),
    );
}

fn render_worker(worker: &Arc<WorkerSlot>) {
    loop {
        // Wait for a request to land in the slot. Newer dispatches
        // overwrite older pending ones in place, so when we wake we
        // always pick up the latest desired state.
        let req = {
            let mut guard = worker.request.lock().unwrap();
            while guard.is_none() {
                // CondVar unlocks the guard while the thread is parked waiting
                // for the notification.
                guard = worker.request_cv.wait(guard).unwrap();
            }
            guard.take().unwrap()
        };

        let disk = match floppy_image_to_fluxfox(&req.image) {
            Ok(d) => d,
            Err(e) => {
                log::warn!("floppy visualizer: cannot convert image: {e:#}");
                continue;
            }
        };

        let sides = req.image.get_side_count();
        for side in 0..sides {
            match render_side(&disk, side as u8, req.size, req.layers) {
                Ok(img) => {
                    worker.results.lock().unwrap()[side] = Some(img);
                    req.ctx.request_repaint();
                }
                Err(e) => {
                    log::warn!("floppy visualizer: side {side} render failed: {e:#}");
                }
            }
        }
    }
}

fn render_side(
    disk: &DiskImage,
    side: u8,
    size: u32,
    layers: LayerFlags,
) -> Result<egui::ColorImage> {
    let mut pixmap = Pixmap::new(size, size)
        .ok_or_else(|| anyhow::anyhow!("failed to allocate {size}x{size} pixmap"))?;
    pixmap.fill(tiny_skia::Color::from_rgba8(20, 20, 24, 255));

    let direction = if side == 0 {
        TurningDirection::Clockwise
    } else {
        TurningDirection::CounterClockwise
    };

    let track_count = disk.track_ct(side as usize);
    let common = CommonVizParams {
        radius: Some(size as f32 / 2.0),
        max_radius_ratio: 1.0,
        min_radius_ratio: MIN_RADIUS_RATIO,
        pos_offset: None,
        index_angle: 0.0,
        track_limit: Some(track_count),
        pin_last_standard_track: true,
        track_gap: 0.0,
        track_overlap: true,
        direction,
    };

    let data_params = RenderTrackDataParams {
        side,
        decode: false,
        sector_mask: true,
        resolution: Default::default(),
        slices: 1440,
        overlap: 0.1,
    };

    let raster_params = RenderRasterizationParams {
        image_size: VizDimensions::from((size, size)),
        supersample: 1,
        image_bg_color: None,
        disk_bg_color: None,
        mask_color: None,
        palette: None,
        pos_offset: None,
    };

    if layers.data
        && let Err(e) =
            rasterize_track_data(disk, &mut pixmap, &common, &data_params, &raster_params)
    {
        log::debug!("rasterize_track_data side {side}: {e:?}");
    }

    if layers.metadata {
        // Metadata layer
        let meta_params = RenderTrackMetadataParams {
            quadrant: None,
            side,
            geometry: Default::default(),
            winding: Default::default(),
            draw_empty_tracks: false,
            draw_sector_lookup: false,
        };
        let mut meta_common = common.clone();
        meta_common.index_angle = 0.0;

        match vectorize_disk_elements_by_quadrants(disk, &meta_common, &meta_params) {
            Ok(display_list) => {
                let mut paint = Paint {
                    anti_alias: true,
                    ..Default::default()
                };
                let styles = default_skia_styles();
                let track_style = Default::default();
                let _ = render_display_list(
                    &mut pixmap,
                    &mut paint,
                    common.index_angle,
                    &display_list,
                    &track_style,
                    &styles,
                );
            }
            Err(e) => {
                log::debug!("vectorize_disk_elements side {side}: {e:?}");
            }
        }
    }

    if layers.weak {
        // Weak bits layer
        let mut rr = raster_params.clone();
        rr.mask_color = Some(VizColor::from_rgba8(255, 200, 0, 180));
        if let Err(e) = render_track_mask(
            disk,
            &mut pixmap,
            RenderMaskType::WeakBits,
            &common,
            &data_params,
            &rr,
        ) {
            log::debug!("render_track_mask weak side {side}: {e:?}");
        }
    }

    if layers.errors {
        // Error layer
        let mut rr = raster_params.clone();
        rr.mask_color = Some(VizColor::from_rgba8(255, 64, 64, 200));
        if let Err(e) = render_track_mask(
            disk,
            &mut pixmap,
            RenderMaskType::Errors,
            &common,
            &data_params,
            &rr,
        ) {
            log::debug!("render_track_mask errors side {side}: {e:?}");
        }
    }

    // tiny_skia stores pixels as premultiplied RGBA8; egui::ColorImage
    // wants un-premultiplied. Blit byte-by-byte and un-premultiply.
    let mut pixels = Vec::with_capacity((size * size) as usize);
    for px in pixmap.pixels() {
        let r = px.red();
        let g = px.green();
        let b = px.blue();
        let a = px.alpha();
        if a == 0 {
            pixels.push(egui::Color32::TRANSPARENT);
        } else {
            let inv = 255.0 / a as f32;
            let to_u8 = |c: u8| (c as f32 * inv).round().clamp(0.0, 255.0) as u8;
            pixels.push(egui::Color32::from_rgba_unmultiplied(
                to_u8(r),
                to_u8(g),
                to_u8(b),
                a,
            ));
        }
    }
    Ok(egui::ColorImage::new(
        [size as usize, size as usize],
        pixels,
    ))
}
