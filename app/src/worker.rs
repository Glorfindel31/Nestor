//! The one road between the UI and the engine.
//!
//! `ui::App::update` runs on the thread that pumps the window's event loop.
//! Anything slow called from there freezes the window solid for its whole
//! duration - no repaint, no input, nothing. That is not hypothetical: this
//! project shipped exactly that bug under Tauri (both commands were
//! synchronous `#[tauri::command]`s, which run on the IPC dispatch thread,
//! which on desktop is the event-loop thread) and an 80-generation run made
//! the app look hung. See `docs/PORT_STATUS.md`'s Phase 6 row.
//!
//! So: every call into `crate::commands` happens on a thread spawned here,
//! and results come back as `Msg`s on a channel the UI drains at the top of
//! each frame. One thread per job rather than a pool - jobs are one-shot and
//! never more than a couple in flight, so a pool would be machinery with
//! nothing to manage.
//!
//! `ctx.request_repaint()` after every send is load-bearing, not a
//! nicety: egui only repaints in response to input otherwise, so without it
//! a progress event sent while the mouse is still would sit in the channel
//! unseen until the user happened to move it.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;

use crate::commands;
use crate::dto::{
    BestResultDto, ExportRequest, NestConfigDto, NestRunCompleteDto, NestRunStartDto, PolygonDto, RepackSheetRequest, RepackSheetResponse, ReportRequest, RunNestRequest, RunNestResponse,
    ValidatePlacementRequest, ValidatePlacementResponse,
};

/// Which exporter a save targets. The three share one request shape
/// (`ExportRequest`); only PDF needs the extra report metadata.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExportFormat {
    Dxf,
    Svg,
    Pdf,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 3] = [ExportFormat::Dxf, ExportFormat::Svg, ExportFormat::Pdf];

    pub fn ext(self) -> &'static str {
        match self {
            ExportFormat::Dxf => "dxf",
            ExportFormat::Svg => "svg",
            ExportFormat::Pdf => "pdf",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Dxf => "DXF",
            ExportFormat::Svg => "SVG",
            ExportFormat::Pdf => "PDF REPORT",
        }
    }
}

/// Everything the worker can send back. One enum rather than a channel per
/// event type, so `update()` drains exactly one queue and ordering between
/// (say) a progress tick and a completion is preserved.
pub enum Msg {
    /// One file finished importing. Sent per file, not per batch, so a
    /// 30-file drop shows progress instead of going quiet.
    Imported { file: String, shapes: Vec<PolygonDto> },
    ImportFailed { file: String, error: String },
    ImportBatchDone { ok: usize, failed: usize },

    RunStart(NestRunStartDto),
    /// Once per completed generation.
    Progress { generation: usize, generations: usize, best_fitness: f64, sheets_used: usize, unplaced_count: usize, utilisation: f64 },
    /// Once per individual placed *within* a generation. Fires far more
    /// often than `Progress`; without it a slow generation is
    /// indistinguishable from a hung one.
    Tick { generation: usize, individuals_done: usize, individuals_total: usize },
    RunComplete(NestRunCompleteDto),
    NestDone(Box<Result<RunNestResponse, String>>),

    Repacked(Box<Result<RepackSheetResponse, String>>),
    Validated(Box<Result<ValidatePlacementResponse, String>>),
    Exported { format: ExportFormat, result: Result<(), String> },

    /// Startup: whatever `config.json` and `best_result.json` held.
    Loaded { config: Option<NestConfigDto>, best: Option<BestResultDto>, errors: Vec<String> },

    /// Free-form narration for the console window.
    Log(String),
}

pub struct Worker {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: egui::Context,
    pub cancel: Arc<commands::NestCancelFlag>,
}

