//! The whole UI. Immediate-mode (egui), no markup, no webview.
//!
//! The one invariant this module must not break: **nothing here calls into
//! `crate::commands` directly**. Every backend call goes through
//! `crate::worker` onto a background thread, because `update()` runs on the
//! thread that pumps the window's event loop - a synchronous import of a big
//! DXF, or a nest run of any real size, would freeze the window solid for
//! its whole duration. This project already hit exactly that bug once under
//! Tauri (see `docs/PORT_STATUS.md`'s Phase 6 row) and the hazard here is
//! identical.
//!
//! Layout, matching what the web UI established: a header strip, then four
//! numbered steps - 01 IMPORT, 02 ASSIGN ROLES, 03 CONFIGURE (in the bottom
//! drawer), 04 RESULT - plus a floating RUN control and a floating console.

mod canvas;
mod config;
mod console;
mod history_chart;
mod i18n;
mod import;
mod library;
mod keys;
mod prefs;
mod result;
mod shapes;
mod shell;
mod state;
mod theme;

use std::collections::HashMap;

use crate::dto::{BestResultDto, NestSnapshotDto, PartRuleDto, PolygonDto, RunNestRequest, SheetPlacementDto};
use crate::worker::{ExportFormat, Msg, Worker};
use i18n::{t, tv};
use state::{ConfigForm, ShapeRow, Status};

/// Bumped from `rustynesting-prefs` when the palette was first replaced: the
/// old key holds a rust/oxide `accent` hex that would have silently overridden
/// the new default. Deliberately *not* bumped again now that the accent is a
/// constant - an `accent` left in a stored blob is simply an unknown field to
/// this struct, so language, scale and help-dismissed all survive the change.
const PREFS_KEY: &str = "rustynesting-prefs-v2";

/// A nest result as the RESULT panel displays it. Either the winner of a run,
/// one of the earlier attempts from its history, or a recovered best result
/// from a previous session.
///
/// `Clone` for the undo stack - see `App::push_undo`.
#[derive(Clone)]
pub struct Snapshot {
    pub placements: Vec<SheetPlacementDto>,
    pub fitness: f64,
    /// Already a percentage (0-100), not a fraction - `nesting`'s
    /// `recompute_totals`/`place_parts` both multiply by 100 before it ever
    /// reaches here, and `best_result.json` stores it that way. Display it
    /// verbatim; multiplying again is what once printed "9025.8% utilisation".
    pub utilisation: f64,
    pub unplaced_count: usize,
    pub unplaced_ids: Vec<usize>,
    /// Which parts are pinned. Lives here rather than on the placement so
    /// that switching between history entries doesn't carry pins across
    /// results they don't belong to.
    pub locked: std::collections::HashSet<usize>,
}

/// Moves one step along the edit history: takes the newest state off `from`,
/// makes it current, and puts the state being left onto `to`. `false` when
/// there was nothing to move.
///
/// Undo and redo are the same operation with the stacks swapped, and writing
/// it once is what makes them exactly inverse - the failure mode of two
/// hand-written versions is a redo that pushes a *clone* of the current state
/// rather than the state itself, which quietly duplicates entries and makes
/// undo/redo/undo land somewhere other than where it started.
fn step_history(from: &mut Vec<Snapshot>, to: &mut Vec<Snapshot>, current: &mut Option<Snapshot>) -> bool {
    let Some(previous) = from.pop() else { return false };
    if let Some(leaving) = current.replace(previous) {
        to.push(leaving);
    }
    true
}

impl Snapshot {
    fn from_history(h: &NestSnapshotDto) -> Self {
        Self {
            placements: h.placements.clone(),
            fitness: h.fitness,
            utilisation: h.utilisation,
            unplaced_count: h.unplaced_count,
            unplaced_ids: h.unplaced_ids.clone(),
            locked: Default::default(),
        }
    }
}

pub struct App {
    prefs: prefs::Prefs,
    worker: Worker,
    console: console::Console,

    // ---- 01 IMPORT ----
    import_status: Status,
    importing: bool,
    /// Shapes (not files) read by the batch in flight - what the status line
    /// reports when it finishes. Counting files instead said "1 shape(s)
    /// imported" after reading 99 of them out of one DXF.
    imported_this_batch: usize,
    rect_w: f64,
    rect_h: f64,
    rect_layer: String,
    /// Paths waiting on the user's answer to the SVG-unit dialog. The prompt
    /// is per *batch*, not per file - being asked the same question thirty
    /// times for one drop is not a dialog, it's a punishment.
    pending_svg_batch: Option<Vec<std::path::PathBuf>>,
    svg_unit_choice: Option<String>,

