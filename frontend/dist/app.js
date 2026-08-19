// Minimal UI: talks directly to the Rust engine via Tauri's
// import_dxf_command/import_svg_command/run_nest_command. Deliberately not
// an adaptation of the legacy Ractive UI (frontend/deepnest.js,
// frontend/ui/**) - that code assumes a Node-integrated Electron renderer
// (require("electron"), require("@electron/remote"), require("axios"),
// etc.) that doesn't exist in Tauri's webview, and its own SVG import
// (svgparser.js) targets the DOM parser (`DOMParser`/imperial-unit-aware
// CSS length resolution) that isn't available/desired here - this UI's SVG
// import goes through the native `geometry::svg_import` module instead
// (metric-only, see that module's doc comment). Legacy files kept as
// reference, not wired up.

import { boundsOf, toSvgPoints, pointsToPath, rotatedTranslatedPoints, colorForLayer, renderShapeSvg, UNPLACED_COLOR } from "./render.js";
import { t, getLang, setLang, applyStaticTranslations } from "./i18n.js";
import { getAccent, getScaleName, setAccent, setScale, applySavedPrefs } from "./prefs.js";

const invoke = window.__TAURI__.core.invoke;

/** @type {{layer: string, points: {x:number,y:number}[], is_circle: unknown, children: unknown[], real_boundary: unknown, _uiId: number}[]} */
let importedShapes = [];

// Stable per-shape identity for table rows, independent of array position -
// `renderShapesTable` only ever appends rows (never rebuilds, so a role/qty
// the user already set survives a later import), so removing a shape from
// the middle of `importedShapes` must not shift what `data-role`/`data-qty`
// on every *other* row's `<select>`/`<input>` refers to. A plain array index
// would do exactly that; this counter never gets reused or reassigned.
let nextShapeUiId = 0;

// Remembered across a run so a later action (picking a different history
// entry, exporting) doesn't need to re-invoke the engine - it's all
// already in the last response.
let lastNestRequest = null;
let currentSnapshot = null;
// The authoritative id -> shape mapping from the last run_nest_command
// response (RunNestResponse::parts_by_id) - used for both rendering and
// export, instead of each independently re-deriving its own id->shape
// mapping from request.parts/quantities (fragile: has to exactly mirror
// dto::expand_parts's sequential id assignment, and export_dxf used to do
// exactly this server-side too - see that command's own doc comment for
// the silent-corruption risk this replaces).
let lastPartsById = null;
// The per-part orientation rules the last run actually applied
// (RunNestResponse::part_rules). Kept alongside lastPartsById and sent back
// with every repack, so a manual repack can't rotate a grain-locked piece
// into an angle the nest itself was forbidden from using.
let lastPartRules = {};
// Set at the start of each run so the "nest-tick" listener below can turn
// (generation, individuals_done/individuals_total) into an overall
// percentage - the tick event itself doesn't carry the total generation
// count, only run_nest_command's own request did.
let currentGenerations = 0;

const el = (id) => document.getElementById(id);

// DXF layer names and import filenames are attacker-controlled text (a
// crafted DXF's layer name, or a crafted filename, can contain arbitrary
// HTML) that several places below interpolate into innerHTML for display.
// Tauri's CSP is null and custom commands need no capability grant here
// (see capabilities/default.json's own doc comment), so unescaped markup
// here isn't just a cosmetic XSS - it's a path to calling any
// import_dxf_command/export_dxf_command/etc. arbitrary command straight
// from a malicious .dxf file. Every raw layer/filename string interpolated
// into an innerHTML template must go through this first.
const escapeHtml = (str) => String(str).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

function setStatus(id, message, isError) {
  // Errors next to RUN NEST specifically go to the console instead of the
  // inline status text - that strip is small and easy to miss/overwrite
  // with the next status update, while the console keeps a scrollable
  // history of what actually went wrong.
  if (isError && id === "run-status") {
    el(id).textContent = "";
    logLine(message, "error");
    return;
  }
  const node = el(id);
  node.textContent = message;
  node.classList.toggle("error", Boolean(isError));
}

function setBusy(spinnerId, busy) {
  el(spinnerId).hidden = !busy;
}

// A running log of what the app is doing - import/run start, success,
// failure, and (via the "nest-progress"/"nest-run-start"/"nest-run-complete"
// events below) live per-generation and per-run stats while a run is in
// progress, instead of the UI just going quiet until the whole run returns.
// `kind` picks a color (see app.css's `.log-*` rules): "run" for a new
// escalating attempt starting, "best" for one that just beat every attempt
// before it, unset for plain informational lines.
// Capped, not unbounded - "nest-tick" alone fires once per individual
// placed inside a single generation (see its own listener's comment below),
// so a long session running several multi-generation nests could otherwise
// accumulate thousands of never-GC'd .log-line divs in a window that's
// never reloaded. The on-disk log (see the comment below) keeps the full,
// uncapped history regardless - this only bounds what stays live in the DOM.
const CONSOLE_LOG_MAX_LINES = 500;

function logLine(message, kind) {
  const node = el("console-log");
  const time = new Date().toLocaleTimeString();
  const line = `[${time}] ${message}`;
  const entry = document.createElement("div");
  entry.className = kind ? `log-line log-${kind}` : "log-line";
  entry.textContent = line;
  node.appendChild(entry);
  while (node.children.length > CONSOLE_LOG_MAX_LINES) {
    node.removeChild(node.firstChild);
  }
  node.scrollTop = node.scrollHeight;
  // Fire-and-forget: also append to the on-disk log
  // (<app_log_dir>/rustynesting.log) so this history survives past the
  // window closing - the console panel above is DOM-only and empties every
  // restart, which made a bad run impossible to look at after the fact.
  invoke("append_log_command", { line }).catch(() => {});
}

// Locks/unlocks every import/role/config control while a nest is running,
// so a config change mid-run can't silently apply to a request that's
// already in flight (buildRequest() already snapshotted its own copy, but
// letting the user edit fields that look "live" while ignored was
// confusing on its own).
// Locks #panel-result too, not just import/shapes/config - Export and the
// per-sheet REPACK buttons both read `lastNestRequest`/`currentSnapshot`/
// `lastPartsById`, which get reassigned to the *new* run's request the
// moment handleRunNest starts (before the new result actually lands) while
// the *old* result is still what's on screen. Without this, clicking
// Export/Repack mid-run mixes the new run's sheets with the old run's
// placements/parts_by_id - silently wrong if sheets changed between runs.
const CONTROLS_LOCKED_SELECTOR =
  "#panel-import input, #panel-import select, #panel-import button, #panel-shapes input, #panel-shapes select, #panel-shapes button, #panel-config input, #panel-config select, #panel-config button, #panel-result input, #panel-result select, #panel-result button";

function setControlsLocked(locked) {
  document.querySelectorAll(CONTROLS_LOCKED_SELECTOR).forEach((node) => {
    node.disabled = locked;
  });
}