impl Worker {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self { tx, rx, ctx, cancel: Arc::new(commands::NestCancelFlag::default()) }
    }

    pub fn drain(&self) -> impl Iterator<Item = Msg> + '_ {
        self.rx.try_iter()
    }

    /// Runs `job` on a fresh thread with a sender and a repaint handle.
    fn spawn(&self, job: impl FnOnce(&Emit) + Send + 'static) {
        let emit = Emit { tx: std::sync::Mutex::new(self.tx.clone()), ctx: self.ctx.clone() };
        std::thread::spawn(move || job(&emit));
    }

    /// Reads `config.json` and `best_result.json`. On its own thread like
    /// everything else - it's two small reads, but doing them inline would
    /// mean the very first frame is the one that blocks.
    pub fn load_saved(&self) {
        self.spawn(|emit| {
            let mut errors = Vec::new();
            let config = commands::load_config().unwrap_or_else(|e| {
                errors.push(format!("could not read saved config: {e}"));
                None
            });
            let best = commands::load_best_result().unwrap_or_else(|e| {
                errors.push(format!("could not read saved best result: {e}"));
                None
            });
            emit.send(Msg::Loaded { config, best, errors });
        });
    }

    /// Imports a batch sequentially, one message per file. A file that fails
    /// to parse is reported and skipped, not fatal to the batch - one bad
    /// DXF in a drop of thirty should not lose the other twenty-nine.
    pub fn import(&self, paths: Vec<std::path::PathBuf>, tolerance: f64, svg_unit: Option<String>) {
        self.spawn(move |emit| {
            let (mut ok, mut failed) = (0, 0);
            for path in paths {
                let p = path.to_string_lossy().to_string();
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.clone());
                let is_svg = path.extension().map(|e| e.eq_ignore_ascii_case("svg")).unwrap_or(false);
                let result = if is_svg {
                    commands::import_svg(&p, tolerance, svg_unit.as_deref())
                } else {
                    commands::import_dxf(&p, tolerance)
                };
                match result {
                    Ok(shapes) => {
                        ok += 1;
                        emit.send(Msg::Imported { file: name, shapes });
                    }
                    Err(error) => {
                        failed += 1;
                        emit.send(Msg::ImportFailed { file: name, error });
                    }
                }
            }
            emit.send(Msg::ImportBatchDone { ok, failed });
        });
    }

    /// Starts a nest. Returns `Err` if one is already in flight - the guard
    /// is backend-side (`NestCancelFlag::begin_run`) rather than trusting the
    /// UI to keep the RUN button disabled, because two overlapping runs would
    /// share one cancel flag with no way to tell them apart.
    pub fn nest(&self, request: RunNestRequest) -> Result<(), String> {
        self.cancel.begin_run()?;
        let flag = self.cancel.clone();
        let cancel_handle = self.cancel.cancel_handle();
        // Cloned before `request` moves into the thread: a recovered result
        // needs the sheet geometry to render against in a later session, and
        // the config to be repackable or hand-editable at all (every one of
        // those paths needs the margin/spacing/rotations the nest actually
        // ran with).
        let request_sheets = request.sheets.clone();
        let request_config = Some(request.config.clone());

        self.spawn(move |emit| {
            let result = commands::run_nest_with_progress(
                request,
                |generation, generations, best| {
                    emit.send(Msg::Progress {
                        generation,
                        generations,
                        best_fitness: best.fitness,
                        sheets_used: best.placements.len(),
                        unplaced_count: best.unplaced_count,
                        utilisation: best.utilisation,
                    });
                },
                move || cancel_handle.load(std::sync::atomic::Ordering::Relaxed),
                |generation, individuals_done, individuals_total| {
                    emit.send(Msg::Tick { generation, individuals_done, individuals_total });
                },
                |s| emit.send(Msg::RunStart(*s)),
                |c| emit.send(Msg::RunComplete(*c)),
            );
            flag.end_run();

            // Best-effort persistence: a cancelled or empty run has nothing
            // worth keeping, and an I/O failure here must never fail an
            // otherwise successful nest - the UI already has the response
            // regardless. Logged rather than silently swallowed.
            if let Ok(response) = &result {
                if !response.placements.is_empty() {
                    let candidate = BestResultDto {
                        placements: response.placements.clone(),
                        fitness: response.fitness,
                        utilisation: response.utilisation,
                        unplaced_count: response.unplaced_count,
                        unplaced_ids: response.unplaced_ids.clone(),
                        parts_by_id: response.parts_by_id.clone(),
                        sheets: request_sheets,
                        part_rules: response.part_rules.clone(),
                        config: request_config,
                    };
                    if let Err(e) = commands::save_best_result_if_better(&candidate) {
                        emit.send(Msg::Log(format!("could not persist best result: {e}")));
                    }
                }
            }
            emit.send(Msg::NestDone(Box::new(result)));
        });
        Ok(())
    }

    pub fn repack(&self, request: RepackSheetRequest) {
        self.spawn(move |emit| emit.send(Msg::Repacked(Box::new(commands::repack_sheet(request)))));
    }

    pub fn validate(&self, request: ValidatePlacementRequest) {
        self.spawn(move |emit| emit.send(Msg::Validated(Box::new(commands::validate_placement(request)))));
    }

    pub fn export(&self, format: ExportFormat, path: std::path::PathBuf, request: ExportRequest, report: Option<ReportRequest>) {
        self.spawn(move |emit| {
            let p = path.to_string_lossy().to_string();
            let result = match format {
                ExportFormat::Dxf => commands::export_dxf(&p, request),
                ExportFormat::Svg => commands::export_svg(&p, request),
                // `report` is always `Some` for PDF - built by the caller,
                // which is the only place that knows the part list and title.
                ExportFormat::Pdf => match report {
                    Some(r) => commands::export_report(&p, r),
                    None => Err("internal: PDF export without report metadata".to_string()),
                },
            };
            emit.send(Msg::Exported { format, result });
        });
    }

    pub fn save_config(&self, config: NestConfigDto) {
        self.spawn(move |emit| {
            if let Err(e) = commands::save_config(&config) {
                emit.send(Msg::Log(format!("could not save config: {e}")));
            }
        });
    }

    pub fn clear_best_result(&self) {
        self.spawn(|emit| {
            if let Err(e) = commands::clear_best_result() {
                emit.send(Msg::Log(format!("could not clear saved best result: {e}")));
            }
        });
    }
}

/// A sender plus the repaint handle that has to accompany every send.
/// Bundled so a job can't accidentally do one without the other.
///
/// The `Mutex` is what makes `Emit` `Sync`, and that is a requirement, not
/// tidiness: `run_nest_with_progress`'s `on_individual_placed` hook is called
/// from rayon's worker threads and so must be `Sync`, which a bare
/// `mpsc::Sender` (`Send` but not `Sync`) can't satisfy. One uncontended-ish
/// lock per individual placed is nothing against the placement work itself.
pub struct Emit {
    tx: std::sync::Mutex<Sender<Msg>>,
    ctx: egui::Context,
}

impl Emit {
    pub fn send(&self, msg: Msg) {
        // A closing window drops the receiver; there's no meaningful
        // recovery from inside a progress callback, so a failed send is
        // ignored rather than aborting an otherwise-fine run.
        let _ = self.tx.lock().expect("log channel poisoned").send(msg);
        self.ctx.request_repaint();
    }
}
