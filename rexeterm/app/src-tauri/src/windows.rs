use rexeterm_app::desktop::{dispatch, Desktop, Workspace};
use serde_json::Value;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::State;
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
async fn desktop_call(
    state: State<'_, Arc<Desktop>>,
    action: String,
    args: Option<Value>,
) -> Result<Value, String> {
    dispatch(
        state.inner(),
        &action,
        args.unwrap_or_else(|| serde_json::json!({})),
    )
    .await
    .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
async fn terminal_poll(
    state: State<'_, Arc<Desktop>>,
    id: String,
) -> Result<tauri::ipc::Response, String> {
    state
        .terminal_poll(&id)
        .await
        .map(tauri::ipc::Response::new)
        .map_err(|error| format!("{error:#}"))
}

#[tauri::command]
async fn pick_local(
    app: tauri::AppHandle,
    save: bool,
    name: Option<String>,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app.dialog().file();
        if let Some(name) = name {
            dialog = dialog.set_file_name(name);
        }
        let result = if save {
            dialog.blocking_save_file()
        } else {
            dialog.blocking_pick_file()
        };
        result
            .map(|path| {
                path.into_path()
                    .map(|path| path.to_string_lossy().into_owned())
                    .map_err(|e| e.to_string())
            })
            .transpose()
    })
    .await
    .map_err(|e| e.to_string())?
}

pub fn run() {
    let config = rexeterm_app::config::beside_executable()
        .expect("无法取得程序目录")
        .join("workspace.json");
    rexeterm_app::config::private_directory(config.parent().unwrap())
        .expect("无法创建程序旁配置目录");
    let defaults = Workspace {
        version: 1,
        last_host: None,
        hosts: vec![],
    };
    let desktop = tauri::async_runtime::block_on(Desktop::new(config, defaults))
        .expect("初始化 RexETerm 工作区失败");
    let closing = Arc::new(AtomicBool::new(false));
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(desktop.clone())
        .invoke_handler(tauri::generate_handler![
            desktop_call,
            terminal_poll,
            pick_local
        ])
        .build(tauri::generate_context!())
        .expect("创建 RexETerm 窗口失败")
        .run(move |app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if !closing.swap(true, Ordering::SeqCst) {
                    api.prevent_exit();
                    let desktop = desktop.clone();
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        if let Err(error) = desktop.shutdown().await {
                            eprintln!("RexETerm shutdown: {error:#}");
                        }
                        app.exit(0);
                    });
                }
            }
        });
}