function shapeThumbnailSvg(shape, strokeOverride = null) {
  const bounds = boundsOf(shape.points);
  const pad = Math.max(bounds.w, bounds.h, 1) * 0.08;
  const vbW = bounds.w + pad * 2;
  const vbH = bounds.h + pad * 2;
  const transform = (points) => toSvgPoints(points, bounds).map((p) => ({ x: p.x + pad, y: p.y + pad }));
  return `<svg class="thumb" viewBox="0 0 ${vbW.toFixed(2)} ${vbH.toFixed(2)}" width="56" height="56">${renderShapeSvg(shape, transform, true, strokeOverride)}</svg>`;
}

// Imports every path in `paths` (one import_dxf_command/import_svg_command
// call each, picked by extension - each command only reads a single file)
// and appends their shapes onto whatever was already imported, so multiple
// files/drops accumulate into one part pool instead of each import replacing
// the last. A failure on one file is logged and skipped rather than aborting
// the rest of the batch.
// Strips directory and extension from an import path, e.g.
// "C:\foo\bar\Untitled.dxf" -> "Untitled" - used to build each row's
// [filename-{number}] NAME column so a part can be told apart from same-
// named parts in a different file at a glance.
function fileBaseName(path) {
  const base = path.split(/[\\/]/).pop() ?? path;
  return base.replace(/\.[^.]+$/, "");
}

// DXF stays the primary/first-class import path; SVG import is additive
// (see geometry::svg_import's module doc - metric-only, imperial units are
// rejected with an error, not silently converted). Both commands return the
// same PolygonDto tree shape, so nothing past this dispatch needs to know
// which format a given shape came from.
function importCommandFor(path) {
  const ext = path.split(".").pop()?.toLowerCase();
  if (ext === "dxf") return "import_dxf_command";
  if (ext === "svg") return "import_svg_command";
  return null;
}

// A real SVG's width/height/viewBox often isn't a trustworthy physical
// size - plenty of design tools export unitless or arbitrary user units -
// so rather than trust auto-detection silently, every SVG import (browse or
// drag-drop, once per batch, not once per file) asks the user to confirm or
// override what one file unit represents. Resolves to "" for "auto-detect
// from file" (see index.html's <select> options), a concrete unit string
// ("mm"/"cm"/"m"/"px"), or `null` if the user cancelled - imperial units
// are never offered as a choice in the first place (see svg-unit-select's
// options), so there's nothing to reject here that geometry::svg_import
// wouldn't reject anyway.
let svgUnitPromptResolve = null;

function openSvgUnitPrompt() {
  return new Promise((resolve) => {
    svgUnitPromptResolve = resolve;
    el("svg-unit-select").value = "";
    el("svg-unit-overlay").hidden = false;
  });
}

function closeSvgUnitPrompt(result) {
  el("svg-unit-overlay").hidden = true;
  if (svgUnitPromptResolve) {
    svgUnitPromptResolve(result);
    svgUnitPromptResolve = null;
  }
}

el("btn-svg-unit-ok").addEventListener("click", () => closeSvgUnitPrompt(el("svg-unit-select").value));
el("btn-svg-unit-cancel").addEventListener("click", () => closeSvgUnitPrompt(null));
el("svg-unit-overlay").addEventListener("click", (event) => {
  if (event.target === el("svg-unit-overlay")) closeSvgUnitPrompt(null);
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !el("svg-unit-overlay").hidden) closeSvgUnitPrompt(null);
});

async function importPaths(paths) {
  if (paths.length === 0) return;
  const tolerance = Number(el("import-tolerance").value);

  // One prompt per batch, not per file - every .svg in this batch shares
  // whatever unit the user picks (or "auto-detect" left as-is).
  let svgUnitOverride = null;
  if (paths.some((p) => p.toLowerCase().endsWith(".svg"))) {
    const choice = await openSvgUnitPrompt();
    if (choice === null) {
      logLine("import cancelled: SVG unit prompt dismissed");
      return;
    }
    svgUnitOverride = choice || null; // "" (auto-detect) -> no override sent to the backend
  }

  setStatus("import-status", t("import_importing", { n: paths.length }), false);
  el("btn-import").disabled = true;
  setBusy("import-spinner", true);
  let imported = 0;
  for (const path of paths) {
    const command = importCommandFor(path);
    if (!command) {
      logLine(`import skipped for ${path}: unsupported file type (expected .dxf or .svg)`);
      continue;
    }
    logLine(`import: ${path} (tolerance ${tolerance}mm)`);
    try {
      const args = { path, curve_tolerance: tolerance };
      if (command === "import_svg_command") {
        args.unit_override = svgUnitOverride;
      }
      const shapes = await invoke(command, args);
      const fileName = fileBaseName(path);
      for (const shape of shapes) {
        shape._uiId = nextShapeUiId++;
        shape._file = fileName;
      }
      importedShapes = importedShapes.concat(shapes);
      imported += shapes.length;
      logLine(`import ok: ${shapes.length} shape(s) from ${path}`);
    } catch (err) {
      logLine(`import failed for ${path}: ${err}`);
    }
  }
  el("btn-import").disabled = false;
  setBusy("import-spinner", false);

  if (imported > 0) {
    setStatus("import-status", t("import_status_ok", { n: imported, total: importedShapes.length }), false);
    renderShapesTable();
    el("panel-shapes").hidden = false;
    el("panel-config").hidden = false;
  } else {
    setStatus("import-status", t("import_status_none"), true);
  }
}

// Bulk role assignment - sets every imported shape's ROLE select in one
// click instead of needing one click per row, the real friction point on a
// DXF with many separate profiles. Reuses the same delegated "change"
// listener's effect (`updateDominantIndicators`) since setting `.value`
// directly on a `<select>` doesn't fire a native "change" event on its own.
function markAllRoles(role) {
  document.querySelectorAll("#shapes-body [data-role]").forEach((select) => {
    select.value = role;
  });
  updateDominantIndicators();
}

// Builds the settings-bar label's markup rather than plain text, so the
// "03" step number can stay accent-colored (`.settings-bar .step-num`)
// independently of the language-varying text next to it - the same split
// `.panel h2` uses for the other three step headings, just built in JS
// since this one's label also toggles between a collapsed/expanded arrow.
function renderSettingsBarLabel(collapsed) {
  return `<span class="step-num">03</span> / ${t("settings_bar_text")} ${collapsed ? "▾" : "▴"}`;
}

function handleToggleShapes() {
  const body = el("shapes-collapsible");
  const button = el("btn-toggle-shapes");
  const collapsed = !body.hidden;
  body.hidden = collapsed;
  button.textContent = collapsed ? "▸" : "▾";
}

// Forces every collapsible section shut - called right as a nest run
// starts (see handleRunNest below). `setControlsLocked` already disables
// every field inside these while a run is in progress, so there's nothing
// left to look at in them mid-run anyway; collapsing keeps the console and
// progress bar - the only things actually worth watching - from competing
// for space with panels nobody can act on right now.
function collapseAllPanels() {
  el("shapes-collapsible").hidden = true;
  el("btn-toggle-shapes").textContent = "▸";
  el("advanced-collapsible").hidden = true;
  el("btn-toggle-advanced").textContent = t("btn_advanced_collapsed");
  el("settings-collapsible").hidden = true;
  el("settings-bar-label").innerHTML = renderSettingsBarLabel(true);
}

