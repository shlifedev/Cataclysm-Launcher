#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod launcher;

use launcher::{
    create_backup, disconnect_webdav, export_backup, fetch_releases, get_webdav_connection,
    install_release, launch_installation, list_backups, list_installations, list_remote_backups,
    restore_remote_backup, reveal_backup, reveal_installation, reveal_installation_location,
    save_webdav_connection, test_webdav_connection, upload_backup, AppState,
};

fn main() {
    let http = reqwest::Client::builder()
        .user_agent("Cataclysm-Hub/0.1")
        .build()
        .expect("HTTP client should initialize");

    tauri::Builder::default()
        .manage(AppState { http })
        .invoke_handler(tauri::generate_handler![
            fetch_releases,
            install_release,
            list_installations,
            launch_installation,
            reveal_installation,
            reveal_installation_location,
            create_backup,
            list_backups,
            reveal_backup,
            export_backup,
            get_webdav_connection,
            test_webdav_connection,
            save_webdav_connection,
            disconnect_webdav,
            list_remote_backups,
            upload_backup,
            restore_remote_backup
        ])
        .run(tauri::generate_context!())
        .expect("error while running Cataclysm Hub");
}
