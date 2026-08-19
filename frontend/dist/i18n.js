// Minimal dictionary-based i18n: one flat key->string map per language, a
// {placeholder} substitution helper, and a DOM-attribute convention
// (data-i18n / data-i18n-title / data-i18n-placeholder) so index.html's
// static markup doesn't need per-element JS wiring. Adding a language later
// is just another entry in `translations` below - no new mechanism needed.
//
// ponytail: only the primary UI (labels, buttons, hints, tooltips, status
// messages, dialogs) is translated - the floating CONSOLE window's own
// narration (per-file import lines, per-generation/tick progress, run
// start/complete events) stays English-only for now. That's debug
// telemetry, not something a non-English-reading operator needs to act on,
// and it's a lot of interpolation-heavy strings for comparatively little
// value. Extend the same way (add keys here, wrap the string in `t()`) if
// that ever needs to change.

export const translations = {
  en: {
    heading_import_text: "IMPORT DXF / SVG",
    tolerance_label: "TOLERANCE (mm)",
    tolerance_tooltip:
      "How closely curves (arcs/circles) are approximated with straight line segments, in millimeters. Lower = more accurate but more points; higher = fewer points but a coarser shape.",
    btn_browse: "BROWSE...",
    dropzone_text: "or drag one or more .dxf/.svg files here",
    rect_hint: "or add a rectangle directly (a stock sheet size, or a simple rectangular part) without a DXF/SVG file:",
    rect_width_label: "WIDTH (mm)",
    rect_height_label: "HEIGHT (mm)",
    rect_layer_label: "LAYER NAME",
    btn_add_rect: "ADD RECTANGLE",

    svg_unit_title: "SVG UNITS",
    svg_unit_intro: "What does one coordinate unit in this SVG file represent in real life?",
    svg_unit_label: "FILE UNIT",
    svg_unit_auto: "Auto-detect from file (recommended)",
    svg_unit_mm: "Millimeters (mm)",
    svg_unit_cm: "Centimeters (cm)",
    svg_unit_m: "Meters (m)",
    svg_unit_px: "Pixels (px, 96/inch)",
    svg_unit_cancel: "CANCEL",
    svg_unit_ok: "IMPORT",

    heading_roles_text: "ASSIGN ROLES",
    btn_mark_all_parts: "ALL PART",
    btn_mark_all_parts_tooltip: "Set every imported shape's ROLE to PART in one click, instead of picking each row's dropdown individually.",
    btn_mark_all_sheets: "ALL SHEET",
    btn_mark_all_sheets_tooltip: "Set every imported shape's ROLE to SHEET in one click, instead of picking each row's dropdown individually.",
    btn_remove_selected: "REMOVE SELECTED",
    btn_remove_selected_tooltip: "Permanently remove every ticked shape below from the import list. Cannot be undone.",
    toggle_collapse_tooltip: "Collapse/expand",
    roles_hint: "Mark each imported shape as a SHEET (stock to nest onto) or a PART (thing to nest), and set a quantity.",
    select_all_tooltip: "Select all",
    th_index: "#",
    th_name: "NAME",
    th_bbox: "BBOX (W×H)",
    th_preview: "PREVIEW",
    th_role: "ROLE",
    th_qty: "QTY",
    th_grain: "ANGLES",
    th_grain_tooltip:
      "Restrict which angles this piece may be placed at. Use it when the material has a direction: a visible grain, a brushed finish, or a printed pattern that has to run the same way on every piece. ANY lets the nest use every angle it is searching (best packing).",
    th_part_mirror: "FLIP",
    th_part_mirror_tooltip:
      "Whether this specific piece may be cut flipped over, overriding the job-wide FLIP PIECES OVER setting. Use DENY for a piece that must keep one face up (a good side, a printed face, an asymmetric feature) in a job where flipping is otherwise fine.",
    rot_any: "ANY",
    rot_0_180: "0 / 180 only",
    rot_quarter: "90° steps",
    rot_fixed: "0° only",
    mirror_job: "USE JOB",
    mirror_allow: "ALLOW",
    mirror_deny: "DENY",
    th_dominant: "DOMINANT?",
    th_dominant_tooltip:
      "Whether this part alone would take up enough of a sheet (per the DOMINANT AREA setting below) to close that sheet immediately after being placed - it still gets placed, just without any neighbors.",

    heading_result_text: "RESULT",
    view_attempt_label: "VIEW ATTEMPT",
    view_attempt_tooltip:
      "Every attempt that beat the previous best while the GA ran, in the order it found them - not just the one it ended up keeping. Pick one to see that arrangement instead of the final winner.",
    unplaced_hint: "These parts could not be placed on any sheet (outlined in red below and in the list they came from):",
    export_hint: "Export the attempt currently shown above to a file:",
    export_format_label: "FORMAT",
    export_spacing_label: "SHEET SPACING (mm)",
    export_spacing_tooltip:
      "Neither DXF nor SVG has a separate concept of 'sheets' - every sheet used gets written into the same drawing, laid out left to right. This is the gap kept between each one so they don't overlap.",
    export_outline_label: "INCLUDE SHEET OUTLINE",
    export_outline_tooltip: "Also write each sheet's own rectangle as a shape in the export, not just the parts placed on it.",
    export_unplaced_label: "INCLUDE UNPLACED PARTS",
    export_unplaced_tooltip:
      "Also write every part that never got placed on any sheet, laid out in a simple non-overlapping grid after the last sheet (not nested - just kept from overlapping so you can see and handle them).",
    btn_export: "EXPORT",

    margin_label: "MARGIN (mm)",
    margin_tooltip:
      "Minimum empty gap kept between a piece and the true edge of the sheet - e.g. so a cutting tool has room to run without going off the material. Set to 0 for jobs where the tool can safely overhang the edge (like most laser cutting).",
    spacing_label: "SPACING (mm)",
    spacing_tooltip:
      "Minimum gap kept between two pieces placed next to each other, so a cutting tool fits between them and they don't touch. Set to 0 if pieces can be placed edge-to-edge with no gap at all.",
    runs_label: "RUNS",
    runs_tooltip:
      "How many increasingly thorough attempts to try automatically. The first attempt is cheap and quick; each one after tries one more rotation angle plus a bigger search budget than the last, and whichever attempt actually nests best is what you get - you don't need to know anything about rotations/population/generations for this to work well. More runs = better results, but takes longer. Advanced Settings below let you change where the escalation starts, if you want to.",
    cleanup_label: "CLEANUP THRESHOLD (%, blank = off)",
    cleanup_tooltip:
      "After a run finishes, any sheet whose own utilisation ends up below this gets re-packed in place - same settings, that sheet's parts only, never pulling parts from other sheets. Leave blank to turn this off. You can also REPACK a single sheet manually from the RESULT view regardless of this setting.",
    cleanup_placeholder: "e.g. 60",
    cleanup_hint:
      "Sheets under this utilisation get auto-repacked in place after a run (same parts, tighter arrangement) - blank turns it off. You can also REPACK any single sheet manually from the RESULT view regardless.",
    mirror_label: "FLIP PIECES OVER (MIRROR)",
    mirror_tooltip:
      "Lets the nest place a piece flipped over (mirrored), not just rotated - which often saves real material on pieces that are not symmetric. Only tick this if the material has no side: no grain, no coating, no printed or good face, and no feature that has to stay on one face. If the material does have a side, a flipped piece is scrap.",
    mirror_on_badge: "ON — PIECES MAY BE CUT MIRRORED",
    mirror_run_warning: "MIRRORING ON",
    mirror_hint:
      "Only for material with no good side (no grain, no coating, no printed face). RUN 1 is always done without flipping, so you always get a flip-free result to compare against - whichever run packs best is the one you keep. Always starts OFF at app launch, never restored from the last session.",
    btn_advanced_collapsed: "ADVANCED SETTINGS ▾",
    btn_advanced_expanded: "ADVANCED SETTINGS ▴",
    settings_bar_text: "CONFIGURE — SETTINGS",

    placement_label: "PLACEMENT",
    placement_tooltip:
      "How the program judges where to slide a piece into place. TIGHT FIT (recommended): judges purely by how snugly a piece touches already-placed pieces or the sheet edge - the one that benchmarked best against a real interlocking-tile stress test, and never worse than the alternatives on a typical mixed-shape job either. The remaining options are kept for comparison/fallback. GRAVITY + CORRECTIVE: the sheet's first two pieces settle by GRAVITY, then every piece after switches to TIGHT FIT's contact measure. GRAVITY: let pieces settle toward a corner, like Tetris. BOX: try to keep everything placed so far inside as small a rectangle as possible. CONVEX HULL: judge by the simplified outer shape wrapped around everything placed so far. GRAVITY + TIGHT FIT: use GRAVITY first, then break ties between equally-good spots by which one touches more.",
    placement_opt_tightfit: "TIGHT FIT (RECOMMENDED)",
    placement_opt_gravitycorrective: "GRAVITY + CORRECTIVE",
    placement_opt_gravitytightfit: "GRAVITY + TIGHT FIT",
    placement_opt_gravity: "GRAVITY",
    placement_opt_box: "BOX",
    placement_opt_convexhull: "CONVEX HULL",
    placement_hint:
      "TIGHT FIT (recommended) scores by how snugly a piece touches its neighbors - best for irregular/interlocking shapes. GRAVITY settles pieces like Tetris; BOX/CONVEX HULL score by the overall shape placed so far and plateau earlier on irregular parts. GRAVITY + CORRECTIVE/TIGHT FIT are hybrids. Hover PLACEMENT for the full comparison.",

    rotations_label: "STARTING ROTATIONS",
    rotations_tooltip:
      "The starting number of angles the first RUN tries for each piece - each RUN after the first tries one more angle than the last (see RUNS above), so this only matters as a starting point, not a fixed limit.",
    population_label: "STARTING POPULATION",
    population_tooltip:
      "The starting number of full arrangements ('attempts') compared side by side in the first RUN's rounds - later RUNs use a proportionally bigger population to search their wider rotation grid.",
    mutation_label: "MUTATION %",
    mutation_tooltip:
      "The chance that a small random change (swap two pieces' order, or reroll one piece's rotation) gets made when mixing two good attempts together to make a new one. Keeps the search from getting stuck repeating itself. Stays the same across every RUN.",
    mutation_hint: "MUTATION %: chance of a random tweak (reordering parts or rerolling a rotation) when breeding two good attempts together - keeps the search from stalling on one arrangement.",
    generations_label: "STARTING GENERATIONS",
    generations_tooltip:
      "The starting number of rounds ('try a bunch of attempts, keep the best, breed a new batch, repeat') the first RUN does - later RUNs use proportionally more rounds to search their wider rotation grid.",
    dominant_label: "DOMINANT AREA",
    dominant_tooltip:
      "If one piece alone takes up more than this fraction of a sheet (0.9 = 90%), the program stops trying to fit more onto that sheet and starts a fresh one, since there's basically no room left anyway. It still gets placed - it just gets a sheet to itself instead of sharing.",
    dominant_hint: "DOMINANT AREA: a part alone taking up more than this share of a sheet gets that sheet to itself instead of sharing (still placed) - flagged live as CLOSES SHEET in the shapes table above.",
    max_threads_label: "MAX CPU THREADS",
    max_threads_tooltip:
      "How many CPU cores the program is allowed to use at once while it searches for a good packing. 0 = use every core your computer has (fastest). Set a lower number to leave some CPU free for other programs while it runs.",
    seed_label: "SEED",
    seed_tooltip:
      "Starting point for the search's randomness (initial attempt order/rotations, every mutation/breeding choice across every round). The same seed with the same everything else always reproduces the exact same result - change it to sample a different random starting point, keep it fixed to compare other settings fairly.",

    bottom_bar_summary_tooltip: "Sheets used / parts unplaced / material utilisation for the current best result - stays visible even while this bar is collapsed.",

    btn_run: "RUN NEST",
    btn_stop: "STOP",
    console_title: "CONSOLE",

    app_settings_title: "App settings",
    lang_switch_title: "Language",
    lang_switch_label: "LANGUAGE",
    accent_switch_title: "Accent color",
    accent_switch_label: "ACCENT",
    accent_yellow: "Yellow",
    accent_orange: "Orange",
    accent_green: "Green",
    accent_cyan: "Cyan",
    accent_magenta: "Magenta",
    accent_hex_label: "HEX",
    accent_hex_tooltip: "Type any hex color code (e.g. #ffc400)",
    scale_switch_title: "Text size",
    scale_switch_label: "TEXT SIZE",
    scale_small: "SMALL",
    scale_normal: "NORMAL",
    scale_large: "BIG",

    import_importing: "importing {n} file(s)...",
    import_status_ok: "{n} shape(s) imported ({total} total)",
    import_status_none: "no shapes imported - see console",
    rect_invalid_size: "width and height must both be greater than 0",
    run_need_sheet: "mark at least one shape as SHEET",
    run_need_part: "mark at least one shape as PART with quantity > 0",
    run_invalid_config_field: "\"{field}\" in the settings is not a valid number - check that field and try again",
    run_status_running: "nesting...",
    run_status_stopped: "stopped early",
    run_status_done: "done",
    run_status_failed: "nest failed: {err}",
    unplaced_label_no_room: "no room found",
    unplaced_detail_no_room: "Didn't find room in this run - try more generations, a smaller margin/spacing, or fewer competing parts.",
    unplaced_label_too_large: "too large for any sheet",
    unplaced_detail_too_large: "Too large to fit on any available sheet at all (checked its own width/height against every sheet's), even by itself.",
    bottom_bar_summary: "sheets {sheets} · unplaced {unplaced} · util {util}%",
    stat_fitness: "FITNESS",
    stat_utilisation: "UTILISATION",
    stat_unplaced: "UNPLACED",
    stat_sheets_used: "SHEETS USED",
    sheet_caption: "SHEET {n} — {parts} part(s), {util}% used",
    repack_tooltip: "Re-pack this sheet's own parts in place (same settings as the run), keeping it only if it's actually better - never pulls parts from other sheets.",
    repack_button: "REPACK",
    repack_needs_config: "This result was recovered from an older session that did not save its settings - run a nest first to enable REPACK.",
    repack_status_running: "repacking sheet {n}...",
    repack_status_improved: "sheet {n} improved ({util}% used)",
    repack_status_no_improvement: "sheet {n}: no improvement found, kept as-is",
    repack_status_failed: "repack sheet {n} failed: {err}",
    history_option: "#{i} gen {gen}{best} - fitness {fitness}, {unplaced} unplaced",
    history_best_suffix: " (best)",
    export_invalid_spacing: "sheet spacing must be 0 or more",
    export_dialog_failed: "couldn't open the save dialog: {err}",
    export_status_running: "exporting...",
    export_status_done: "exported",
    dominant_closes_sheet: "CLOSES SHEET",
    role_part: "PART",
    role_sheet: "SHEET",
    role_skip: "SKIP",
    confirm_remove_message: "Remove {n} selected shape(s) from the import list? This cannot be undone.",
    confirm_remove_title: "Remove shapes",
    recover_message: "A saved nest result from a previous session exists ({sheets} sheet(s), {util}% utilisation). Recover it?",
    recover_title: "Recover last session?",

    btn_reset: "RESET",
    btn_reset_tooltip: "Clear the imported parts/sheets and the last nest result. Config settings (margin, spacing, rotations, etc.) are kept. Cannot be undone.",
    confirm_reset_message: "Clear all imported parts/sheets and the last nest result? Config settings are kept. This cannot be undone.",
    confirm_reset_title: "Reset",

    help_button_title: "How to use this app",
    help_title: "HOW TO USE RUSTYNESTING",
    help_intro: "RustyNesting nests parts onto stock sheets before cutting.",
    help_step_import: "01 IMPORT — load DXF/SVG file(s), or add a rectangle by hand.",
    help_step_roles: "02 ASSIGN ROLES — mark each shape SHEET (stock) or PART (to cut), set quantities.",
    help_step_configure: "03 CONFIGURE — adjust margin/spacing etc. at the bottom if needed (optional).",
    help_step_run: "RUN NEST — then review and EXPORT the result.",
    help_tip: "Hover any label for more detail.",
    help_dont_show: "Don't show this again",
    help_close: "GOT IT",
  },

  vi: {
    heading_import_text: "NHẬP DXF / SVG",
    tolerance_label: "DUNG SAI (mm)",
    tolerance_tooltip:
      "Đường cong (cung/tròn) được xấp xỉ bằng đoạn thẳng chính xác đến mức nào, tính bằng milimét. Thấp hơn = chính xác hơn nhưng nhiều điểm hơn; cao hơn = ít điểm hơn nhưng hình dạng thô hơn.",
    btn_browse: "DUYỆT TỆP...",
    dropzone_text: "hoặc kéo thả một hoặc nhiều tệp .dxf/.svg vào đây",
    rect_hint: "hoặc thêm trực tiếp một hình chữ nhật (kích thước tấm phôi, hoặc một chi tiết chữ nhật đơn giản) mà không cần tệp DXF/SVG:",
    rect_width_label: "CHIỀU RỘNG (mm)",
    rect_height_label: "CHIỀU CAO (mm)",
    rect_layer_label: "TÊN LỚP",
    btn_add_rect: "THÊM HÌNH CHỮ NHẬT",

    svg_unit_title: "ĐƠN VỊ SVG",
    svg_unit_intro: "Một đơn vị tọa độ trong tệp SVG này tương ứng với bao nhiêu ngoài đời thực?",
    svg_unit_label: "ĐƠN VỊ TỆP",
    svg_unit_auto: "Tự động nhận diện từ tệp (khuyến nghị)",
    svg_unit_mm: "Milimét (mm)",
    svg_unit_cm: "Xentimét (cm)",
    svg_unit_m: "Mét (m)",
    svg_unit_px: "Điểm ảnh (px, 96/inch)",
    svg_unit_cancel: "HỦY",
    svg_unit_ok: "NHẬP",

    heading_roles_text: "GÁN VAI TRÒ",
    btn_mark_all_parts: "TẤT CẢ = CHI TIẾT",
    btn_mark_all_parts_tooltip: "Đặt VAI TRÒ của mọi hình đã nhập thành CHI TIẾT chỉ bằng một cú nhấp, thay vì chọn từng dòng một.",
    btn_mark_all_sheets: "TẤT CẢ = TẤM PHÔI",
    btn_mark_all_sheets_tooltip: "Đặt VAI TRÒ của mọi hình đã nhập thành TẤM PHÔI chỉ bằng một cú nhấp, thay vì chọn từng dòng một.",
    btn_remove_selected: "XÓA MỤC ĐÃ CHỌN",
    btn_remove_selected_tooltip: "Xóa vĩnh viễn mọi hình đã đánh dấu bên dưới khỏi danh sách nhập. Không thể hoàn tác.",
    toggle_collapse_tooltip: "Thu gọn/Mở rộng",
    roles_hint: "Đánh dấu mỗi hình đã nhập là TẤM PHÔI (vật liệu để xếp lên) hoặc CHI TIẾT (thứ cần xếp), và đặt số lượng.",
    select_all_tooltip: "Chọn tất cả",
    th_index: "#",
    th_name: "TÊN",
    th_bbox: "KHUNG BAO (R×C)",
    th_preview: "XEM TRƯỚC",
    th_role: "VAI TRÒ",
    th_qty: "SL",
    th_grain: "GÓC",
    th_grain_tooltip:
      "Giới hạn các góc mà chi tiết này được phép đặt. Dùng khi vật liệu có hướng: vân gỗ nhìn thấy, bề mặt xước, hay họa tiết in phải chạy cùng chiều trên mọi chi tiết. TẤT CẢ cho phép dùng mọi góc (xếp tốt nhất).",
    th_part_mirror: "LẬT",
    th_part_mirror_tooltip:
      "Chi tiết cụ thể này có được cắt ở trạng thái lật ngược hay không, ghi đè thiết lập LẬT NGƯỢC chung. Chọn CẤM cho chi tiết phải giữ nguyên một mặt (mặt đẹp, mặt in, chi tiết không đối xứng).",
    rot_any: "TẤT CẢ",
    rot_0_180: "Chỉ 0 / 180",
    rot_quarter: "Bước 90°",
    rot_fixed: "Chỉ 0°",
    mirror_job: "THEO JOB",
    mirror_allow: "CHO PHÉP",
    mirror_deny: "CẤM",
    th_dominant: "CHIẾM ƯU THẾ?",
    th_dominant_tooltip:
      "Liệu chi tiết này một mình có chiếm đủ diện tích tấm phôi (theo thiết lập DIỆN TÍCH CHIẾM ƯU THẾ bên dưới) để đóng tấm phôi đó ngay sau khi đặt hay không - vẫn được đặt, chỉ là không có chi tiết nào khác cùng tấm.",

    heading_result_text: "KẾT QUẢ",
    view_attempt_label: "XEM LẦN THỬ",
    view_attempt_tooltip:
      "Mọi lần thử đã vượt qua kết quả tốt nhất trước đó trong quá trình chạy GA, theo thứ tự tìm thấy - không chỉ lần cuối cùng được giữ lại. Chọn một lần để xem cách sắp xếp đó thay vì kết quả cuối cùng.",
    unplaced_hint: "Các chi tiết này không thể đặt lên tấm phôi nào (viền đỏ bên dưới và trong danh sách gốc):",
    export_hint: "Xuất lần thử đang hiển thị ở trên ra tệp:",
    export_format_label: "ĐỊNH DẠNG",
    export_spacing_label: "KHOẢNG CÁCH GIỮA TẤM (mm)",
    export_spacing_tooltip:
      "Cả DXF lẫn SVG đều không có khái niệm riêng về 'tấm phôi' - mọi tấm phôi được dùng đều ghi vào cùng một bản vẽ, xếp từ trái sang phải. Đây là khoảng cách giữ giữa các tấm để chúng không chồng lên nhau.",
    export_outline_label: "BAO GỒM VIỀN TẤM PHÔI",
    export_outline_tooltip: "Cũng ghi hình chữ nhật của từng tấm phôi vào tệp xuất, không chỉ các chi tiết đặt trên đó.",
    export_unplaced_label: "BAO GỒM CHI TIẾT CHƯA ĐẶT",
    export_unplaced_tooltip:
      "Cũng ghi mọi chi tiết chưa từng được đặt lên tấm phôi nào, xếp thành lưới đơn giản không chồng lấn sau tấm phôi cuối cùng (không phải xếp nesting - chỉ để tránh chồng lấn cho bạn xem và xử lý thủ công).",
    btn_export: "XUẤT",

    margin_label: "LỀ (mm)",
    margin_tooltip:
      "Khoảng trống tối thiểu giữa một chi tiết và mép thật của tấm phôi - ví dụ để dao cắt có chỗ chạy mà không vượt ra ngoài vật liệu. Đặt 0 cho các công việc mà dao có thể an toàn tràn ra mép (như hầu hết cắt laser).",
    spacing_label: "KHOẢNG CÁCH (mm)",
    spacing_tooltip:
      "Khoảng cách tối thiểu giữ giữa hai chi tiết đặt cạnh nhau, để dao cắt vừa lọt giữa chúng và chúng không chạm nhau. Đặt 0 nếu các chi tiết có thể đặt sát mép nhau, không có khoảng cách.",
    runs_label: "SỐ LẦN CHẠY",
    runs_tooltip:
      "Số lần thử ngày càng kỹ lưỡng để tự động thực hiện. Lần thử đầu tiên rẻ và nhanh; mỗi lần sau thử thêm một góc xoay và ngân sách tìm kiếm lớn hơn lần trước, và lần thử nào xếp tốt nhất sẽ là kết quả bạn nhận được - bạn không cần biết gì về góc xoay/quần thể/thế hệ để việc này hoạt động tốt. Nhiều lần chạy hơn = kết quả tốt hơn, nhưng lâu hơn. THIẾT LẬP NÂNG CAO bên dưới cho phép bạn thay đổi điểm bắt đầu leo thang, nếu muốn.",
    cleanup_label: "NGƯỠNG DỌN DẸP (%, để trống = tắt)",
    cleanup_tooltip:
      "Sau khi chạy xong, bất kỳ tấm phôi nào có tỷ lệ sử dụng thấp hơn ngưỡng này sẽ được sắp xếp lại tại chỗ - cùng thiết lập, chỉ các chi tiết của tấm đó, không bao giờ lấy chi tiết từ tấm khác. Để trống để tắt. Bạn cũng có thể XẾP LẠI thủ công từng tấm từ màn hình KẾT QUẢ bất kể thiết lập này.",
    cleanup_placeholder: "vd. 60",
    cleanup_hint:
      "Các tấm phôi có tỷ lệ sử dụng dưới ngưỡng này sẽ tự động được xếp lại tại chỗ sau khi chạy (cùng chi tiết, sắp xếp chặt hơn) - để trống để tắt. Bạn cũng có thể XẾP LẠI thủ công bất kỳ tấm nào từ màn hình KẾT QUẢ.",
    mirror_label: "LẬT NGƯỢC CHI TIẾT (ĐỐI XỨNG)",
    mirror_tooltip:
      "Cho phép xếp chi tiết ở trạng thái lật ngược (đối xứng gương), không chỉ xoay - thường tiết kiệm được nhiều vật liệu với các chi tiết không đối xứng. Chỉ bật nếu vật liệu không phân mặt: không vân, không phủ, không in ấn hay mặt đẹp, và không có chi tiết nào bắt buộc phải nằm trên một mặt. Nếu vật liệu có phân mặt, chi tiết bị lật là phế phẩm.",
    mirror_on_badge: "ĐANG BẬT — CHI TIẾT CÓ THỂ BỊ CẮT NGƯỢC",
    mirror_run_warning: "ĐANG BẬT LẬT NGƯỢC",
    mirror_hint:
      "Chỉ dùng cho vật liệu không phân mặt (không vân, không phủ, không in ấn). Lần chạy 1 luôn được thực hiện không lật, nên bạn luôn có kết quả không lật để so sánh - lần chạy nào xếp tốt nhất sẽ được giữ. Luôn bắt đầu ở trạng thái TẮT mỗi lần mở ứng dụng, không khôi phục từ phiên trước.",
    btn_advanced_collapsed: "THIẾT LẬP NÂNG CAO ▾",
    btn_advanced_expanded: "THIẾT LẬP NÂNG CAO ▴",
    settings_bar_text: "CẤU HÌNH — THIẾT LẬP",

    placement_label: "KIỂU XẾP",
    placement_tooltip:
      "Cách chương trình đánh giá vị trí trượt một chi tiết vào chỗ. KHÍT CHẶT (khuyến nghị): đánh giá thuần túy dựa trên mức độ chi tiết áp sát các chi tiết đã đặt hoặc mép tấm phôi - lựa chọn có kết quả tốt nhất khi kiểm tra với bài test ghép khít thực tế, và không bao giờ kém hơn các lựa chọn khác trên công việc hình dạng hỗn hợp thông thường. Các tùy chọn còn lại được giữ để so sánh/dự phòng. TRỌNG LỰC + HIỆU CHỈNH: hai chi tiết đầu tiên của tấm phôi lắng xuống theo TRỌNG LỰC, sau đó mọi chi tiết tiếp theo chuyển sang phép đo tiếp xúc của KHÍT CHẶT. TRỌNG LỰC: để các chi tiết lắng về một góc, giống Tetris. HỘP: cố giữ mọi thứ đã đặt trong một hình chữ nhật nhỏ nhất có thể. BAO LỒI: đánh giá theo hình bao đơn giản hóa quanh mọi thứ đã đặt. TRỌNG LỰC + KHÍT CHẶT: dùng TRỌNG LỰC trước, rồi phá thế hòa giữa các vị trí tốt ngang nhau bằng chi tiết nào tiếp xúc nhiều hơn.",
    placement_opt_tightfit: "KHÍT CHẶT (KHUYẾN NGHỊ)",
    placement_opt_gravitycorrective: "TRỌNG LỰC + HIỆU CHỈNH",
    placement_opt_gravitytightfit: "TRỌNG LỰC + KHÍT CHẶT",
    placement_opt_gravity: "TRỌNG LỰC",
    placement_opt_box: "HỘP",
    placement_opt_convexhull: "BAO LỒI",
    placement_hint:
      "KHÍT CHẶT (khuyến nghị) đánh giá theo mức độ chi tiết áp sát các chi tiết lân cận - phù hợp nhất cho hình dạng không đều/ghép khít. TRỌNG LỰC để chi tiết lắng như Tetris; HỘP/BAO LỒI đánh giá theo hình bao tổng thể và chững lại sớm hơn với hình dạng không đều. TRỌNG LỰC + HIỆU CHỈNH/KHÍT CHẶT là kiểu lai. Di chuột vào KIỂU XẾP để xem so sánh đầy đủ.",

    rotations_label: "SỐ GÓC XOAY BAN ĐẦU",
    rotations_tooltip:
      "Số góc xoay ban đầu mà LẦN CHẠY đầu tiên thử cho mỗi chi tiết - mỗi LẦN CHẠY sau thử thêm một góc so với lần trước (xem SỐ LẦN CHẠY ở trên), vì vậy đây chỉ là điểm khởi đầu, không phải giới hạn cố định.",
    population_label: "QUẦN THỂ BAN ĐẦU",
    population_tooltip:
      "Số lượng cách sắp xếp đầy đủ ('lần thử') ban đầu được so sánh song song trong các vòng của LẦN CHẠY đầu tiên - các LẦN CHẠY sau dùng quần thể lớn hơn tương ứng để tìm kiếm trên lưới góc xoay rộng hơn.",
    mutation_label: "TỶ LỆ ĐỘT BIẾN %",
    mutation_tooltip:
      "Xác suất xảy ra một thay đổi ngẫu nhiên nhỏ (đổi chỗ thứ tự hai chi tiết, hoặc quay lại góc xoay của một chi tiết) khi lai hai lần thử tốt để tạo ra lần thử mới. Giúp việc tìm kiếm không bị lặp lại chính nó. Giữ nguyên xuyên suốt mọi LẦN CHẠY.",
    mutation_hint: "TỶ LỆ ĐỘT BIẾN %: xác suất xảy ra một thay đổi ngẫu nhiên nhỏ (sắp xếp lại chi tiết hoặc quay lại góc xoay) khi lai hai lần thử tốt - giúp việc tìm kiếm không bị chững lại ở một cách sắp xếp.",
    generations_label: "SỐ THẾ HỆ BAN ĐẦU",
    generations_tooltip:
      "Số vòng ban đầu ('thử một loạt, giữ lại tốt nhất, lai tạo lứa mới, lặp lại') mà LẦN CHẠY đầu tiên thực hiện - các LẦN CHẠY sau dùng nhiều vòng hơn tương ứng để tìm kiếm trên lưới góc xoay rộng hơn.",
    dominant_label: "DIỆN TÍCH CHIẾM ƯU THẾ",
    dominant_tooltip:
      "Nếu một chi tiết duy nhất chiếm hơn tỷ lệ này của tấm phôi (0.9 = 90%), chương trình sẽ ngừng cố xếp thêm vào tấm đó và bắt đầu tấm mới, vì gần như không còn chỗ trống. Chi tiết vẫn được đặt - chỉ là nó chiếm riêng một tấm thay vì chia sẻ.",
    dominant_hint: "DIỆN TÍCH CHIẾM ƯU THẾ: một chi tiết duy nhất chiếm hơn tỷ lệ này của tấm phôi sẽ chiếm riêng tấm đó thay vì chia sẻ (vẫn được đặt) - được đánh dấu trực tiếp là ĐÓNG TẤM trong bảng hình ở trên.",
    max_threads_label: "SỐ LUỒNG CPU TỐI ĐA",
    max_threads_tooltip:
      "Số lõi CPU chương trình được phép dùng cùng lúc khi tìm cách xếp tốt. 0 = dùng mọi lõi máy tính có (nhanh nhất). Đặt số thấp hơn để chừa CPU cho chương trình khác trong khi chạy.",
    seed_label: "HẠT GIỐNG",
    seed_tooltip:
      "Điểm khởi đầu cho tính ngẫu nhiên của việc tìm kiếm (thứ tự/góc xoay ban đầu, mọi lựa chọn đột biến/lai tạo ở mỗi vòng). Cùng một hạt giống với mọi thứ khác giữ nguyên sẽ luôn cho ra đúng một kết quả - thay đổi để lấy mẫu điểm khởi đầu ngẫu nhiên khác, giữ cố định để so sánh công bằng các thiết lập khác.",

    bottom_bar_summary_tooltip: "Số tấm đã dùng / chi tiết chưa xếp / tỷ lệ sử dụng vật liệu của kết quả tốt nhất hiện tại - vẫn hiển thị ngay cả khi thanh này đang thu gọn.",

    btn_run: "CHẠY XẾP HÌNH",
    btn_stop: "DỪNG",
    console_title: "NHẬT KÝ",

    app_settings_title: "Cài đặt ứng dụng",
    lang_switch_title: "Ngôn ngữ",
    lang_switch_label: "NGÔN NGỮ",
    accent_switch_title: "Màu nhấn",
    accent_switch_label: "MÀU NHẤN",
    accent_yellow: "Vàng",
    accent_orange: "Cam",
    accent_green: "Xanh lá",
    accent_cyan: "Xanh lam nhạt",
    accent_magenta: "Hồng cánh sen",
    accent_hex_label: "MÃ HEX",
    accent_hex_tooltip: "Nhập bất kỳ mã màu hex nào (vd. #ffc400)",
    scale_switch_title: "Cỡ chữ",
    scale_switch_label: "CỠ CHỮ",
    scale_small: "NHỎ",
    scale_normal: "BÌNH THƯỜNG",
    scale_large: "LỚN",

    import_importing: "đang nhập {n} tệp...",
    import_status_ok: "đã nhập {n} hình ({total} tổng cộng)",
    import_status_none: "không nhập được hình nào - xem nhật ký",
    rect_invalid_size: "chiều rộng và chiều cao phải lớn hơn 0",
    run_need_sheet: "đánh dấu ít nhất một hình là TẤM PHÔI",
    run_need_part: "đánh dấu ít nhất một hình là CHI TIẾT với số lượng > 0",
    run_invalid_config_field: "\"{field}\" trong thiết lập không phải là số hợp lệ - kiểm tra lại trường này và thử lại",
    run_status_running: "đang xếp hình...",
    run_status_stopped: "đã dừng sớm",
    run_status_done: "hoàn tất",
    run_status_failed: "xếp hình thất bại: {err}",
    unplaced_label_no_room: "không tìm được chỗ",
    unplaced_detail_no_room: "Không tìm được chỗ trong lần chạy này - hãy thử nhiều thế hệ hơn, giảm lề/khoảng cách, hoặc giảm số chi tiết cạnh tranh.",
    unplaced_label_too_large: "quá lớn so với mọi tấm phôi",
    unplaced_detail_too_large: "Quá lớn để vừa với bất kỳ tấm phôi nào (đã so sánh chiều rộng/cao với từng tấm), kể cả khi đặt một mình.",
    bottom_bar_summary: "tấm {sheets} · chưa xếp {unplaced} · sử dụng {util}%",
    stat_fitness: "ĐỘ THÍCH NGHI",
    stat_utilisation: "TỶ LỆ SỬ DỤNG",
    stat_unplaced: "CHƯA XẾP",
    stat_sheets_used: "SỐ TẤM ĐÃ DÙNG",
    sheet_caption: "TẤM {n} — {parts} chi tiết, đã dùng {util}%",
    repack_tooltip: "Xếp lại tại chỗ các chi tiết của riêng tấm này (cùng thiết lập với lần chạy), chỉ giữ lại nếu thực sự tốt hơn - không bao giờ lấy chi tiết từ tấm khác.",
    repack_button: "XẾP LẠI",
    repack_needs_config: "Kết quả này được khôi phục từ phiên cũ không lưu thiết lập - hãy chạy một lần xếp mới để dùng XẾP LẠI.",
    repack_status_running: "đang xếp lại tấm {n}...",
    repack_status_improved: "tấm {n} đã cải thiện (đã dùng {util}%)",
    repack_status_no_improvement: "tấm {n}: không cải thiện được, giữ nguyên",
    repack_status_failed: "xếp lại tấm {n} thất bại: {err}",
    history_option: "#{i} thế hệ {gen}{best} - độ thích nghi {fitness}, {unplaced} chưa xếp",
    history_best_suffix: " (tốt nhất)",
    export_invalid_spacing: "khoảng cách giữa tấm phải từ 0 trở lên",
    export_dialog_failed: "không mở được hộp thoại lưu tệp: {err}",
    export_status_running: "đang xuất...",
    export_status_done: "đã xuất",
    dominant_closes_sheet: "ĐÓNG TẤM",
    role_part: "CHI TIẾT",
    role_sheet: "TẤM PHÔI",
    role_skip: "BỎ QUA",
    confirm_remove_message: "Xóa {n} hình đã chọn khỏi danh sách nhập? Không thể hoàn tác.",
    confirm_remove_title: "Xóa hình",
    recover_message: "Có kết quả xếp hình đã lưu từ phiên trước ({sheets} tấm, sử dụng {util}%). Khôi phục?",
    recover_title: "Khôi phục phiên trước?",

    btn_reset: "ĐẶT LẠI",
    btn_reset_tooltip: "Xóa các chi tiết/tấm phôi đã nhập và kết quả xếp hình gần nhất. Giữ nguyên cấu hình (lề, khoảng cách, góc xoay, v.v.). Không thể hoàn tác.",
    confirm_reset_message: "Xóa toàn bộ chi tiết/tấm phôi đã nhập và kết quả xếp hình gần nhất? Cấu hình sẽ được giữ nguyên. Không thể hoàn tác.",
    confirm_reset_title: "Đặt lại",

    help_button_title: "Cách sử dụng ứng dụng này",
    help_title: "CÁCH SỬ DỤNG RUSTYNESTING",
    help_intro: "RustyNesting xếp các chi tiết lên tấm phôi trước khi cắt.",
    help_step_import: "01 NHẬP — tải tệp DXF/SVG, hoặc thêm hình chữ nhật thủ công.",
    help_step_roles: "02 GÁN VAI TRÒ — đánh dấu mỗi hình là TẤM PHÔI (vật liệu) hoặc CHI TIẾT (cần cắt), đặt số lượng.",
    help_step_configure: "03 CẤU HÌNH — chỉnh lề/khoảng cách... ở dưới nếu cần (không bắt buộc).",
    help_step_run: "CHẠY XẾP HÌNH — rồi xem và XUẤT kết quả.",
    help_tip: "Di chuột vào bất kỳ nhãn nào để xem thêm chi tiết.",
    help_dont_show: "Không hiển thị lại",
    help_close: "ĐÃ HIỂU",
  },
};

