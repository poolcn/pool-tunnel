mod commands;
mod config;
mod defender;
mod gost;
mod models;
mod net;
mod ping;
mod reporter;
mod state;

use std::path::PathBuf;

use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // 单实例：已有实例时激活主窗口
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        .setup(|app| {
            let data_dir: PathBuf = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::env::temp_dir().join("pool-tunnel"));
            app.manage(state::AppState::new(data_dir));

            // Windows：启动即添加 Defender 排除项
            #[cfg(target_os = "windows")]
            {
                let _ = defender::add_gost_exclusion();
            }

            commands::start_background_tasks(app.handle().clone());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_initial_state,
            commands::refresh_config,
            commands::rebuild_groups_with_lan_ip,
            commands::start_tunnel,
            commands::stop_tunnel,
            commands::get_logs,
            commands::get_status,
            commands::set_selected,
            commands::ping_delay,
            commands::get_app_version,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // 应用退出前停止 gost 子进程，避免孤儿进程占用端口/继续转发
            if let tauri::RunEvent::ExitRequested { .. } = event {
                app_handle.state::<state::AppState>().gost.stop(app_handle);
            }
        });
}
