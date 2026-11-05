// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs;
use std::path::Path;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use umak_core::api::{self, ApiError, Core, CoreEvent, EventSink};
use umak_core::db::{Database, DbError};

const EVENT_CHANNEL: &str = "umak:event";

/// Forwards core events to the webview.
struct TauriEvents<R: Runtime>(AppHandle<R>);

impl<R: Runtime> EventSink for TauriEvents<R> {
    fn emit(&self, event: &CoreEvent) {
        if let Err(error) = self.0.emit(EVENT_CHANNEL, event) {
            eprintln!("failed to emit {event:?}: {error}");
        }
    }
}

/// The single IPC entry point. It runs off the main thread so a slow command cannot
/// stall the window.
#[tauri::command(async)]
fn call(core: State<'_, Core>, name: String, args: Value) -> Result<Value, ApiError> {
    api::dispatch(&core, &name, args)
}

fn manage_core<R: Runtime>(app: &AppHandle<R>, database: &Path) -> Result<(), DbError> {
    let db = Database::open(database)?;
    app.manage(Core::new(db, Box::new(TauriEvents(app.clone())))?);
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            fs::create_dir_all(&data_dir)?;
            manage_core(app.handle(), &data_dir.join("umak.db"))?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![call])
        .run(tauri::generate_context!())
        .expect("failed to run the Umak app");
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use serde_json::json;
    use tauri::Listener;
    use tauri::ipc::{CallbackFn, InvokeBody};
    use tauri::test::{
        INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder, mock_context, noop_assets,
    };
    use tauri::webview::InvokeRequest;

    use super::*;

    fn invoke(
        webview: &tauri::WebviewWindow<MockRuntime>,
        name: &str,
        args: Value,
    ) -> Result<Value, Value> {
        get_ipc_response(
            webview,
            InvokeRequest {
                cmd: "call".into(),
                callback: CallbackFn(0),
                error: CallbackFn(1),
                url: "http://tauri.localhost".parse().unwrap(),
                body: InvokeBody::Json(json!({ "name": name, "args": args })),
                headers: Default::default(),
                invoke_key: INVOKE_KEY.to_string(),
            },
        )
        .map(|body| body.deserialize().unwrap())
    }

    #[test]
    fn commands_and_events_cross_the_tauri_transport() {
        let temp = tempfile::tempdir().unwrap();
        let app = mock_builder()
            .invoke_handler(tauri::generate_handler![call])
            .build(mock_context(noop_assets()))
            .unwrap();
        manage_core(app.handle(), &temp.path().join("umak.db")).unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();

        let (sender, events) = mpsc::channel();
        app.listen(EVENT_CHANNEL, move |event| {
            sender.send(event.payload().to_owned()).unwrap();
        });

        let info = invoke(&webview, "app.info", json!({})).unwrap();
        assert_eq!(info["name"], "Umak");

        let folder = temp.path().join("Albums");
        fs::create_dir(&folder).unwrap();
        let added = invoke(
            &webview,
            "library.roots.add",
            json!({ "path": folder, "name": null }),
        )
        .unwrap();
        assert_eq!(added["name"], "Albums");

        let payload = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&payload).unwrap(),
            json!({ "type": "libraryRootsChanged" })
        );

        let error = invoke(&webview, "no.such.command", json!({})).unwrap_err();
        assert_eq!(error["code"], "unknownCommand");
    }
}
