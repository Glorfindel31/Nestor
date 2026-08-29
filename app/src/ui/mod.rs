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
//! Layout: a header strip, then the three numbered steps of the actual job -
//! 01 IMPORT, 02 ASSIGN ROLES, 03 RESULT - plus a floating RUN control, a
//! floating console, and CONFIGURE in a right-hand side panel.
//!
//! **CONFIGURE is deliberately not numbered.** It used to be "03", which put
//! a hole in the sequence the central column shows: a side panel is not
//! somewhere the eye travels between 02 and RESULT, so the column read
//! 01 - 02 - 04 and the numbering was making a promise the layout did not
//! keep. It is also not a step anyone must pass through: the defaults are
//! meant to work (see `PRODUCT.md`'s third product principle), so it is
//! parameters for the run, reachable when wanted, not a stage of it.

mod canvas;
mod config;
mod console;
mod effects;

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

/// Console-only, and deliberately English: the operator who just picked a
/// language whose glyphs cannot be drawn is looking at boxes, so a
/// translated warning would be one more row of them.
const MISSING_CJK_FONT: &str = "no CJK font found on this system - Japanese, Korean and Chinese text will show as empty boxes. Install one (Windows: Settings > Time & Language > Language > add the language pack; Linux: the noto-fonts-cjk package) and restart.";

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

