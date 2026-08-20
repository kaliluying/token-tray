use std::fs::{create_dir_all, OpenOptions};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager, Runtime};

pub fn record<R: Runtime>(app: &AppHandle<R>, event: &'static str, result: &'static str) {
    let Ok(directory) = app.path().app_log_dir() else {
        return;
    };
    if create_dir_all(&directory).is_err() {
        return;
    }

    let path = directory.join("token-tray.log");
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default();
    let line = format!("{timestamp} event={event} result={result}\n");
    let _ = file.write_all(line.as_bytes());
}
