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
    BestResultDto, ExportRequest, NestConfigDto, NestRunCompleteDto, NestRunStartDto, PlacedPartDto, PolygonDto, RepackSheetRequest, RepackSheetResponse, ReportRequest, RunNestRequest,
    RunNestResponse, SheetPlacementDto, ValidatePlacementRequest, ValidatePlacementResponse,
};

use crate::dto::{AuditReportDto, AuditRequest, RemnantDto, RemnantRequest, ShapeStore};

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
    /// `size_guessed` means nothing in the file said how big the drawing is
    /// and 96dpi was assumed - see `geometry::svg_import::size_is_guessed`.
    /// Always `false` for DXF, which carries real-world units by definition.
    Imported { file: String, shapes: Vec<PolygonDto>, size_guessed: bool },
    ImportFailed { file: String, error: String },
    ImportBatchDone { ok: usize, failed: usize },

    RunStart(NestRunStartDto),
    /// The id -> shape map for the whole run, once, before the first part
    /// lands. The live view has ids to draw from the moment placement
    /// starts; without this it would have nothing to draw them *with* until
    /// the finished response arrived.
    PartsReady(std::collections::HashMap<usize, PolygonDto>),

    /// Once per completed generation.
    Progress { generation: usize, generations: usize, best_fitness: f64, sheets_used: usize, unplaced_count: usize, utilisation: f64 },
    /// Once per individual placed *within* a generation. Fires far more
    /// often than `Progress`; without it a slow generation is
    /// indistinguishable from a hung one.
    Tick { generation: usize, individuals_done: usize, individuals_total: usize },
    /// The layout one individual has built so far, for the live view. Sent
    /// at most every `LIVE_FRAME`, and only while the live toggle is on.
    ///
    /// A whole frame rather than a per-part delta: the UI redraws from a
    /// `Snapshot` it already knows how to render, so handing it the current
    /// state costs one clone of a few hundred small structs and saves the
    /// UI from maintaining a second, incrementally-mutated copy of the
    /// layout that could drift out of step with the engine's.
    Live { placements: Vec<SheetPlacementDto>, ghost: Option<GhostSet> },


    RunComplete(NestRunCompleteDto),
    NestDone(Box<Result<RunNestResponse, String>>),

    Repacked(Box<Result<RepackSheetResponse, String>>),
    Validated(Box<Result<ValidatePlacementResponse, String>>),
    /// The whole-result manufacturability check. Runs after every nest,
    /// drag and repack, so the badge can never describe a stale arrangement.
    Audited(Box<Result<AuditReportDto, String>>),

    /// The saved parts library and remnant shelf, as read from disk.
    StoreLoaded(Box<Result<ShapeStore, String>>),
    /// A write finished. Carries the store it wrote so the UI adopts exactly
    /// what is now on disk, rather than assuming its in-memory copy matched.
    StoreSaved(Box<Result<ShapeStore, String>>),
    /// Offcuts harvested from the displayed result.
    RemnantsComputed(Box<Result<Vec<RemnantDto>, String>>),
    Exported { format: ExportFormat, result: Result<(), String> },

    /// Startup: whatever `config.json` and `best_result.json` held.
    Loaded { config: Option<NestConfigDto>, best: Option<BestResultDto>, errors: Vec<String> },

    /// A newer release exists on GitHub. Only ever sent when there is one -
    /// silence means "current", or that the check could not reach the net.
    UpdateAvailable(crate::update::Release),

    /// Free-form narration for the console window.
    Log(String),
}

pub struct Worker {
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: egui::Context,
    pub cancel: Arc<commands::NestCancelFlag>,
    /// Whether a running nest should report its progress part by part.
    ///
    /// Shared with the job thread rather than read from the request, so the
    /// user can turn the live view on and off *during* a run. The engine
    /// hooks check it on every part; off costs one relaxed atomic load.
    pub live: Arc<std::sync::atomic::AtomicBool>,
}


impl Worker {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self { tx, rx, ctx, cancel: Arc::new(commands::NestCancelFlag::default()), live: Arc::new(std::sync::atomic::AtomicBool::new(false)) }

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