    // ---- 02 ASSIGN ROLES ----
    shapes: Vec<ShapeRow>,
    next_ui_id: usize,
    shapes_collapsed: bool,
    select_all: bool,
    /// What the bulk-apply row is currently set to. Held on `App` rather than
    /// read back out of the table, because these are the *pending* values -
    /// nothing is written to any row until the matching APPLY is pressed.
    bulk_rot: state::RotRule,
    bulk_mirror: state::MirrorRule,
    bulk_qty: usize,
    confirm_remove: bool,

    // ---- 03 CONFIGURE ----
    cfg: ConfigForm,
    settings_open: bool,
    /// Whether the left-hand log panel is open.
    console_open: bool,
    /// Result states to restore with Ctrl+Z, oldest first.
    ///
    /// A dragged piece and a repacked sheet both edit the current result in
    /// place, and a mis-aimed drag is otherwise unrecoverable without
    /// re-running the whole nest. Whole-`Snapshot` clones rather than a
    /// per-edit diff: a snapshot is placements plus four scalars, the edits
    /// are user-speed rather than per-frame, and an inverse operation per
    /// edit type is a lot of machinery to get subtly wrong for something
    /// this small.
    undo_stack: Vec<Snapshot>,
    /// The other half of `undo_stack`: states undone and not yet redone,
    /// newest last. Cleared by any *new* edit, which is what stops a redo
    /// from restoring a result that no longer follows from what is on screen.
    redo_stack: Vec<Snapshot>,
    advanced_open: bool,

    // ---- run ----
    running: bool,
    progress: f32,
    run_status: Status,
    /// Generation budget of the run in flight, re-set per escalating attempt
    /// - only used for progress-bar arithmetic.
    current_generations: usize,

    // ---- 04 RESULT ----
    snapshot: Option<Snapshot>,
    /// Result of the last manufacturability check, and whether one is in
    /// flight. `None` means "not checked yet" - which the badge shows as its
    /// own state rather than as a pass, because an unchecked nest and a
    /// clean one are exactly the distinction the audit exists to make.
    audit: Option<crate::dto::AuditReportDto>,
    auditing: bool,
    /// The saved parts library and remnant shelf. Loaded once at startup; a
    /// load failure leaves this at its default *and* logs loudly, rather than
    /// letting an unreadable file look like an empty library.
    store: crate::dto::ShapeStore,
    store_open: bool,
    /// Offcuts harvested from the displayed result, waiting to be saved.
    /// Cleared whenever the result changes, for the same reason the audit is.
    remnants: Vec<crate::dto::RemnantDto>,
    harvesting: bool,
    history: Vec<NestSnapshotDto>,
    history_index: usize,
    /// The authoritative id -> shape map from the last run. Used for
    /// rendering *and* for export, rather than re-deriving it from the
    /// request (which would have to exactly mirror `expand_parts`'s id
    /// assignment, and would silently drift the moment either side changed).
    parts_by_id: HashMap<usize, PolygonDto>,
    part_rules: HashMap<usize, PartRuleDto>,
    /// Which library entry each *expanded* sheet came from, index-aligned with
    /// the `sheets` of the run in flight. Built at the same time and in the
    /// same order as that list, so a placement's `sheet_index` resolves
    /// straight back to the offcut it consumed - deriving it afterwards would
    /// mean re-implementing `build_request`'s quantity expansion and drifting
    /// the moment either side changed.
    sheet_origin: Vec<Option<usize>>,
    /// Sheets and config the displayed result was produced with. `config` is
    /// `None` for a result recovered from a session that predates it being
    /// saved - repack and drag are disabled in that case rather than failing
    /// at click time.
    result_sheets: Vec<PolygonDto>,
    result_config: Option<crate::dto::NestConfigDto>,
    export_format: ExportFormat,
    export_spacing: f64,
    export_outline: bool,
    export_unplaced: bool,
    export_status: Status,
    /// Drag in progress on the result canvas: which part, and where it has
    /// been dragged to in model coordinates.
    drag: Option<result::Drag>,
    /// Zoom/pan per result sheet card. Absent means "fitted", which is what
    /// every sheet starts as - a sheet only earns an entry once someone
    /// actually moves its view.
    sheet_views: HashMap<usize, result::SheetView>,
    repacking: Option<usize>,