const STORAGE_KEY = "rustynesting-lang";
let currentLang = translations[localStorage.getItem(STORAGE_KEY)] ? localStorage.getItem(STORAGE_KEY) : "en";

export function getLang() {
  return currentLang;
}

// A thrown value substituted into a {err} placeholder isn't always a plain
// string - Tauri command failures are (Result<T, String> on the Rust side),
// but a native dialog rejection or a genuine JS bug could hand back an
// Error object instead, which template-literal coercion stringifies to the
// useless "[object Object]" rather than its actual message.
const stringifyVar = (v) => (v instanceof Error ? v.message : typeof v === "object" && v !== null ? String(v.message ?? v) : String(v));

// Falls back to English for any key missing from the current language -
// what makes adding a third language later safe to do incrementally
// (a partial dictionary degrades to English instead of showing raw keys).
export function t(key, vars) {
  let str = translations[currentLang]?.[key] ?? translations.en[key] ?? key;
  if (vars) {
    for (const [k, v] of Object.entries(vars)) str = str.replaceAll(`{${k}}`, stringifyVar(v));
  }
  return str;
}

// Sweeps the static markup for the data-i18n* attribute convention -
// index.html tags every translatable element once; switching languages is
// just re-running this, no per-element JS wiring needed.
export function applyStaticTranslations() {
  document.documentElement.lang = currentLang;
  document.querySelectorAll("[data-i18n]").forEach((node) => {
    node.textContent = t(node.dataset.i18n);
  });
  document.querySelectorAll("[data-i18n-title]").forEach((node) => {
    node.title = t(node.dataset.i18nTitle);
  });
  document.querySelectorAll("[data-i18n-placeholder]").forEach((node) => {
    node.placeholder = t(node.dataset.i18nPlaceholder);
  });
}

export function setLang(lang) {
  currentLang = translations[lang] ? lang : "en";
  localStorage.setItem(STORAGE_KEY, currentLang);
  applyStaticTranslations();
}
