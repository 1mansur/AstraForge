#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
use astraforge_core::error::AppError;
use astraforge_core::service::{Engine, Request};
use serde_json::Value;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
#[tauri::command]
async fn request(request: Request, state: State<'_, Arc<Engine>>) -> Result<Value, AppError> {
    let engine = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || engine.handle(request))
        .await
        .map_err(|_| AppError::new("worker_failed", "The background operation failed"))?
}
fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let path = app.path().app_data_dir()?;
            std::fs::create_dir_all(&path)?;
            let handle = app.handle().clone();
            let engine = Engine::new(
                &path.join("astraforge.sqlite"),
                Arc::new(move |name, payload| {
                    if let Err(error) = handle.emit(name, payload) {
                        eprintln!("Desktop event failed: {error}");
                    }
                }),
            )?;
            app.manage(Arc::new(engine));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![request])
        .run(tauri::generate_context!())
        .expect("AstraForge desktop runtime failed");
}
