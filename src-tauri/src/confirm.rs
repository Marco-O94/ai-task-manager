//! Native confirmations (spec §10.2): a script in the webview cannot click them.

use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

/// OK/Cancel warning dialog, shown with `blocking_show` off the main thread; `true` = OK.
/// In a debug E2E run the answer is the one the run queued (`e2e::take_confirm`).
pub async fn confirm_native(app: &AppHandle, title: &str, message: &str) -> bool {
    #[cfg(debug_assertions)]
    if let Some(answer) = crate::e2e::take_confirm(title, message) {
        return answer;
    }
    let dialog = app
        .dialog()
        .message(message)
        .title(title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancel);
    tauri::async_runtime::spawn_blocking(move || dialog.blocking_show())
        .await
        .unwrap_or(false)
}