// Advanced settings (placement type, rotations/population/generations
// starting points, dominant area, threads, seed) start collapsed - the
// friction-free default only needs MARGIN/SPACING/RUNS above, see RUNS'
// own tooltip and commands.rs's escalation loop.
function handleToggleAdvanced() {
  const body = el("advanced-collapsible");
  const button = el("btn-toggle-advanced");
  const collapsed = !body.hidden;
  body.hidden = collapsed;
  button.textContent = collapsed ? t("btn_advanced_collapsed") : t("btn_advanced_expanded");
}

// The bottom bar's own drawer toggle - MARGIN/SPACING/RUNS and (nested
// inside) Advanced Settings collapse together behind this one handle, so
// the bar stays a slim accent strip (RUN/STOP live in their own #run-float
// button, unaffected by this toggle) until you actually want to change
// something.
function handleToggleSettings() {
  const body = el("settings-collapsible");
  const label = el("settings-bar-label");
  const collapsed = !body.hidden;
  body.hidden = collapsed;
  label.innerHTML = renderSettingsBarLabel(collapsed);
}

async function handleBrowse() {
  let selected;
  try {
    selected = await window.__TAURI__.dialog.open({
      multiple: true,
      filters: [{ name: "DXF/SVG", extensions: ["dxf", "svg"] }],
    });
  } catch (err) {
    logLine(`file dialog failed: ${err}`, "error");
    return;
  }
  if (!selected) return; // user cancelled
  const paths = Array.isArray(selected) ? selected : [selected];
  await importPaths(paths);
}

// Appends rows for any shape not yet rendered, rather than clearing and
// rebuilding the whole table - importing happens incrementally now (a
// second file, a dropped file, a hand-added rectangle), and rebuilding
// every row from scratch would silently discard whatever ROLE/QTY the user
// already set on earlier rows.
function renderShapesTable() {
  const body = el("shapes-body");
  for (let i = body.children.length; i < importedShapes.length; i++) {
    const shape = importedShapes[i];
    const { w, h } = boundsOf(shape.points);
    const row = document.createElement("tr");
    row.dataset.row = shape._uiId;
    row.innerHTML = `
      <td><input type="checkbox" data-select="${shape._uiId}" /></td>
      <td>${i + 1}</td>
      <td>${escapeHtml(shape._file)}-${i + 1}</td>
      <td>${w.toFixed(1)} × ${h.toFixed(1)}</td>
      <td>${shapeThumbnailSvg(shape)}</td>
      <td>
        <select data-role="${shape._uiId}">
          <option value="part">${t("role_part")}</option>
          <option value="sheet">${t("role_sheet")}</option>
          <option value="skip">${t("role_skip")}</option>
        </select>
      </td>
      <td><input type="number" class="qty-input" data-qty="${shape._uiId}" value="1" min="0" step="1" /></td>
      <td>
        <select data-rot="${shape._uiId}">
          <option value="any">${t("rot_any")}</option>
          <option value="0,180">${t("rot_0_180")}</option>
          <option value="0,90,180,270">${t("rot_quarter")}</option>
          <option value="0">${t("rot_fixed")}</option>
        </select>
      </td>
      <td>
        <select data-mirror="${shape._uiId}">
          <option value="job">${t("mirror_job")}</option>
          <option value="allow">${t("mirror_allow")}</option>
          <option value="deny">${t("mirror_deny")}</option>
        </select>
      </td>
      <td><span class="dominant-flag" data-dominant="${shape._uiId}"></span></td>
    `;
    body.appendChild(row);
  }
  renumberShapesTable();
  updateDominantIndicators();
}

// The #/NAME columns show the shape's position in `importedShapes`, baked
// in as text at row-creation time - fine for pure append, but stale after
// `handleRemoveSelected` deletes from the middle: surviving rows keep their
// original numbers, so the next appended row's freshly-computed position
// can collide with an existing row's now-outdated one. Called after both
// appending and removing so the displayed numbers always match current
// array order, not creation-time order.
function renumberShapesTable() {
  const body = el("shapes-body");
  importedShapes.forEach((shape, i) => {
    const row = body.querySelector(`tr[data-row="${shape._uiId}"]`);
    if (!row) return;
    row.children[1].textContent = String(i + 1);
    row.children[2].textContent = `${shape._file}-${i + 1}`;
  });
}

// Shoelace formula - matches geometry::polygon::polygon_area exactly (only
// the magnitude matters here, not winding direction).
function polygonArea(points) {
  let area = 0;
  const n = points.length;
  for (let i = 0; i < n; i++) {
    const j = (i + n - 1) % n;
    area += (points[j].x + points[i].x) * (points[j].y - points[i].y);
  }
  return Math.abs(0.5 * area);
}

// Live preview of nesting::placement::place_parts's own "part_area >=
// dominant_part_area_threshold * sheet_area" check - a part that trips this
// isn't excluded from nesting, it still gets placed, it just closes
// whichever sheet it lands on immediately afterward instead of sharing it.
// Reference sheet is the LARGEST shape currently marked SHEET: a part that
// clears the bar against the biggest available sheet clears it against
// every smaller one too, so this only flags parts that are *always*
// dominant regardless of which sheet they end up on - a deliberate
// under-flag for mixed sheet sizes rather than a per-sheet-size breakdown.
function updateDominantIndicators() {
  const dominantInput = el("cfg-dominant");
  if (!dominantInput) return;
  const threshold = Number(dominantInput.value);

  let maxSheetArea = 0;
  importedShapes.forEach((shape) => {
    const roleEl = document.querySelector(`[data-role="${shape._uiId}"]`);
    if (roleEl?.value === "sheet") {
      maxSheetArea = Math.max(maxSheetArea, polygonArea(shape.points));
    }
  });

  importedShapes.forEach((shape) => {
    const cell = document.querySelector(`[data-dominant="${shape._uiId}"]`);
    if (!cell) return;
    const roleEl = document.querySelector(`[data-role="${shape._uiId}"]`);
    const isDominant = roleEl?.value === "part" && maxSheetArea > 0 && polygonArea(shape.points) >= threshold * maxSheetArea;
    cell.textContent = isDominant ? t("dominant_closes_sheet") : "";
  });
}

// Lets the user define a sheet or part directly by size, instead of
// needing a DXF file for it - e.g. a stock sheet size that isn't in any
// DXF on hand yet. Built the same shape as an imported PolygonDto so it
// flows through buildRequest/renderShapesTable unchanged.
function handleAddRectangle() {
  const width = Number(el("rect-width").value);
  const height = Number(el("rect-height").value);
  const layer = el("rect-layer").value.trim() || "CUSTOM";
  if (!(width > 0) || !(height > 0)) {
    setStatus("import-status", t("rect_invalid_size"), true);
    return;
  }

  importedShapes.push({
    layer,
    points: [
      { x: 0, y: 0 },
      { x: width, y: 0 },
      { x: width, y: height },
      { x: 0, y: height },
    ],
    is_circle: null,
    children: [],
    _uiId: nextShapeUiId++,
    _file: layer,
  });
  logLine(`added custom rectangle: ${width} x ${height} (layer "${layer}")`);
  renderShapesTable();
  el("panel-shapes").hidden = false;
  el("panel-config").hidden = false;
}