/// Groups audit issues into one log line: how many of each kind, on which
/// sheets, and which parts. Without it the log records a bare count, which
/// says a nest is unsafe but nothing about why - see the call site.
///
/// Part ids are capped because a wholesale failure lists every part on the
/// sheet, and a log line hundreds of ids long is no more useful than a short
/// one. The kinds come from `dto::AuditIssueDto` (`overlap`, `outside_sheet`,
/// `below_spacing`, `outside_margin`).
fn audit_breakdown(issues: &[crate::dto::AuditIssueDto]) -> String {
    use std::collections::{BTreeMap, BTreeSet};
    const MAX_IDS: usize = 8;

    // BTree, not Hash: the line is diffed between runs, so a stable order
    // matters more than the lookup speed of at most four keys.
    let mut by_kind: BTreeMap<&str, (usize, BTreeSet<usize>, BTreeSet<usize>)> = BTreeMap::new();
    for issue in issues {
        let entry = by_kind.entry(issue.kind.as_str()).or_insert_with(|| (0, BTreeSet::new(), BTreeSet::new()));
        entry.0 += 1;
        entry.1.insert(issue.sheet_index);
        entry.2.extend(issue.part_ids.iter().copied());
    }

    let join = |values: &BTreeSet<usize>, cap: usize| {
        let mut out: Vec<String> = values.iter().take(cap).map(usize::to_string).collect();
        if values.len() > cap {
            out.push(format!("+{} more", values.len() - cap));
        }
        out.join(",")
    };
    by_kind
        .iter()
        .map(|(kind, (count, sheets, parts))| format!("{kind} x{count} on sheet(s) {} (parts {})", join(sheets, MAX_IDS), join(parts, MAX_IDS)))
        .collect::<Vec<_>>()
        .join("; ")
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
    /// How many files the running import batch covers - 0 when idle.
    /// A count rather than a flag because `import_importing` interpolates it.
    importing: usize,
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
    /// Where a shift-click range in the shapes table extends *from* - the
    /// visible row index of the last plain checkbox click.
    select_anchor: Option<usize>,
    /// Which quantity field held focus *last* frame. Tab has to be answered
    /// from that, not from `Response::has_focus`: by the time the response
    /// exists, egui has already taken the focus away to hand it on.
    qty_focus: Option<egui::Id>,
    /// What the bulk-apply row is currently set to. Held on `App` rather than
    /// read back out of the table, because these are the *pending* values -
    /// nothing is written to any row until the matching APPLY is pressed.
    bulk_rot: state::RotRule,
    bulk_mirror: state::MirrorRule,
    bulk_qty: usize,
    /// Substring the shapes table is filtered by. Empty shows everything.
    shape_filter: String,
    confirm_remove: bool,

    // ---- CONFIGURE ----
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
    /// The layout the engine is building right now, replaced wholesale every
    /// time a `Msg::Live` frame arrives and cleared when the run ends.
    ///
    /// Separate from `snapshot` rather than writing into it: `snapshot` is
    /// the result the user can drag, pin, repack and export, and a live
    /// frame is none of those things. Keeping them apart means a cancelled
    /// run leaves the previous *finished* result on screen untouched.
    live: Option<Snapshot>,
    /// The part the engine is choosing a position for, and every position it
    /// scored for it. Drawn as outlines under the committed parts.
    live_ghost: Option<crate::worker::GhostSet>,


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
    /// What caused the audit currently in flight - "a nest run", "a repack",
    /// "a drag", and so on. Seven different actions request an audit, and
    /// until this existed the log recorded only the verdict, so a result that
    /// came back with 31 fatal issues could not be attributed to the action
    /// that produced it. The audit is asynchronous, so the reason has to be
    /// parked here rather than passed through the worker.
    audit_reason: &'static str,
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
    /// Quantity to stamp on every row of the sample job currently importing
    /// (`import::load_preset`), cleared when that batch finishes.
    preset_qty: Option<usize>,
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
    /// Cleared once the first frame has re-applied the style with real font
    /// metrics - see `theme::apply`'s `fonts_ready`.
    metrics_pending: bool,
    /// Set only if GitHub reported a release newer than this build. Drives
    /// the header badge; `None` covers "current" and "could not check".
    update: Option<crate::update::Release>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let prefs: prefs::Prefs = cc.storage.and_then(|s| eframe::get_value(s, PREFS_KEY)).unwrap_or_default();
        // Before `apply`, which reads the palette it selects.
        theme::set(prefs.theme);
        theme::apply(&cc.egui_ctx, prefs.scale.factor(), false);

        // Once, here - not inside `apply`, which reruns on every TEXT SIZE
        // change. See `install_fonts`.
        let cjk_ok = theme::install_fonts(&cc.egui_ctx, prefs.lang);
        // Explicitly 1.0, not merely left alone: egui persists the zoom
        // factor in its own memory, so a version that once set it would
        // otherwise keep scaling strokes here forever.
        cc.egui_ctx.set_zoom_factor(1.0);

        let worker = Worker::new(cc.egui_ctx.clone());
        // The engine reads this flag, not `prefs` - carry the saved
        // preference across before any run can start.
        worker.live.store(prefs.live_view, std::sync::atomic::Ordering::Relaxed);
        worker.load_saved();
        // Once per launch, on its own thread like everything else.
        worker.check_update();


        let mut app = Self {
            help_open: !prefs.help_dismissed,
            prefs,
            worker,
            console: Default::default(),
            import_status: Default::default(),
            importing: 0,
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
            select_anchor: None,
            qty_focus: None,
            bulk_rot: state::RotRule::Any,
            bulk_mirror: state::MirrorRule::Job,
            bulk_qty: 1,
            shape_filter: String::new(),
            confirm_remove: false,
            cfg: Default::default(),
            settings_open: false,
            console_open: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            advanced_open: false,
            running: false,
            live: None,
            live_ghost: None,


            progress: 0.0,
            run_status: Default::default(),
            current_generations: 1,
            snapshot: None,
            audit: None,
            auditing: false,
            audit_reason: "startup",
            store: Default::default(),
            store_open: false,
            remnants: Vec::new(),
            harvesting: false,
            history: Vec::new(),
            history_index: 0,
            parts_by_id: Default::default(),
            preset_qty: None,
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
            update: None,
            confirm_reset: false,
            recover_prompt: None,
            metrics_pending: true,
        };
        app.console.log(console::Kind::Run, "Nestor started");
        app.worker.load_store();
        if !cjk_ok {
            app.console.log(console::Kind::Error, MISSING_CJK_FONT.to_owned());
        }
        app
    }

    /// Switches language.
    ///
    /// The three status lines are cleared rather than re-resolved: every
    /// other label in this UI goes through `t()` every frame and follows the
    /// switch on its own, but those hold text that was resolved once, when
    /// the event happened, and would sit there in the old language until the
    /// next action replaced them.
    ///
    /// ponytail: cleared, not re-resolved. Carrying the key and its arguments
    /// on `Status` would let them survive the switch, and is the upgrade if
    /// these ever hold something worth keeping - today they are transient
    /// feedback about the last action, and an empty line beats a stale one in
    /// the wrong language.
    pub fn set_lang(&mut self, ctx: &egui::Context, lang: i18n::Lang) {
        if self.prefs.lang == lang {
            return;
        }
        self.prefs.lang = lang;
        // Rebuilt on every switch, not only into or out of a CJK language:
        // the loaded CJK faces are ordered with the current language's own
        // first, because Japanese and Chinese draw some shared characters
        // differently and the first face to claim a codepoint wins it.
        if !theme::install_fonts(ctx, lang) {
            self.console.log(console::Kind::Error, MISSING_CJK_FONT.to_owned());
        }
        self.import_status.clear();
        self.run_status.clear();
        self.export_status.clear();
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
            Msg::Imported { file, shapes, size_guessed } => {
                self.console.log(console::Kind::Plain, format!("imported {} shape(s) from {file}", shapes.len()));
                if size_guessed {
                    // Loud on purpose: this is the one import that succeeds
                    // perfectly at the wrong size, and nothing downstream -
                    // not the preview, not the audit, not the export - can
                    // tell. Only the person who drew it can.
                    let warning = self.tv("import_size_guessed", &[("file", &file)]);
                    self.console.error(warning.clone());
                    self.import_status.err(warning);
                }
                self.imported_this_batch += shapes.len();
                for poly in shapes {
                    self.push_shape(file.clone(), poly);
                    // A sample job's parts arrive at the quantity that makes
                    // it a job rather than a single lonely part.
                    if let (Some(qty), Some(row)) = (self.preset_qty, self.shapes.last_mut()) {
                        row.qty = qty;
                    }
                }
            }
            Msg::ImportFailed { file, error } => {
                self.console.error(format!("import failed for {file}: {error}"));
            }
            Msg::ImportBatchDone { ok, failed } => {
                self.importing = 0;
                self.preset_qty = None;
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
            Msg::PartsReady(parts) => {
                // Only useful while a run is live; `adopt_response` replaces
                // this wholesale with the finished run's own map.
                self.parts_by_id = parts;
            }
            Msg::Live { placements, ghost } => {

                // A frame that arrives after the run finished (the last one
                // can still be in the channel behind `NestDone`) must not
                // resurrect the live view over the real result.
                if !self.running {
                    return;
                }
                // fitness/utilisation/unplaced are left at zero and never
                // shown: a partial layout has no honest value for any of
                // them (utilisation of a sheet still being filled means
                // nothing, and every part not yet placed would count as
                // unplaced). `result::panel` skips the stats row while the
                // live view is what's on screen - see its own comment.
                self.live = Some(Snapshot {
                    placements,
                    fitness: 0.0,
                    utilisation: 0.0,

                    unplaced_count: 0,
                    unplaced_ids: Vec::new(),
                    locked: Default::default(),
                });
                self.live_ghost = ghost;
            }

            Msg::RunComplete(c) => {

                self.console.log(console::Kind::Run, format!("run {}/{} done: {} sheet(s), {} unplaced, {:.1}% used{}", c.run, c.total_runs, c.sheets_used, c.unplaced_count, c.utilisation, if c.improved { " (new best)" } else { "" }));
            }
            Msg::NestDone(result) => {
                self.running = false;
                self.progress = 0.0;
                self.live = None;
                self.live_ghost = None;


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
                        // In the console, not only on the status line: a
                        // repack that succeeded left no trace in the log at
                        // all, so a bad arrangement afterwards could not be
                        // told apart from a bad drag. The status line is
                        // transient and the log is what gets read later.
                        self.console.log(
                            console::Kind::Plain,
                            format!(
                                "repack sheet {}: {}, {:.1}% used",
                                response.placement.sheet_index,
                                if response.improved { "improved" } else { "no improvement, kept as-is" },
                                response.utilisation
                            ),
                        );
                        let improved = self.apply_repack(i, response);
                        self.run_status.ok(self.t(if improved { "repack_status_improved" } else { "repack_status_no_improvement" }));
                    }
                    (Err(e), _) => {
                        self.console.error(format!("repack failed: {e}"));
                        let n = index.map_or_else(|| "?".to_string(), |i| (i + 1).to_string());
                        self.run_status.err(self.tv("repack_status_failed", &[("n", &n)]));
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
                        let after = self.audit_reason;
                        if !report.passed {
                            // Loud in the console as well as on the badge: a
                            // fatal issue is the one thing here that must not
                            // be missed by someone not looking at the panel.
                            //
                            // The breakdown is not decoration. "31 fatal
                            // issue(s)" cannot distinguish 31 parts hanging
                            // off the sheet from 31 pairwise overlaps, and
                            // those point at different bugs - a real report
                            // of exactly that was unattributable for want of
                            // this line.
                            self.console.error(format!(
                                "audit after {after}: {} fatal issue(s), {} warning(s) - {}",
                                report.fatal_count,
                                report.warning_count,
                                audit_breakdown(&report.issues)
                            ));
                        } else if report.warning_count > 0 {
                            self.console.log(
                                console::Kind::Plain,
                                format!("audit after {after}: passed with {} warning(s) - {}", report.warning_count, audit_breakdown(&report.issues)),
                            );
                        } else {
                            self.console.log(console::Kind::Plain, format!("audit after {after}: passed"));
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
            Msg::UpdateAvailable(release) => {
                self.console.log(console::Kind::Plain, format!("update available: v{}", release.version));
                self.update = Some(release);
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

    /// Switches the visual world.
    ///
    /// Two calls rather than one because they cost very different things:
    /// `apply` clones and stores a `Style`, while `install_fonts` throws away
    /// and rebuilds the entire glyph atlas. Only the second is expensive, and
    /// only a theme change needs it - which is why TEXT SIZE, next to this in
    /// the menu, calls `apply` alone.
    pub fn set_theme(&mut self, ctx: &egui::Context, theme: theme::Theme) {
        if self.prefs.theme == theme {
            return;
        }
        self.prefs.theme = theme;
        theme::set(theme);
        let _ = theme::install_fonts(ctx, self.prefs.lang);
        theme::apply(ctx, self.prefs.scale.factor(), true);
        self.console.log(console::Kind::Plain, format!("theme: {}", theme.label()));
    }

    /// Turns the live view on or off, including part way through a run.
    ///
    /// The flag the engine actually reads lives on the worker and is shared
    /// with the running job, so this takes effect on the next part placed
    /// rather than the next run. Switching off also drops the partial layout
    /// immediately - leaving it on screen frozen at whatever part it had
    /// reached would read as the run having stalled there.
    pub fn set_live_view(&mut self, on: bool) {
        self.prefs.live_view = on;
        self.worker.live.store(on, std::sync::atomic::Ordering::Relaxed);
        if !on {
            self.live = None;
            self.live_ghost = None;
        }
    }

    /// The snapshot the RESULT panel is currently drawing.
    ///
    /// While a nest runs with the live view on, that is the partial layout
    /// the engine is building; otherwise it is the finished result. The
    /// fallback to `snapshot` matters: with the live view off, a run must
    /// leave the *previous* result on screen rather than blanking the panel
    /// for the duration.
    pub fn shown(&self) -> Option<&Snapshot> {
        if self.running {
            self.live.as_ref().or(self.snapshot.as_ref())
        } else {
            self.snapshot.as_ref()
        }
    }

    /// True when what's on screen is a nest in progress, not a result. The
    /// stats row, the history selector and every editing affordance are
    /// meaningless against a half-built layout.
    pub fn showing_live(&self) -> bool {
        self.running && self.live.is_some()
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
        self.request_audit("a nest run");
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
    fn request_audit(&mut self, reason: &'static str) {
        self.audit = None;
        self.audit_reason = reason;
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
        self.request_audit("a repack");
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
            self.request_audit("an undo");
            self.run_status.ok(self.t("undo_done"));
        } else {
            self.run_status.ok(self.t("undo_nothing"));
        }
    }

    /// Re-applies the last undone change. Only reachable until the next edit,
    /// which clears the stack - see `push_undo`.
    fn redo(&mut self) {
        if step_history(&mut self.redo_stack, &mut self.undo_stack, &mut self.snapshot) {
            self.request_audit("a redo");
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
                // Nothing from a previous run may show through under the
                // first frames of this one.
                self.live = None;
                self.live_ghost = None;

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
        self.request_audit("recovering the saved best result");
        self.console.log(console::Kind::Best, "recovered the best result from a previous session");
    }
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, PREFS_KEY, &self.prefs);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // First frame only: `App::new` had to size the buttons from a
        // constant because egui had no font atlas yet. It has one now, so
        // re-apply with the label's real row height before anything draws.
        if std::mem::take(&mut self.metrics_pending) {
            theme::apply(ctx, self.prefs.scale.factor(), true);
        }
        self.pump();
        import::handle_dropped_files(self, ctx);
        keys::handle(self, ctx);

        // Behind every panel, then over everything: the active theme's own
        // motion. Both take no layout space and accept no input - see
        // `effects`'s own module comment.
        effects::background(ctx);

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

        effects::foreground(ctx);
        shell::dialogs(self, ctx);

    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(fitness: f64) -> Snapshot {
        Snapshot { placements: Vec::new(), fitness, utilisation: 0.0, unplaced_count: 0, unplaced_ids: Vec::new(), locked: Default::default() }
    }

    /// The whole point of the breakdown is telling one failure mode from
    /// another in the log, so the kinds must stay separated and counted. The
    /// id cap is what keeps a wholesale failure - every part on a sheet at
    /// once, which is exactly the case this was written for - to one readable
    /// line instead of hundreds of ids.
    #[test]
    fn audit_breakdown_separates_kinds_and_caps_the_id_list() {
        let issue = |kind: &str, sheet: usize, parts: Vec<usize>| crate::dto::AuditIssueDto { kind: kind.to_string(), fatal: true, sheet_index: sheet, part_ids: parts };

        let mixed = vec![issue("overlap", 0, vec![1, 2]), issue("outside_sheet", 1, vec![7]), issue("overlap", 0, vec![2, 3])];
        let line = audit_breakdown(&mixed);
        assert!(line.contains("overlap x2 on sheet(s) 0 (parts 1,2,3)"), "got: {line}");
        assert!(line.contains("outside_sheet x1 on sheet(s) 1 (parts 7)"), "got: {line}");

        let wholesale: Vec<_> = (0..30).map(|id| issue("outside_sheet", 0, vec![id])).collect();
        let line = audit_breakdown(&wholesale);
        assert!(line.contains("outside_sheet x30"), "the count is never capped: {line}");
        assert!(line.contains("+22 more"), "only the id list is capped: {line}");

        assert_eq!(audit_breakdown(&[]), "", "a clean report adds nothing to the line");
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