    // ---- dialogs / chrome ----
    help_open: bool,
    settings_menu_open: bool,
    confirm_reset: bool,
    /// A best result found in a previous session, waiting on "recover or
    /// start fresh".
    recover_prompt: Option<Box<BestResultDto>>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let prefs: prefs::Prefs = cc.storage.and_then(|s| eframe::get_value(s, PREFS_KEY)).unwrap_or_default();
        theme::apply(&cc.egui_ctx, prefs.scale.factor());
        // Once, here - not inside `apply`, which reruns on every TEXT SIZE
        // change. See `install_fonts`.
        theme::install_fonts(&cc.egui_ctx);
        // Explicitly 1.0, not merely left alone: egui persists the zoom
        // factor in its own memory, so a version that once set it would
        // otherwise keep scaling strokes here forever.
        cc.egui_ctx.set_zoom_factor(1.0);

        let worker = Worker::new(cc.egui_ctx.clone());
        worker.load_saved();

        let mut app = Self {
            help_open: !prefs.help_dismissed,
            prefs,
            worker,
            console: Default::default(),
            import_status: Default::default(),
            importing: false,
            imported_this_batch: 0,
            rect_w: 2440.0,
            rect_h: 1220.0,
            rect_layer: "CUSTOM".into(),
            pending_svg_batch: None,
            svg_unit_choice: None,
            shapes: Vec::new(),
            next_ui_id: 1,
            shapes_collapsed: false,
            select_all: false,
            bulk_rot: state::RotRule::Any,
            bulk_mirror: state::MirrorRule::Job,
            bulk_qty: 1,
            confirm_remove: false,
            cfg: Default::default(),
            settings_open: false,
            console_open: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            advanced_open: false,
            running: false,
            progress: 0.0,
            run_status: Default::default(),
            current_generations: 1,
            snapshot: None,
            audit: None,
            auditing: false,
            store: Default::default(),
            store_open: false,
            remnants: Vec::new(),
            harvesting: false,
            history: Vec::new(),
            history_index: 0,
            parts_by_id: Default::default(),
            part_rules: Default::default(),
            sheet_origin: Vec::new(),
            result_sheets: Vec::new(),
            result_config: None,
            export_format: ExportFormat::Dxf,
            export_spacing: 20.0,
            export_outline: true,
            export_unplaced: false,
            export_status: Default::default(),
            drag: None,
            sheet_views: HashMap::new(),
            repacking: None,
            settings_menu_open: false,
            confirm_reset: false,
            recover_prompt: None,
        };
        app.console.log(console::Kind::Run, "Nestor started");
        app.worker.load_store();
        app
    }

    fn t<'a>(&self, key: &'a str) -> &'a str {
        t(self.prefs.lang, key)
    }

    fn tv(&self, key: &str, vars: &[(&str, &str)]) -> String {
        tv(self.prefs.lang, key, vars)
    }

    /// True while a run is in flight. The web UI locked the RESULT panel too,
    /// not just the inputs, and that is deliberate: without it a repack or an
    /// export could mix the previous run's placements with the new run's
    /// parts.
    fn controls_locked(&self) -> bool {
        self.running
    }

    /// Drains everything the worker has sent since the last frame.
    fn pump(&mut self) {
        let msgs: Vec<Msg> = self.worker.drain().collect();
        for msg in msgs {
            self.handle(msg);
        }
    }

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Imported { file, shapes } => {
                self.console.log(console::Kind::Plain, format!("imported {} shape(s) from {file}", shapes.len()));
                self.imported_this_batch += shapes.len();
                for poly in shapes {
                    self.push_shape(file.clone(), poly);
                }
            }
            Msg::ImportFailed { file, error } => {
                self.console.error(format!("import failed for {file}: {error}"));
            }
            Msg::ImportBatchDone { ok, failed } => {
                self.importing = false;
                let imported = std::mem::take(&mut self.imported_this_batch);
                if ok == 0 {
                    self.import_status.err(self.t("import_status_none"));
                } else {
                    let msg = self.tv("import_status_ok", &[("n", &imported.to_string()), ("total", &self.shapes.len().to_string())]);
                    self.import_status.ok(msg);
                }
                if failed > 0 {
                    self.console.error(format!("{failed} file(s) failed to import - see lines above"));
                }
            }

            Msg::RunStart(s) => {
                self.current_generations = s.generations.max(1);
                self.console.log(console::Kind::Run, format!("run {}/{}: rotations {}, population {}, {} generations", s.run, s.total_runs, s.rotations, s.population_size, s.generations));
            }
            Msg::Progress { generation, generations, best_fitness, sheets_used, unplaced_count, utilisation } => {
                self.progress = generation as f32 / generations.max(1) as f32;
                self.console.log(
                    console::Kind::Plain,
                    format!("gen {generation}/{generations}: fitness {best_fitness:.1}, {sheets_used} sheet(s), {unplaced_count} unplaced, {:.1}% used", utilisation),
                );
            }
            Msg::Tick { generation, individuals_done, individuals_total } => {
                // Sub-generation resolution, so a slow generation shows
                // movement instead of looking hung. Same arithmetic the web
                // UI used.
                let within = individuals_done as f32 / individuals_total.max(1) as f32;
                self.progress = ((generation.saturating_sub(1)) as f32 + within) / self.current_generations.max(1) as f32;
            }
            Msg::RunComplete(c) => {
                self.console.log(console::Kind::Run, format!("run {}/{} done: {} sheet(s), {} unplaced, {:.1}% used{}", c.run, c.total_runs, c.sheets_used, c.unplaced_count, c.utilisation, if c.improved { " (new best)" } else { "" }));
            }
            Msg::NestDone(result) => {
                self.running = false;
                self.progress = 0.0;
                match *result {
                    Ok(response) => {
                        let cancelled = response.cancelled;
                        self.console.log(
                            console::Kind::Best,
                            format!(
                                "nest {}: fitness {:.1}, {} sheet(s), {} unplaced, {:.1}% used",
                                if cancelled { "cancelled" } else { "complete" },
                                response.fitness,
                                response.placements.len(),
                                response.unplaced_count,
                                response.utilisation
                            ),
                        );
                        self.adopt_response(response);
                        self.run_status.ok(self.t(if cancelled { "run_status_stopped" } else { "run_status_done" }));
                    }
                    Err(e) => {
                        // Run errors go to the console, not the inline strip:
                        // an engine message is long and technical, and the
                        // strip is one line under a button.
                        self.console.error(format!("nest failed: {e}"));
                        self.run_status.err(self.t("run_status_failed"));
                    }
                }
            }

            Msg::Repacked(result) => {
                let index = self.repacking.take();
                match (*result, index) {
                    (Ok(response), Some(i)) => {
                        let improved = self.apply_repack(i, response);
                        self.run_status.ok(self.t(if improved { "repack_status_improved" } else { "repack_status_no_improvement" }));
                    }
                    (Err(e), _) => {
                        self.console.error(format!("repack failed: {e}"));
                        self.run_status.err(self.t("repack_status_failed"));
                    }
                    (Ok(_), None) => {}
                }
            }
            Msg::Validated(result) => self.finish_drag(*result),
            Msg::StoreLoaded(result) | Msg::StoreSaved(result) => match *result {
                Ok(store) => {
                    self.console.log(console::Kind::Plain, format!("library: {} saved shape(s)", store.shapes.len()));
                    self.store = store;
                }
                Err(e) => {
                    // Deliberately loud. An unreadable store must never be
                    // mistaken for an empty one - that is how someone loses a
                    // library they spent months building without noticing.
                    self.console.error(format!("library: {e}"));
                    self.run_status.err(self.t("library_error"));
                }
            },

            Msg::RemnantsComputed(result) => {
                self.harvesting = false;
                match *result {
                    Ok(remnants) => {
                        self.console.log(console::Kind::Plain, format!("offcuts: found {}", remnants.len()));
                        self.remnants = remnants;
                    }
                    Err(e) => {
                        self.console.error(format!("offcut scan failed: {e}"));
                        self.remnants.clear();
                    }
                }
            }

            Msg::Audited(result) => {
                self.auditing = false;
                match *result {
                    Ok(report) => {
                        if !report.passed {
                            // Loud in the console as well as on the badge: a
                            // fatal issue is the one thing here that must not
                            // be missed by someone not looking at the panel.
                            self.console.error(format!("audit: {} fatal issue(s), {} warning(s)", report.fatal_count, report.warning_count));
                        } else if report.warning_count > 0 {
                            self.console.log(console::Kind::Plain, format!("audit: passed with {} warning(s)", report.warning_count));
                        } else {
                            self.console.log(console::Kind::Plain, "audit: passed".to_string());
                        }
                        self.audit = Some(report);
                    }
                    Err(e) => {
                        self.console.error(format!("audit failed: {e}"));
                        self.audit = None;
                    }
                }
            }
            Msg::Exported { format, result } => match result {
                Ok(()) => {
                    self.export_status.ok(self.t("export_status_done"));
                    self.console.log(console::Kind::Plain, format!("exported {}", format.label()));
                }
                Err(e) => {
                    self.export_status.err(e.clone());
                    self.console.error(format!("export failed: {e}"));
                }
            },

            Msg::Loaded { config, best, errors } => {
                for e in errors {
                    self.console.error(e);
                }
                if let Some(c) = config {
                    self.cfg.from_dto(&c);
                    self.console.log(console::Kind::Plain, "restored saved config");
                }
                if let Some(b) = best {
                    self.recover_prompt = Some(Box::new(b));
                }
            }
            Msg::Log(line) => self.console.log(console::Kind::Plain, line),
        }
    }

    fn push_shape(&mut self, file: String, poly: PolygonDto) {
        self.shapes.push(ShapeRow::new(self.next_ui_id, file, poly));
        self.next_ui_id += 1;
    }

    /// Area of the largest shape currently marked SHEET - the reference the
    /// DOMINANT indicator compares against.
    ///
    /// ponytail: deliberately the *largest* sheet, which under-flags when a
    /// job mixes sheet sizes (a part can be dominant on the small sheet and
    /// not on the big one). Under-flagging is the safe direction: it never
    /// claims a part closes a sheet when it doesn't.
    fn largest_sheet_area(&self) -> f64 {
        self.shapes.iter().filter(|s| s.role == state::Role::Sheet).map(|s| s.area).fold(0.0, f64::max)
    }

    fn adopt_response(&mut self, response: crate::dto::RunNestResponse) {
        self.parts_by_id = response.parts_by_id;
        self.part_rules = response.part_rules;
        self.history = if response.history.is_empty() {
            vec![NestSnapshotDto {
                generation: 0,
                placements: response.placements.clone(),
                fitness: response.fitness,
                utilisation: response.utilisation,
                unplaced_count: response.unplaced_count,
                unplaced_ids: response.unplaced_ids.clone(),
            }]
        } else {
            response.history
        };
        self.history_index = self.history.len() - 1;
        // A fresh run replaces the result outright - undoing into edits made
        // against the *previous* one would restore a nest that no longer
        // matches the parts list.
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.snapshot = Some(Snapshot::from_history(&self.history[self.history_index]));
        self.consume_used_remnants();
        self.request_audit();
    }

    /// Marks every library remnant that this run actually nested onto as
    /// consumed, and writes the store.
    ///
    /// Without this the offcut shelf only ever grows: the same physical piece
    /// of material stays on the list and gets offered again for every future
    /// job, which makes the whole shelf untrustworthy - the one thing a
    /// materials list cannot afford to be.
    ///
    /// Only sheets that received at least one part count. A remnant that was
    /// offered to the nest and left empty is still sitting on the shelf.
    fn consume_used_remnants(&mut self) {
        let Some(snapshot) = &self.snapshot else { return };
        let used: std::collections::HashSet<usize> = snapshot
            .placements
            .iter()
            .filter(|p| !p.parts.is_empty())
            .filter_map(|p| self.sheet_origin.get(p.sheet_index).copied().flatten())
            .collect();
        if used.is_empty() {
            return;
        }
        let consumed = used.iter().filter(|id| self.store.consume(**id)).count();
        if consumed == 0 {
            return;
        }
        self.console.log(console::Kind::Plain, format!("library: {consumed} offcut(s) consumed by this nest"));
        let store = self.store.clone();
        self.worker.save_store(store);
    }

    /// Re-checks the displayed result for manufacturability.
    ///
    /// Called after every change to what is on screen - a finished run, a
    /// committed drag, a repack, or switching to a different attempt - because
    /// a badge that can outlive the arrangement it describes is worse than no
    /// badge: it says "checked" about something nobody checked.
    ///
    /// Clears the previous verdict first, so the gap between the edit and the
    /// answer reads as "unknown" rather than as the stale pass.
    fn request_audit(&mut self) {
        self.audit = None;
        // Offcuts describe one specific arrangement. Keeping them across a
        // change would offer the user a remnant that no longer exists.
        self.remnants.clear();
        let Some(snapshot) = &self.snapshot else { return };
        // Only with the config the result was actually produced under: margin
        // and spacing decide the answer, and defaults would check clearances
        // this nest was never asked to honour.
        let Some(config) = self.result_config.clone() else { return };
        if self.result_sheets.is_empty() {
            return;
        }
        self.auditing = true;
        self.worker.audit(crate::dto::AuditRequest {
            sheets: self.result_sheets.clone(),
            placements: snapshot.placements.clone(),
            parts_by_id: self.parts_by_id.clone(),
            config,
        });
    }

    fn apply_repack(&mut self, index: usize, response: crate::dto::RepackSheetResponse) -> bool {
        // Both guards run *before* `push_undo`, not after: an undo entry for a
        // repack that never happened costs the user a Ctrl-Z that reports
        // "undid the last change" while nothing on screen moves.
        if !matches!(&self.snapshot, Some(snap) if index < snap.placements.len()) {
            return false;
        }
        self.push_undo();
        let snap = self.snapshot.as_mut().expect("checked directly above");
        snap.placements[index] = response.placement;
        // A repack rearranges a whole sheet, so its verdict is the one most
        // worth re-earning.
        self.request_audit();
        response.improved
    }

    /// Remembers the current result so the next edit to it can be undone.
    /// Call immediately *before* mutating, not after.
    fn push_undo(&mut self) {
        let Some(snap) = &self.snapshot else { return };
        // Bounded: a long editing session should not grow without limit, and
        // nobody undoes further back than this by hand.
        const MAX_UNDO: usize = 32;
        if self.undo_stack.len() == MAX_UNDO {
            self.undo_stack.remove(0);
        }
        self.undo_stack.push(snap.clone());
        // A fresh edit forks the history: whatever was undone is no longer
        // reachable from here, and offering it would paste an unrelated
        // arrangement over the current one.
        self.redo_stack.clear();
    }

    /// Restores the result as it was before the last drag or repack.
    fn undo(&mut self) {
        if step_history(&mut self.undo_stack, &mut self.redo_stack, &mut self.snapshot) {
            self.request_audit();
            self.run_status.ok(self.t("undo_done"));
        } else {
            self.run_status.ok(self.t("undo_nothing"));
        }
    }

    /// Re-applies the last undone change. Only reachable until the next edit,
    /// which clears the stack - see `push_undo`.
    fn redo(&mut self) {
        if step_history(&mut self.redo_stack, &mut self.undo_stack, &mut self.snapshot) {
            self.request_audit();
            self.run_status.ok(self.t("redo_done"));
        } else {
            self.run_status.ok(self.t("redo_nothing"));
        }
    }

    /// Also returns, alongside the request, which library entry each expanded
    /// sheet came from - see `App::sheet_origin`.
    fn build_request(&self) -> Result<(RunNestRequest, Vec<Option<usize>>), String> {
        let mut sheets = Vec::new();
        let mut sheet_origin: Vec<Option<usize>> = Vec::new();
        let mut parts = Vec::new();
        for row in &self.shapes {
            // Quantity 0 means "excluded" for BOTH roles. The web UI used to
            // force a sheet row to contribute at least one sheet regardless
            // of its QTY, an undocumented asymmetry that made the field look
            // broken for that role.
            if row.qty == 0 {
                continue;
            }
            match row.role {
                state::Role::Sheet => {
                    sheets.extend(std::iter::repeat_n(row.poly.clone(), row.qty));
                    sheet_origin.extend(std::iter::repeat_n(row.from_store, row.qty));
                }
                state::Role::Part => parts.push(crate::dto::PartDto {
                    polygon: row.poly.clone(),
                    quantity: row.qty,
                    allowed_rotations: row.rot.angles(),
                    mirror: row.mirror.as_option(),
                }),
                state::Role::Skip => {}
            }
        }
        if sheets.is_empty() {
            return Err(self.t("run_need_sheet").to_string());
        }
        if parts.is_empty() {
            return Err(self.t("run_need_part").to_string());
        }
        if let Some(field) = self.cfg.first_nan_field() {
            return Err(self.tv("run_invalid_config_field", &[("field", self.t(field))]));
        }
        Ok((RunNestRequest { sheets, parts, config: self.cfg.to_dto() }, sheet_origin))
    }

    fn start_run(&mut self) {
        let (request, sheet_origin) = match self.build_request() {
            Ok(r) => r,
            Err(e) => {
                self.run_status.err(e);
                return;
            }
        };
        self.worker.save_config(request.config.clone());
        self.sheet_origin = sheet_origin;
        self.result_sheets = request.sheets.clone();
        self.result_config = Some(request.config.clone());
        match self.worker.nest(request) {
            Ok(()) => {
                self.running = true;
                self.progress = 0.0;
                self.export_status.clear();
                self.run_status.ok(self.t("run_status_running"));
                // Collapse everything so the result has room the moment it
                // arrives, and so a long run isn't spent staring at inputs
                // that are locked anyway.
                self.shapes_collapsed = true;
                self.settings_open = false;
                self.advanced_open = false;
                self.console.log(console::Kind::Run, "nest started");
            }
            Err(e) => self.run_status.err(e),
        }
    }

    fn reset(&mut self) {
        self.shapes.clear();
        self.snapshot = None;
        self.history.clear();
        self.parts_by_id.clear();
        self.part_rules.clear();
        self.result_sheets.clear();
        self.result_config = None;
        self.import_status.clear();
        self.run_status.clear();
        self.export_status.clear();
        self.select_all = false;
        // Clears the persisted best result too, so the reset survives a
        // restart instead of the recovery prompt resurrecting what was just
        // thrown away. Config values are deliberately left alone.
        self.worker.clear_best_result();
        self.console.log(console::Kind::Run, "reset");
    }

    fn recover(&mut self, best: BestResultDto) {
        self.parts_by_id = best.parts_by_id;
        self.part_rules = best.part_rules;
        self.result_sheets = best.sheets;
        self.result_config = best.config;
        self.history.clear();
        self.history_index = 0;
        self.snapshot = Some(Snapshot {
            placements: best.placements,
            fitness: best.fitness,
            utilisation: best.utilisation,
            unplaced_count: best.unplaced_count,
            unplaced_ids: best.unplaced_ids,
            locked: Default::default(),
        });
        self.request_audit();
        self.console.log(console::Kind::Best, "recovered the best result from a previous session");
    }
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, PREFS_KEY, &self.prefs);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump();
        import::handle_dropped_files(self, ctx);
        keys::handle(self, ctx);

        shell::header(self, ctx);
        shell::bottom_bar(self, ctx);
        // Side panels before the central one, as egui requires: they claim
        // their width first and the central column gets what is left.
        shell::config_panel(self, ctx);
        console::panel(self, ctx);
        shell::run_float(self, ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                import::panel(self, ui);
                library::panel(self, ui);
                shapes::panel(self, ui);
                result::panel(self, ui);
                // Clearance for the floating RUN control, which is anchored
                // over this scroll area rather than inside it - without this
                // the last panel's own controls sit underneath it.
                ui.add_space(130.0);
            });
        });

        shell::dialogs(self, ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(fitness: f64) -> Snapshot {
        Snapshot { placements: Vec::new(), fitness, utilisation: 0.0, unplaced_count: 0, unplaced_ids: Vec::new(), locked: Default::default() }
    }

    /// Undo then redo has to land exactly back where it started, however many
    /// times it is done - the property the whole feature is, and the one a
    /// second hand-written stack juggle would break.
    #[test]
    fn undo_and_redo_are_inverse() {
        let (mut undo, mut redo) = (vec![snap(1.0), snap(2.0)], Vec::new());
        let mut current = Some(snap(3.0));

        assert!(step_history(&mut undo, &mut redo, &mut current));
        assert_eq!(current.as_ref().unwrap().fitness, 2.0);
        assert!(step_history(&mut undo, &mut redo, &mut current));
        assert_eq!(current.as_ref().unwrap().fitness, 1.0);
        assert!(!step_history(&mut undo, &mut redo, &mut current), "nothing left to undo");
        assert_eq!(current.as_ref().unwrap().fitness, 1.0, "a failed step must not move anything");

        assert!(step_history(&mut redo, &mut undo, &mut current));
        assert!(step_history(&mut redo, &mut undo, &mut current));
        assert_eq!(current.as_ref().unwrap().fitness, 3.0, "back where it started");
        assert!(redo.is_empty());
        assert_eq!(undo.len(), 2, "the undo stack must be rebuilt, not duplicated");
        assert!(!step_history(&mut redo, &mut undo, &mut current));
    }
}