function shapeToPolygonDto(shape) {
  return {
    points: shape.points,
    layer: shape.layer,
    is_circle: shape.is_circle ?? null,
    children: shape.children ?? [],
    texts: shape.texts ?? [],
    real_boundary: shape.real_boundary ?? null,
  };
}

function buildRequest() {
  const sheets = [];
  const parts = [];

  importedShapes.forEach((shape) => {
    const role = document.querySelector(`[data-role="${shape._uiId}"]`).value;
    const qty = Number(document.querySelector(`[data-qty="${shape._uiId}"]`).value);
    if (role === "sheet" && qty > 0) {
      // Same "qty 0 means excluded" rule PART already uses below - a SHEET
      // row previously always contributed at least one sheet regardless of
      // its QTY field (Math.max(qty, 1)), an undocumented asymmetry that
      // made QTY look like it did nothing for a role where a user might
      // reasonably expect it to work the same as it does for PART.
      for (let n = 0; n < qty; n++) sheets.push(shapeToPolygonDto(shape));
    } else if (role === "part" && qty > 0) {
      // "any"/"job" send null, i.e. "no per-part rule" - the backend only
      // materialises a rule for parts that actually constrain something.
      const angles = document.querySelector(`[data-rot="${shape._uiId}"]`).value;
      const mirror = document.querySelector(`[data-mirror="${shape._uiId}"]`).value;
      parts.push({
        polygon: shapeToPolygonDto(shape),
        quantity: qty,
        allowed_rotations: angles === "any" ? null : angles.split(",").map(Number),
        mirror: mirror === "job" ? null : mirror === "allow",
      });
    }
  });

  const config = {
    placement_type: el("cfg-placement-type").value,
    rotations: Number(el("cfg-rotations").value),
    population_size: Number(el("cfg-population").value),
    mutation_rate: Number(el("cfg-mutation").value),
    dominant_part_area_threshold: Number(el("cfg-dominant").value),
    curve_tolerance: Number(el("import-tolerance").value),
    generations: Number(el("cfg-generations").value),
    margin: Number(el("cfg-margin").value),
    spacing: Number(el("cfg-spacing").value),
    max_threads: Number(el("cfg-max-threads").value),
    seed: Number(el("cfg-seed").value),
    runs: Number(el("cfg-runs").value),
    cleanup_threshold_percent: el("cfg-cleanup-threshold").value === "" ? null : Number(el("cfg-cleanup-threshold").value),
    mirror: el("cfg-mirror").checked,
  };

  return { sheets, parts, config };
}

async function handleRunNest() {
  const request = buildRequest();
  if (request.sheets.length === 0) {
    setStatus("run-status", t("run_need_sheet"), true);
    return;
  }
  if (request.parts.length === 0) {
    setStatus("run-status", t("run_need_part"), true);
    return;
  }
  // A blank/invalid numeric config field becomes NaN here, which
  // JSON.stringify serializes to `null` over the Tauri IPC boundary -
  // caught here with a field name the user can actually act on, instead of
  // surfacing as an opaque Rust-side deserialization failure.
  const invalidField = Object.entries(request.config).find(([, v]) => typeof v === "number" && Number.isNaN(v));
  if (invalidField) {
    setStatus("run-status", t("run_invalid_config_field", { field: invalidField[0] }), true);
    return;
  }

  const partInstances = request.parts.reduce((n, p) => n + p.quantity, 0);
  currentGenerations = request.config.generations;
  setStatus("run-status", t("run_status_running"), false);
  logLine(`nest: ${request.sheets.length} sheet(s), ${partInstances} part instance(s), ${request.config.runs} run(s)`);
  invoke("save_config_command", { config: request.config }).catch((err) => logLine(`could not save config: ${err}`));
  el("btn-run").disabled = true;
  el("btn-stop").hidden = false;
  el("btn-stop").disabled = false;
  setControlsLocked(true);
  collapseAllPanels();
  setBusy("run-spinner", true);
  el("run-progress").hidden = false;
  el("run-progress-fill").style.width = "0%";

  lastNestRequest = request;

  try {
    const response = await invoke("run_nest_command", { request });
    setStatus("run-status", response.cancelled ? t("run_status_stopped") : t("run_status_done"), false);
    // Console narration stays English regardless of UI language - see i18n.js's own doc comment.
    logLine(
      `nest ${response.cancelled ? "stopped early" : "done"}: fitness=${response.fitness.toFixed(1)} sheets=${response.placements.length} unplaced=${response.unplaced_count} util=${response.utilisation.toFixed(1)}%`
    );
    if (!response.cancelled) {
      el("run-progress-fill").style.width = "100%";
    }
    renderResult(response, request);
    el("panel-result").hidden = false;
  } catch (err) {
    setStatus("run-status", t("run_status_failed", { err }), true);
  } finally {
    el("btn-run").disabled = false;
    el("btn-stop").hidden = true;
    setControlsLocked(false);
    setBusy("run-spinner", false);
    el("run-progress").hidden = true;
  }
}

async function handleStopNest() {
  // Cancellation is checked per-part inside place_parts itself (not just
  // between individuals/generations), so this takes effect within roughly
  // one part's worth of computation - not instant at the OS level, but
  // close enough in practice that no "still finishing up" caveat is needed
  // here anymore.
  logLine("stop requested");
  el("btn-stop").disabled = true;
  try {
    await invoke("cancel_nest_command");
  } catch (err) {
    logLine(`stop request failed: ${err}`);
    el("btn-stop").disabled = false; // let the user try again instead of getting stuck
  }
}

// A rough, honest guess at *why* a part didn't place - the engine itself
// doesn't produce a structured reason, just "didn't fit in the best attempt
// found". The one thing we CAN check independently is whether the part's
// own bounding box could ever fit any available sheet's bounding box at
// all (in either orientation) - if not, it's not a "try more generations"
// problem, it's genuinely too big.
function unplacedReason(shape, request) {
  const { w, h } = boundsOf(shape.points);
  const fitsSomeSheet = request.sheets.some((sheetDto) => {
    const sb = boundsOf(sheetDto.points);
    return (w <= sb.w && h <= sb.h) || (w <= sb.h && h <= sb.w);
  });
  // `label` is short enough to sit visibly under the thumbnail at all times;
  // `detail` (the full explanation) still goes in `title` for anyone who
  // hovers - but the label alone is enough to tell "too big, don't bother
  // retrying" apart from "just needs another attempt" without hovering.
  return fitsSomeSheet
    ? { label: t("unplaced_label_no_room"), detail: t("unplaced_detail_no_room") }
    : { label: t("unplaced_label_too_large"), detail: t("unplaced_detail_too_large") };
}

// Keeps 03/CONFIGURE's own heading-row summary in sync with whatever the
// current best result actually is - unlike `run-status` (a transient
// "nesting.../done" message inside the collapsible drawer), this lives in
// the always-visible strip, so it stays legible even with the bar
// collapsed and the result panel scrolled out of view. Updated both from a
// live "nest-run-complete" event (see below) and from `renderSnapshot`
// (a picked VIEW ATTEMPT, or a recovered previous-session result).
function updateBottomBarSummary(sheetsUsed, unplacedCount, utilisation) {
  el("bottom-bar-summary").textContent = t("bottom_bar_summary", { sheets: sheetsUsed, unplaced: unplacedCount, util: utilisation.toFixed(1) });
}