    /// Asks GitHub whether a newer release exists. Failure is logged and
    /// dropped: an offline machine gets no banner and no complaint.
    pub fn check_update(&self) {
        self.spawn(|emit| match crate::update::check(env!("CARGO_PKG_VERSION")) {
            Ok(Some(release)) => emit.send(Msg::UpdateAvailable(release)),
            Ok(None) => {}
            Err(e) => emit.send(Msg::Log(format!("update check failed: {e}"))),
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
                    commands::import_dxf(&p, tolerance).map(|shapes| (shapes, false))
                };
                match result {
                    Ok((shapes, size_guessed)) => {
                        ok += 1;
                        emit.send(Msg::Imported { file: name, shapes, size_guessed });
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
        let live_on = self.live.clone();


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
                |parts| emit.send(Msg::PartsReady(parts.clone())),
                {
                    let frame = std::sync::Mutex::new(LiveFrame::default());
                    move |event| {
                        if !live_on.load(std::sync::atomic::Ordering::Relaxed) {
                            return;
                        }
                        use nesting::placement::LiveEvent;
                        let mut frame = frame.lock().expect("live frame poisoned");
                        match event {
                            LiveEvent::Begin => frame.reset(),
                            LiveEvent::Part { sheet, part } => frame.push(sheet, part),
                            LiveEvent::Candidates { sheet, part_id, traces } => {
                                frame.ghost = Some(GhostSet {
                                    sheet,
                                    part_id,
                                    positions: traces.iter().map(|t| Ghost { x: t.x, y: t.y, rotation: t.rotation, accepted: t.accepted }).collect(),
                                });
                            }

                        }
                        if let Some(msg) = frame.due(std::time::Instant::now()) {
                            emit.send(msg);
                        }
                    }
                },
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

    pub fn audit(&self, request: AuditRequest) {
        self.spawn(move |emit| emit.send(Msg::Audited(Box::new(commands::audit_nest(request)))));
    }

    pub fn load_store(&self) {
        self.spawn(|emit| emit.send(Msg::StoreLoaded(Box::new(commands::load_shape_store()))));
    }

    /// Writes the store and reports back what it wrote. The whole store goes
    /// over rather than a delta: it is a few kilobytes, and a delta protocol
    /// is a way to get the UI and the file out of step for no measurable gain.
    pub fn save_store(&self, store: ShapeStore) {
        self.spawn(move |emit| {
            let result = commands::save_shape_store(&store).map(|()| store);
            emit.send(Msg::StoreSaved(Box::new(result)));
        });
    }

    pub fn compute_remnants(&self, request: RemnantRequest) {
        self.spawn(move |emit| emit.send(Msg::RemnantsComputed(Box::new(commands::compute_remnants(request)))));
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

//// One position the engine scored for a part and did not necessarily take.
///
/// `nesting::placement::CandidateTrace` with the score dropped: the live
/// view draws these as outlines and only needs to know where, at what angle,
/// and whether this was the one that won.
#[derive(Clone, Copy, Debug)]
pub struct Ghost {
    pub x: f64,
    pub y: f64,
    pub rotation: f64,
    pub accepted: bool,
}

/// Every position scored for the one part the engine is currently placing.
#[derive(Clone, Debug)]
pub struct GhostSet {
    pub sheet: usize,
    pub part_id: usize,
    pub positions: Vec<Ghost>,
}


/// How often the live view may be fed, at most.
///
/// Parts land every few milliseconds and every `Emit::send` takes a lock and
/// requests a repaint, so an unthrottled stream would spend more time in the
/// channel than in the placement it is supposed to be showing. 33ms is one
/// frame at 30fps - past that the eye cannot follow individual parts landing
/// anyway.
const LIVE_FRAME: std::time::Duration = std::time::Duration::from_millis(33);

/// Accumulates the watched individual's layout and feeds `Msg::Live`.
///
/// Lives behind one `Mutex` because `place_parts` calls its hooks from
/// rayon's worker threads (same reason `Emit` has one). Only ever touched by
/// the one individual `run_generation` chose to watch, so the lock is
/// uncontended in practice.
#[derive(Default)]
struct LiveFrame {
    sheets: Vec<SheetPlacementDto>,
    ghost: Option<GhostSet>,
    last_sent: Option<std::time::Instant>,
}


impl LiveFrame {
    fn reset(&mut self) {
        self.sheets.clear();
        self.ghost = None;
    }


    fn push(&mut self, sheet: usize, part: &nesting::placement::PlacedPart) {
        // Sheets arrive in order but a gap would be silent corruption, so
        // grow to fit rather than indexing blind.
        while self.sheets.len() <= sheet {
            self.sheets.push(SheetPlacementDto { sheet_index: self.sheets.len(), parts: Vec::new() });
        }
        self.sheets[sheet].parts.push(PlacedPartDto { id: part.id, x: part.placement.x, y: part.placement.y, rotation: part.rotation, locked: false });
        // The ghosts belonged to the part that just landed; it is drawn
        // solid now, so they have served their purpose.
        self.ghost = None;
    }


    /// A frame if enough time has passed, `None` otherwise. Clones rather
    /// than drains: the accumulated layout is still the truth for the next
    /// frame, only the *sending* is throttled.
    fn due(&mut self, now: std::time::Instant) -> Option<Msg> {
        if self.last_sent.is_some_and(|t| now.duration_since(t) < LIVE_FRAME) {
            return None;
        }
        self.last_sent = Some(now);
        Some(Msg::Live { placements: self.sheets.clone(), ghost: self.ghost.clone() })

    }
}

// A sender plus the repaint handle that has to accompany every send.
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

#[cfg(test)]
mod tests {
    use super::*;
    use nesting::placement::{PlacedPart, Placement};

    fn part(id: usize, x: f64, y: f64) -> PlacedPart {
        PlacedPart { id, placement: Placement { x, y }, rotation: 0.0 }
    }

    /// Parts land on sheets in order, but a gap must grow the vector rather
    /// than index past its end - the accumulator is fed straight from the
    /// engine and has no say in what order sheets are opened.
    #[test]
    fn accumulates_parts_onto_their_own_sheets() {
        let mut frame = LiveFrame::default();
        frame.push(0, &part(1, 10.0, 20.0));
        frame.push(2, &part(2, 30.0, 40.0));

        assert_eq!(frame.sheets.len(), 3, "sheet 2 must create the empty sheet 1 before it");
        assert_eq!(frame.sheets[0].parts.len(), 1);
        assert!(frame.sheets[1].parts.is_empty());
        assert_eq!(frame.sheets[2].parts[0].id, 2);
        assert_eq!(frame.sheets[2].parts[0].x, 30.0);
        // Every sheet reports its own index, so the UI can look up geometry
        // by it rather than by position in this vector.
        assert_eq!(frame.sheets.iter().map(|s| s.sheet_index).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    /// A placed part is drawn solid, so the outlines that were weighing up
    /// where to put it have to stop being drawn at the same moment.
    #[test]
    fn a_landed_part_clears_the_ghosts_that_preceded_it() {
        let mut frame = LiveFrame::default();
        frame.ghost = Some(GhostSet { sheet: 0, part_id: 7, positions: vec![Ghost { x: 0.0, y: 0.0, rotation: 0.0, accepted: true }] });
        frame.push(0, &part(7, 1.0, 2.0));
        assert!(frame.ghost.is_none());
    }

    /// `Begin` means a different individual is starting from an empty sheet.
    /// Without the reset every individual's layout would pile up on the last.
    #[test]
    fn reset_drops_the_previous_individuals_layout() {
        let mut frame = LiveFrame::default();
        frame.push(0, &part(1, 0.0, 0.0));
        frame.reset();
        assert!(frame.sheets.is_empty());
        assert!(frame.ghost.is_none());
    }

    /// The throttle is the whole reason this type exists: parts land every
    /// few milliseconds and every send takes a lock and forces a repaint.
    #[test]
    fn frames_are_rate_limited_but_never_lose_the_accumulated_layout() {
        let mut frame = LiveFrame::default();
        let t0 = std::time::Instant::now();
        frame.push(0, &part(1, 0.0, 0.0));

        // The first frame always goes: there is nothing to wait behind.
        let first = frame.due(t0).expect("first frame should send immediately");
        let Msg::Live { placements, .. } = first else { panic!("expected a Live frame") };
        assert_eq!(placements[0].parts.len(), 1);

        // A part landing inside the window is accumulated, not sent.
        frame.push(0, &part(2, 5.0, 5.0));
        assert!(frame.due(t0 + LIVE_FRAME / 2).is_none(), "a second frame inside the window must be withheld");

        // ...and is still there when the window passes, so throttling delays
        // the drawing without ever dropping a part from it.
        let later = frame.due(t0 + LIVE_FRAME).expect("the window has passed");
        let Msg::Live { placements, .. } = later else { panic!("expected a Live frame") };
        assert_eq!(placements[0].parts.len(), 2);
    }
}
