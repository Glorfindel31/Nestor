mod commands;
mod dto;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(commands::NestCancelFlag::default())
        .invoke_handler(tauri::generate_handler![
            commands::import_dxf_command,
            commands::import_svg_command,
            commands::run_nest_command,
            commands::cancel_nest_command,
            commands::export_dxf_command,
            commands::export_svg_command,
            commands::repack_sheet_command,
            commands::append_log_command,
            commands::save_config_command,
            commands::load_config_command,
            commands::load_best_result_command,
            commands::clear_best_result_command,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