// Renders one candidate nest (either the final response's own top-level
// fields, or one entry from its `history`) - both have the exact same
// shape (placements/fitness/utilisation/unplaced_count/unplaced_ids), so
// one renderer covers whichever the user picks in the VIEW ATTEMPT select.
function renderSnapshot(snapshot, request) {
  currentSnapshot = snapshot;
  updateBottomBarSummary(snapshot.placements.length, snapshot.unplaced_count, snapshot.utilisation);

  const stats = el("result-stats");
  stats.innerHTML = `
    <div><dt>${t("stat_fitness")}</dt><dd>${snapshot.fitness.toFixed(1)}</dd></div>
    <div><dt>${t("stat_utilisation")}</dt><dd>${snapshot.utilisation.toFixed(1)}%</dd></div>
    <div><dt>${t("stat_unplaced")}</dt><dd>${snapshot.unplaced_count}</dd></div>
    <div><dt>${t("stat_sheets_used")}</dt><dd>${snapshot.placements.length}</dd></div>
  `;

  const unplacedSection = el("unplaced-section");
  const unplacedList = el("unplaced-list");
  unplacedList.innerHTML = "";
  const unplacedIds = snapshot.unplaced_ids ?? [];
  unplacedSection.hidden = unplacedIds.length === 0;
  for (const id of unplacedIds) {
    const shape = lastPartsById[id];
    if (!shape) continue;
    const reason = unplacedReason(shape, request);
    const item = document.createElement("div");
    item.className = "unplaced-item";
    item.title = reason.detail;
    item.innerHTML = `${shapeThumbnailSvg(shape, UNPLACED_COLOR)}<span>#${id} ${escapeHtml(shape.layer)}</span><span class="unplaced-reason">${reason.label}</span>`;
    unplacedList.appendChild(item);
  }

  const sheetsEl = el("sheets");
  sheetsEl.innerHTML = "";

  for (const placement of snapshot.placements) {
    const sheetDto = request.sheets[placement.sheet_index];
    const sheetBounds = boundsOf(sheetDto.points);
    const { w, h } = sheetBounds;
    const scale = Math.min(700 / Math.max(w, 1), 500 / Math.max(h, 1));

    const wrapper = document.createElement("div");
    // A quick visual scan cue across many sheets - which ones are packed
    // tight vs. which are mostly empty - without having to eyeball each
    // SVG's whitespace individually. Raw polygon area (not "usable" area
    // net of margin/spacing, which only the Rust side tracks) is close
    // enough for a color band; the exact number is right there in the
    // caption for anyone who wants it precisely.
    const sheetArea = polygonArea(sheetDto.points);
    const usedArea = placement.parts.reduce((sum, p) => {
      const shape = lastPartsById[p.id];
      return shape ? sum + polygonArea(shape.points) : sum;
    }, 0);
    const sheetUtilisation = sheetArea > 0 ? (usedArea / sheetArea) * 100 : 0;
    const utilClass = sheetUtilisation >= 75 ? "sheet-util-high" : sheetUtilisation >= 45 ? "sheet-util-mid" : "sheet-util-low";
    wrapper.className = `sheet ${utilClass}`;

    const svgParts = placement.parts
      .map((p) => {
        const shape = lastPartsById[p.id];
        if (!shape) return "";
        const transform = (points) => toSvgPoints(rotatedTranslatedPoints(points, p.rotation, p.x, p.y), sheetBounds);
        return renderShapeSvg(shape, transform);
      })
      .join("");

    wrapper.innerHTML = `
      <svg viewBox="0 0 ${w} ${h}" width="${(w * scale).toFixed(0)}" height="${(h * scale).toFixed(0)}">
        <polygon points="${pointsToPath(toSvgPoints(sheetDto.points, sheetBounds))}" fill="none" stroke="#8a8a8a" stroke-width="${1 / scale}" />
        ${svgParts}
      </svg>
      <div class="caption">${t("sheet_caption", { n: placement.sheet_index + 1, parts: placement.parts.length, util: sheetUtilisation.toFixed(1) })}
        <button type="button" class="small btn-repack" title="${t("repack_tooltip")}">${t("repack_button")}</button>
      </div>
    `;
    wrapper.querySelector(".btn-repack").addEventListener("click", () => handleRepackSheet(placement.sheet_index));
    sheetsEl.appendChild(wrapper);
  }
}

// Manual, click-a-sheet counterpart to the CLEANUP THRESHOLD config option -
// both call the same nesting::repack::repack_sheet on the backend. Always
// available regardless of the sheet's own utilisation (unlike the automatic
// pass, which only touches sheets under the configured threshold).
async function handleRepackSheet(sheetIndex) {
  if (!currentSnapshot || !lastNestRequest || !lastPartsById) return;
  if (!lastNestRequest.config) {
    setStatus("run-status", t("repack_needs_config"), true);
    return;
  }
  const idx = currentSnapshot.placements.findIndex((p) => p.sheet_index === sheetIndex);
  if (idx === -1) return;
  const placement = currentSnapshot.placements[idx];
  const partsById = {};
  const partRules = {};
  for (const p of placement.parts) {
    partsById[p.id] = lastPartsById[p.id];
    if (lastPartRules[p.id]) partRules[p.id] = lastPartRules[p.id];
  }

  const displayIndex = sheetIndex + 1; // matches the SHEET N label on the card itself, not the internal 0-based index
  setStatus("run-status", t("repack_status_running", { n: displayIndex }), false);
  try {
    const response = await invoke("repack_sheet_command", {
      request: { sheet: lastNestRequest.sheets[sheetIndex], placement, parts_by_id: partsById, config: lastNestRequest.config, part_rules: partRules },
    });
    currentSnapshot.placements[idx] = response.placement;
    renderSnapshot(currentSnapshot, lastNestRequest);
    setStatus(
      "run-status",
      response.improved ? t("repack_status_improved", { n: displayIndex, util: response.utilisation.toFixed(1) }) : t("repack_status_no_improvement", { n: displayIndex }),
      false
    );
    logLine(`repack sheet ${displayIndex}: ${response.improved ? "improved" : "no improvement found"}`);
  } catch (err) {
    setStatus("run-status", t("repack_status_failed", { n: displayIndex, err }), true);
  }
}

// Populates the VIEW ATTEMPT select from response.history (every nest that
// beat the previous best, not just the final winner) and renders whichever
// one is selected - defaulting to the last (the same one the top-level
// response fields describe).
function renderResult(response, request) {
  lastNestRequest = request;
  lastPartsById = response.parts_by_id;
  lastPartRules = response.part_rules || {};

  const historyRow = el("history-row");
  const select = el("history-select");
  const history = response.history?.length ? response.history : [response];
  historyRow.hidden = history.length <= 1;

  select.innerHTML = history
    .map((h, i) => {
      const isLast = i === history.length - 1;
      const label = t("history_option", { i: i + 1, gen: h.generation ?? "-", best: isLast ? t("history_best_suffix") : "", fitness: h.fitness.toFixed(0), unplaced: h.unplaced_count });
      return `<option value="${i}">${label}</option>`;
    })
    .join("");
  select.value = String(history.length - 1);
  select.onchange = () => renderSnapshot(history[Number(select.value)], request);

  renderSnapshot(history[history.length - 1], request);
}

