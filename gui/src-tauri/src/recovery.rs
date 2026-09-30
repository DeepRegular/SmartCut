//! 自動保存: the list as it stands, kept somewhere a crash cannot take it.
//!
//! A project is written when somebody saves it, and not before. Twenty
//! recordings' worth of cuts made over an evening and not yet saved were in
//! the window's memory and nowhere else, and a power cut or a crash took all
//! of it. So while there is work that is not on disc, the list window writes
//! it here as well -- the same `.scproj` a save would write, a few seconds
//! after each change -- and takes it away again once the work is saved or
//! the window closes normally. What is still here at the next start is work
//! a window never got to put down.
//!
//! **Whose file is whose is a lock, not a process id.** Every list window is
//! a process of its own and several can be up at once, each with its own
//! file here. A window holds a lock on its file's companion `.lock` for as
//! long as it runs; the operating system lets go of it when the process ends,
//! however it ends. So a file whose lock can be taken is one whose window is
//! gone, and the window that takes it keeps it -- two windows started at
//! once cannot both offer the same work back.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tauri::Manager;

/// This window's own file, once it has written one, and what it has taken
/// over from windows that are gone.
#[derive(Default)]
pub struct Recovery {
    own: Mutex<Option<Own>>,
    claimed: Mutex<Vec<(String, File)>>,
}

struct Own {
    id: String,
    /// Held for the life of the process. Dropping it lets the lock go.
    _lock: File,
}

fn locked<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Beside the program's other data rather than in its caches: 環境設定 can
/// empty the caches, and this is the one thing in them that is somebody's
/// work.
fn dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("recovery");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn body_at(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.scproj"))
}

fn lock_at(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.lock"))
}

/// Only names this module made: digits and a dash.
fn plain(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_digit() || c == '-')
}

/// Put the list down, over whatever this window put down last.
///
/// Written beside and renamed over, so that a crash in the middle of this
/// write leaves the last whole copy rather than half of this one.
#[tauri::command(async)]
pub fn recovery_put(
    app: tauri::AppHandle,
    state: tauri::State<Recovery>,
    body: String,
) -> Result<(), String> {
    let dir = dir(&app)?;
    let mut own = locked(&state.own);
    if own.is_none() {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let id = format!("{}-{millis}", std::process::id());
        let lock = File::create(lock_at(&dir, &id)).map_err(|e| e.to_string())?;
        lock.try_lock().map_err(|e| e.to_string())?;
        *own = Some(Own { id, _lock: lock });
    }
    let id = &own.as_ref().expect("set above").id;
    let at = body_at(&dir, id);
    let fresh = dir.join(format!("{id}.part"));
    std::fs::write(&fresh, body.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::rename(&fresh, &at).map_err(|e| e.to_string())
}

/// Take this window's copy away: the work is on disc, or there is none.
///
/// The lock stays held, so the name is this window's for the next change.
#[tauri::command(async)]
pub fn recovery_drop(app: tauri::AppHandle, state: tauri::State<Recovery>) {
    let Ok(dir) = dir(&app) else { return };
    if let Some(own) = locked(&state.own).as_ref() {
        let _ = std::fs::remove_file(body_at(&dir, &own.id));
    }
}

/// What a window that is gone left behind.
#[derive(serde::Serialize)]
pub struct Orphan {
    id: String,
    /// The project as it was written, for the window to read.
    body: String,
    /// When it was last written, in milliseconds since 1970.
    saved: u64,
}

/// Every copy whose window is gone, newest first, each now held by this one.
///
/// A lock with no copy beside it -- a window that saved its work and was
/// then killed -- is cleared away here too.
#[tauri::command(async)]
pub fn recovery_orphans(app: tauri::AppHandle, state: tauri::State<Recovery>) -> Vec<Orphan> {
    let Ok(dir) = dir(&app) else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    let own = locked(&state.own).as_ref().map(|o| o.id.clone());
    let mut found = Vec::new();
    let mut claimed = locked(&state.claimed);
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".lock"))
            .map(str::to_string)
        else {
            continue;
        };
        if !plain(&id) || own.as_deref() == Some(id.as_str()) || claimed.iter().any(|(c, _)| *c == id) {
            continue;
        }
        let Ok(lock) = std::fs::OpenOptions::new().read(true).write(true).open(&path) else {
            continue;
        };
        // Held by a window that is still up.
        if lock.try_lock().is_err() {
            continue;
        }
        let body_path = body_at(&dir, &id);
        match std::fs::read_to_string(&body_path) {
            Ok(body) => {
                let saved = std::fs::metadata(&body_path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                found.push(Orphan { id: id.clone(), body, saved });
                claimed.push((id, lock));
            }
            Err(_) => {
                drop(lock);
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(dir.join(format!("{id}.part")));
            }
        }
    }
    found.sort_by_key(|o| std::cmp::Reverse(o.saved));
    found
}

/// Throw away a copy this window took over: put back, or declined.
#[tauri::command(async)]
pub fn recovery_forget(app: tauri::AppHandle, state: tauri::State<Recovery>, id: String) {
    let Ok(dir) = dir(&app) else { return };
    let mut claimed = locked(&state.claimed);
    let Some(at) = claimed.iter().position(|(c, _)| *c == id) else { return };
    // The handle goes first: Windows will not remove a file that is open.
    drop(claimed.remove(at));
    let _ = std::fs::remove_file(body_at(&dir, &id));
    let _ = std::fs::remove_file(dir.join(format!("{id}.part")));
    let _ = std::fs::remove_file(lock_at(&dir, &id));
}

/// On the way out of a window that closed normally: its copy, and its lock.
///
/// A window closed on unsaved work has been asked whether to throw it away,
/// and said yes. What it took over and has not answered for is left alone:
/// its lock goes with the process, and the next start offers it again.
pub fn on_exit(app: &tauri::AppHandle) {
    let Ok(dir) = dir(app) else { return };
    let state = app.state::<Recovery>();
    let own = locked(&state.own).take();
    if let Some(own) = own {
        let id = own.id.clone();
        drop(own);
        let _ = std::fs::remove_file(body_at(&dir, &id));
        let _ = std::fs::remove_file(dir.join(format!("{id}.part")));
        let _ = std::fs::remove_file(lock_at(&dir, &id));
    }
}
