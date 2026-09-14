//! Icon Forge Tauri shell library: IPC commands, job-engine wiring, and the
//! managed library handle (ARCHITECTURE.md §2 native side).
//!
//! All commands are thin: they validate input, forward to [`isg_native`],
//! and translate errors into strings for the webview. Every long operation
//! is a job — commands only ever submit and return a job id; progress
//! arrives as `job://event` payloads on the webview side.

pub mod commands;
pub mod jobs;
pub mod state;

/// Builds the Tauri application (exposed for dev harnesses).
pub fn tauri_app() -> tauri::Builder<tauri::Wry> {
    commands::register(
        tauri::Builder::default()
            .manage(state::App::default())
            .setup(|app| {
                let handle = app.handle().clone();
                app.manage(jobs::spawn_engine(handle));
                Ok(())
            }),
    )
}

/// Runs the desktop app.
pub fn run() {
    tauri_app()
        .run(tauri::generate_context!())
        .expect("error while running Icon Forge");
}