// DXF stays the primary/first-class export format too (matches import);
// SVG is the additive second option, both going through the exact same
// ExportRequest shape (geometry::svg_export mirrors geometry::dxf_export's
// input) - this map is the only place format choice turns into "which
// Tauri command / which file extension".
const EXPORT_FORMATS = {
  dxf: { command: "export_dxf_command", extension: "dxf", filterName: "DXF" },
  svg: { command: "export_svg_command", extension: "svg", filterName: "SVG" },
};

async function handleExport() {
  if (!currentSnapshot || !lastNestRequest || !lastPartsById) return;

  const sheetSpacing = Number(el("export-spacing").value);
  if (!(sheetSpacing >= 0)) {
    setStatus("export-status", t("export_invalid_spacing"), true);
    return;
  }
  const includeSheetOutline = el("export-outline").checked;
  const includeUnplaced = el("export-unplaced").checked;
  const format = EXPORT_FORMATS[el("export-format").value] ?? EXPORT_FORMATS.dxf;

  let path;
  try {
    path = await window.__TAURI__.dialog.save({
      defaultPath: `nest.${format.extension}`,
      filters: [{ name: format.filterName, extensions: [format.extension] }],
    });
  } catch (err) {
    setStatus("export-status", t("export_dialog_failed", { err }), true);
    return;
  }
  if (!path) return; // user cancelled

  const request = {
    sheets: lastNestRequest.sheets,
    // The authoritative id -> shape mapping run_nest_command itself built
    // (RunNestResponse::parts_by_id), not a re-sent parts/quantity list for
    // export_dxf/export_svg to re-derive its own mapping from - see that DTO
    // field's doc comment for the silent-corruption risk this avoids. It's
    // also what makes include_unplaced work: parts_by_id already covers
    // every part copy from the run, placed or not.
    parts_by_id: lastPartsById,
    placements: currentSnapshot.placements,
    sheet_spacing: sheetSpacing,
    include_sheet_outline: includeSheetOutline,
    include_unplaced: includeUnplaced,
  };

  setStatus("export-status", t("export_status_running"), false);
  logLine(`export: ${path} (sheet spacing ${sheetSpacing}mm, sheet outline ${includeSheetOutline ? "on" : "off"}, unplaced parts ${includeUnplaced ? "on" : "off"})`);
  el("btn-export").disabled = true;
  try {
    await invoke(format.command, { path, request });
    setStatus("export-status", t("export_status_done"), false);
    logLine(`export ok: ${path}`);
  } catch (err) {
    setStatus("export-status", String(err), true);
    logLine(`export failed: ${err}`);
  } finally {
    el("btn-export").disabled = false;
  }
}

// Applies every saved display preference (language, accent color, text
// scale) up front, then keeps the header controls and already-rendered
// dynamic content (dominant flags, the current result view if one's
// showing, the settings-bar label) in sync on every change.
// ponytail: the shapes table's ROLE dropdown options (PART/SHEET/SKIP) are
// only translated at row-creation time, not retroactively - switching
// language after importing leaves already-listed rows' dropdown wording in
// the old language (the underlying part/sheet/skip value is unaffected,
// it's cosmetic only). Re-importing or restarting picks up the new
// language. Fixing this needs the table to always rebuild from stored
// per-row state rather than only append, which isn't done here.
applySavedPrefs();
el("lang-switch").value = getLang();
applyStaticTranslations();
el("settings-bar-label").innerHTML = renderSettingsBarLabel(true);

// Reads the real app version from tauri.conf.json (via Tauri's own core
// app module, already covered by capabilities/default.json's core:default
// grant - no new permission needed) instead of hardcoding a copy here that
// could drift out of sync with the actual build.
window.__TAURI__.app
  .getVersion()
  .then((version) => {
    el("app-version").textContent = `v${version}`;
  })
  .catch((err) => logLine(`could not read app version: ${err}`));

el("lang-switch").addEventListener("change", (event) => {
  setLang(event.target.value);
  el("help-lang-switch").value = event.target.value;
  updateDominantIndicators();
  if (currentSnapshot && lastNestRequest) renderSnapshot(currentSnapshot, lastNestRequest);
});

// First-run help modal - shown once automatically (unless previously
// dismissed with its own checkbox ticked), reopenable anytime via the
// header's "?" button. Its own language switch is a second <select> wired
// to the exact same setLang(), not a separate mechanism - picking a
// language here changes the whole app, not just the modal, and the gear
// menu's switch stays in sync with whichever one was used last.
const HELP_DISMISSED_KEY = "rustynesting-help-dismissed";

function openHelp() {
  el("help-lang-switch").value = getLang();
  el("help-overlay").hidden = false;
}

function closeHelp() {
  el("help-overlay").hidden = true;
  if (el("help-dont-show").checked) {
    localStorage.setItem(HELP_DISMISSED_KEY, "1");
  }
}

el("btn-help").addEventListener("click", openHelp);
el("btn-help-close").addEventListener("click", closeHelp);
el("help-overlay").addEventListener("click", (event) => {
  if (event.target === el("help-overlay")) closeHelp();
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && !el("help-overlay").hidden) closeHelp();
});
el("help-lang-switch").addEventListener("change", (event) => {
  setLang(event.target.value);
  el("lang-switch").value = event.target.value;
  updateDominantIndicators();
  if (currentSnapshot && lastNestRequest) renderSnapshot(currentSnapshot, lastNestRequest);
});

if (!localStorage.getItem(HELP_DISMISSED_KEY)) {
  openHelp();
}

el("scale-switch").value = getScaleName();
el("scale-switch").addEventListener("change", (event) => setScale(event.target.value));

// Gear-menu toggle - a dropdown anchored under the button (`.app-settings`'s
// own `position: relative` in app.css), not a modal, so it reads as a
// lightweight preference panel rather than interrupting the page. Closes on
// any click outside itself; the gear button's own click is excluded from
// that check (stopPropagation) so pressing it doesn't immediately re-close
// what it just opened.
const appSettingsMenu = el("app-settings-menu");
el("btn-app-settings").addEventListener("click", (event) => {
  event.stopPropagation();
  appSettingsMenu.hidden = !appSettingsMenu.hidden;
});
document.addEventListener("click", (event) => {
  if (!appSettingsMenu.hidden && !appSettingsMenu.contains(event.target)) {
    appSettingsMenu.hidden = true;
  }
});

// Swatches and the hex field both drive the same accent - each one syncs
// the other's displayed state so neither goes stale after the other is used.
function markSelectedSwatch(color) {
  document.querySelectorAll("#accent-swatches .swatch").forEach((s) => s.classList.toggle("selected", s.dataset.accent.toLowerCase() === color.toLowerCase()));
}

const accentHexInput = el("accent-hex");
const currentAccent = getAccent();
accentHexInput.value = currentAccent;
markSelectedSwatch(currentAccent);

