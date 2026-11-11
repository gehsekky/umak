// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, Runtime, State};
use umak_core::api::{self, ApiError, Core, CoreEvent, EventSink};
use umak_core::db::{Database, DbError};
use umak_core::player_engine::EnginePlayer;
use umak_media::MediaSession;

const EVENT_CHANNEL: &str = "umak:event";
/// Points the app at another data folder, so test runs leave the real database alone.
const DATA_DIR_VARIABLE: &str = "UMAK_DATA_DIR";

/// Forwards core events to the webview, and to the system's media controls if there are any.
struct AppEvents<R: Runtime> {
    app: AppHandle<R>,
    media: Option<MediaSession>,
}

impl<R: Runtime> EventSink for AppEvents<R> {
    fn emit(&self, event: &CoreEvent) {
        if let Err(error) = self.app.emit(EVENT_CHANNEL, event) {
            eprintln!("failed to emit {event:?}: {error}");
        }
        if let Some(media) = &self.media {
            media.emit(event);
        }
    }
}

/// The media controls for the main window. Their presses run as commands, like the UI's.
fn media_session<R: Runtime>(app: &AppHandle<R>) -> Option<MediaSession> {
    #[cfg(windows)]
    let window = Some(app.get_webview_window("main")?.hwnd().ok()?.0);
    #[cfg(not(windows))]
    let window = None;
    let handle = app.clone();
    let run = Arc::new(move |name: &'static str, args: Value| {
        if let Some(core) = handle.try_state::<Core>()
            && let Err(error) = api::dispatch(&core, name, args)
        {
            eprintln!(
                "failed to run {name} from the media controls: {}",
                error.message
            );
        }
    });
    MediaSession::start(window, run)
        .map_err(|error| eprintln!("no media controls: {error}"))
        .ok()
}

/// The single IPC entry point. It runs off the main thread so a slow command cannot
/// stall the window.
#[tauri::command(async)]
fn call(core: State<'_, Core>, name: String, args: Value) -> Result<Value, ApiError> {
    api::dispatch(&core, &name, args)
}

fn manage_core<R: Runtime>(app: &AppHandle<R>, database: &Path) -> Result<(), DbError> {
    let db = Database::open(database)?;
    let events = AppEvents {
        app: app.clone(),
        media: media_session(app),
    };
    let core = Core::with_player(db, Box::new(events), Box::new(EnginePlayer::spawn))?;
    core.keep_art_cached();
    app.manage(core);
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = match std::env::var_os(DATA_DIR_VARIABLE) {
                Some(folder) => PathBuf::from(folder),
                None => app.path().app_data_dir()?,
            };
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