document.querySelectorAll("#accent-swatches .swatch").forEach((swatch) => {
  swatch.addEventListener("click", () => {
    setAccent(swatch.dataset.accent);
    accentHexInput.value = swatch.dataset.accent;
    markSelectedSwatch(swatch.dataset.accent);
  });
});

// Live as you type, but only once the code is a complete, valid hex value -
// an in-progress "#ffc" simply doesn't apply yet rather than erroring.
accentHexInput.addEventListener("input", () => {
  const value = accentHexInput.value.trim();
  if (!/^#([0-9a-fA-F]{3}|[0-9a-fA-F]{6})$/.test(value)) return;
  setAccent(value);
  markSelectedSwatch(value);
});

el("btn-import").addEventListener("click", handleBrowse);
el("btn-add-rect").addEventListener("click", handleAddRectangle);
el("btn-run").addEventListener("click", handleRunNest);
el("btn-stop").addEventListener("click", handleStopNest);
el("btn-toggle-shapes").addEventListener("click", handleToggleShapes);
el("btn-mark-all-parts").addEventListener("click", () => markAllRoles("part"));
el("btn-mark-all-sheets").addEventListener("click", () => markAllRoles("sheet"));
el("btn-toggle-advanced").addEventListener("click", handleToggleAdvanced);
el("btn-toggle-settings").addEventListener("click", handleToggleSettings);
el("btn-export").addEventListener("click", handleExport);

// Live percentage readout + dominant-part re-check on every slider move.
el("cfg-dominant").addEventListener("input", () => {
  el("cfg-dominant-value").textContent = `${Math.round(Number(el("cfg-dominant").value) * 100)}%`;
  updateDominantIndicators();
});


// Delegated (not per-row) since rows are added dynamically after this
// listener is wired - a shape's ROLE changes which shapes count as "sheet"
// for the dominant-area comparison, so it needs the same re-check.
el("shapes-body").addEventListener("change", (event) => {
  if (event.target.matches("[data-role]")) {
    updateDominantIndicators();
  }
});

// Permanently deletes every ticked shape from the import list in one go -
// distinct from the existing ROLE=SKIP option, which only excludes a shape
// from the next run without ever removing it (reversible, no confirmation
// needed). Deletion isn't reversible, so it's gated behind a single native
// confirm dialog covering the whole batch.
async function handleRemoveSelected() {
  const ids = Array.from(document.querySelectorAll("#shapes-body [data-select]:checked")).map((cb) => Number(cb.dataset.select));
  if (ids.length === 0) return;

  const confirmed = await window.__TAURI__.dialog.confirm(t("confirm_remove_message", { n: ids.length }), {
    title: t("confirm_remove_title"),
    kind: "warning",
  });
  if (!confirmed) return;

  importedShapes = importedShapes.filter((s) => !ids.includes(s._uiId));
  ids.forEach((uiId) => el("shapes-body").querySelector(`tr[data-row="${uiId}"]`)?.remove());
  el("select-all-shapes").checked = false;
  renumberShapesTable();
  updateDominantIndicators();
  logLine(`removed ${ids.length} shape(s) (${importedShapes.length} remaining)`);
}

el("btn-remove-selected").addEventListener("click", handleRemoveSelected);

// Clears imported parts/sheets and the last nest result, but leaves every
// #panel-config input untouched - those fields aren't removed from the DOM,
// just hidden along with #panel-shapes/#panel-config (both re-shown by the
// next import), so their values survive exactly like loadSavedConfig()
// intends. Also clears the on-disk best-result save (clear_best_result_command,
// the same one tryRecoverBestResult already calls when the user declines to
// recover) so a reset actually sticks across a restart instead of the old
// nest reappearing via the recovery prompt.
async function handleReset() {
  const confirmed = await window.__TAURI__.dialog.confirm(t("confirm_reset_message"), {
    title: t("confirm_reset_title"),
    kind: "warning",
  });
  if (!confirmed) return;

  importedShapes = [];
  el("shapes-body").innerHTML = "";
  el("select-all-shapes").checked = false;
  el("panel-shapes").hidden = true;
  el("panel-config").hidden = true;

  currentSnapshot = null;
  lastNestRequest = null;
  lastPartsById = null;
  lastPartRules = {};
  el("panel-result").hidden = true;
  el("history-row").hidden = true;
  el("history-select").innerHTML = "";
  el("result-stats").innerHTML = "";
  el("unplaced-section").hidden = true;
  el("unplaced-list").innerHTML = "";
  el("sheets").innerHTML = "";
  el("bottom-bar-summary").textContent = "";
  setStatus("import-status", "", false);
  setStatus("run-status", "", false);

  try {
    await invoke("clear_best_result_command");
  } catch (err) {
    logLine(`could not clear saved best result: ${err}`);
  }

  logLine("reset: cleared imported parts/sheets and last nest result (config kept)");
}

el("btn-reset").addEventListener("click", handleReset);

el("select-all-shapes").addEventListener("change", (event) => {
  document.querySelectorAll("#shapes-body [data-select]").forEach((cb) => {
    cb.checked = event.target.checked;
  });
});

// Restores the config panel to whatever was last saved
// (save_config_command, called right before every run) so a new session
// starts from the previous one's settings instead of index.html's
// hardcoded defaults - lets a follow-up session actually see what the last
// test run used.
async function loadSavedConfig() {
  let saved;
  try {
    saved = await invoke("load_config_command");
  } catch (err) {
    logLine(`could not load saved config: ${err}`);
    return;
  }
  if (!saved) return;
  el("cfg-placement-type").value = saved.placement_type;
  el("cfg-rotations").value = saved.rotations;
  el("cfg-population").value = saved.population_size;
  el("cfg-mutation").value = saved.mutation_rate;
  el("cfg-dominant").value = saved.dominant_part_area_threshold;
  el("cfg-dominant-value").textContent = `${Math.round(saved.dominant_part_area_threshold * 100)}%`;
  el("cfg-generations").value = saved.generations;
  el("cfg-margin").value = saved.margin;
  el("cfg-spacing").value = saved.spacing;
  el("cfg-max-threads").value = saved.max_threads;
  el("cfg-seed").value = saved.seed ?? 0;
  el("cfg-runs").value = saved.runs ?? 6;
  el("cfg-cleanup-threshold").value = saved.cleanup_threshold_percent ?? "";
  // cfg-mirror is deliberately NOT restored: letting pieces be cut mirrored
  // is only safe for material with no good side, and a setting that quietly
  // survives into a session where the material *does* have one produces
  // scrap. It starts off every launch and has to be ticked on purpose.
  el("import-tolerance").value = saved.curve_tolerance;
  updateDominantIndicators();
  logLine("restored config from last session");
}
loadSavedConfig();

// Offers to restore the best nest result ever saved to disk
// (run_nest_command persists it after every run that beats the previous
// best - see that command's own doc comment) so a fresh session doesn't
// start blank after closing the app mid-work. Renders through the same
// renderSnapshot() the live run flow uses - a recovered result has no
// `history` (only the winning snapshot was ever persisted), so it's shown
// as a single-entry view rather than through renderResult()'s history select.
async function tryRecoverBestResult() {
  let best;
  try {
    best = await invoke("load_best_result_command");
  } catch (err) {
    logLine(`could not load saved best result: ${err}`);
    return;
  }
  if (!best) return;

  const recover = await window.__TAURI__.dialog.ask(t("recover_message", { sheets: best.placements.length, util: best.utilisation.toFixed(1) }), {
    title: t("recover_title"),
    kind: "info",
  });

  if (!recover) {
    try {
      await invoke("clear_best_result_command");
    } catch (err) {
      logLine(`could not clear saved best result: ${err}`);
    }
    return;
  }

  lastPartsById = best.parts_by_id;
  lastPartRules = best.part_rules || {};
  // `config` too, not just `sheets`: REPACK (and anything else that
  // re-invokes the engine on a result) reads `lastNestRequest.config`, so a
  // recovered result used to throw the moment you touched it. Older
  // best_result.json files predate the field and legitimately have none -
  // the buttons that need it are disabled in that case rather than failing
  // at click time.
  const request = { sheets: best.sheets, config: best.config || null };
  lastNestRequest = request;
  el("history-row").hidden = true;
  renderSnapshot(best, request);
  el("panel-result").hidden = false;
  logLine("recovered best result from a previous session");
}
tryRecoverBestResult();

// Narrates the auto-escalating "Runs" loop (see commands.rs's
// run_nest_with_progress) - one line per attempt starting, then one when it
// finishes, so the console reads as a story ("trying more rotations now...
// that was better, keep going") instead of just a wall of per-generation
// numbers with no sense of which attempt produced them.
window.__TAURI__.event.listen("nest-run-start", (event) => {
  const r = event.payload;
  logLine(`run ${r.run}/${r.total_runs}: trying ${r.rotations} rotation(s), population ${r.population_size}, ${r.generations} generation(s)...`, "run");
  // Each run has its own generation count (escalates with the run - see
  // commands.rs's escalated_run_config), so the "nest-tick" progress-bar
  // math below needs updating per run, not just once at request time.
  currentGenerations = r.generations;
});

window.__TAURI__.event.listen("nest-run-complete", (event) => {
  const r = event.payload;
  const verdict = r.improved ? "NEW BEST" : "no improvement";
  logLine(
    `run ${r.run}/${r.total_runs} done: sheets=${r.sheets_used} unplaced=${r.unplaced_count} util=${r.utilisation.toFixed(1)}% -> ${verdict}`,
    r.improved ? "best" : "run"
  );
  // Live update, not just at the very end of the whole escalation - the
  // full response (and its own renderSnapshot() call) only ever lands once
  // every configured run has finished, which for a several-run job can be
  // a long wait with no visible sign of the best-so-far otherwise.
  if (r.improved) {
    updateBottomBarSummary(r.sheets_used, r.unplaced_count, r.utilisation);
  }
});

// Live per-generation stats while a nest run is in progress, emitted by
// run_nest_command (see src-tauri/src/commands.rs's run_nest_with_progress)
// - the Rust-side counterpart to this UI's console panel. `generations`
// here is the *current run's* own total (resets each run - see
// "nest-run-start" above for which attempt this belongs to), not the whole
// escalation's.
window.__TAURI__.event.listen("nest-progress", (event) => {
  const p = event.payload;
  logLine(`  gen ${p.generation}/${p.generations}: fitness=${p.best_fitness.toFixed(1)} sheets=${p.sheets_used} unplaced=${p.unplaced_count} util=${p.utilisation.toFixed(1)}%`);
  el("run-progress-fill").style.width = `${((100 * p.generation) / p.generations).toFixed(1)}%`;
});

// Fires far more often than "nest-progress" above - once up front and once
// per individual placed, inside a single generation - so the console and
// progress bar keep moving during a slow generation instead of sitting
// still (a single individual's placement against real, non-trivial
// geometry can take tens of seconds, and without this there was no signal
// at all between "generation started" and "generation finished").
window.__TAURI__.event.listen("nest-tick", (event) => {
  const t = event.payload;
  logLine(t.individuals_done === 0 ? `gen ${t.generation}: starting (${t.individuals_total} to place)...` : `gen ${t.generation}: ${t.individuals_done}/${t.individuals_total} individuals placed`);
  if (currentGenerations > 0) {
    const fraction = t.individuals_total > 0 ? t.individuals_done / t.individuals_total : 0;
    const overall = ((t.generation - 1 + fraction) / currentGenerations) * 100;
    el("run-progress-fill").style.width = `${overall.toFixed(1)}%`;
  }
});

// Drag-and-drop DXF/SVG import - Tauri delivers dropped-file paths as a core
// window event (needs `dragDropEnabled: true` in tauri.conf.json's window
// config; not a plugin, so no extra capability beyond core:event:default).
const dropzone = el("dropzone");
window.__TAURI__.event.listen("tauri://drag-enter", () => dropzone.classList.add("drag-over"));
window.__TAURI__.event.listen("tauri://drag-leave", () => dropzone.classList.remove("drag-over"));
window.__TAURI__.event.listen("tauri://drag-drop", (event) => {
  dropzone.classList.remove("drag-over");
  const paths = event.payload.paths.filter((p) => importCommandFor(p) !== null);
  if (paths.length < event.payload.paths.length) {
    logLine(`ignored ${event.payload.paths.length - paths.length} dropped file(s) that weren't .dxf/.svg`);
  }
  importPaths(paths);
});

// Console as a small floating window instead of a fixed panel in the main
// column - draggable by its header (pointer capture so a fast drag can't
// outrun the element and drop the drag mid-move) and collapsible down to
// just the header. Deliberately not closable - it's the only place errors
// are surfaced now (see setStatus's run-status special case above), so
// there's no "reopen" affordance to lose track of either.
(function setupConsoleWindow() {
  const win = el("console-window");
  const header = el("console-header");
  const minimizeBtn = el("btn-console-minimize");

  let dragging = false;
  let offsetX = 0;
  let offsetY = 0;

  header.addEventListener("pointerdown", (event) => {
    if (event.target.closest(".icon-btn")) return;
    dragging = true;
    const rect = win.getBoundingClientRect();
    offsetX = event.clientX - rect.left;
    offsetY = event.clientY - rect.top;
    // Switch from the CSS default (anchored via `top`/`right`) to an
    // explicit `left` once dragging starts - keeping `right` set would fight
    // the `left` this drag needs to set on every move.
    win.style.left = `${rect.left}px`;
    win.style.top = `${rect.top}px`;
    win.style.right = "auto";
    header.setPointerCapture(event.pointerId);
  });

  header.addEventListener("pointermove", (event) => {
    if (!dragging) return;
    const maxLeft = window.innerWidth - win.offsetWidth;
    const maxTop = window.innerHeight - header.offsetHeight;
    win.style.left = `${Math.min(Math.max(0, event.clientX - offsetX), Math.max(0, maxLeft))}px`;
    win.style.top = `${Math.min(Math.max(0, event.clientY - offsetY), Math.max(0, maxTop))}px`;
  });

  header.addEventListener("pointerup", (event) => {
    dragging = false;
    header.releasePointerCapture(event.pointerId);
  });

  minimizeBtn.addEventListener("click", () => {
    const collapsed = win.classList.toggle("collapsed");
    minimizeBtn.textContent = collapsed ? "▢" : "_";
  });
})();
