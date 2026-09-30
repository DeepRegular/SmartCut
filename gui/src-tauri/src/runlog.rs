//! 実行ログ: what one run said, in a file of its own.
//!
//! The output screen shows a run as it goes -- which clip, how far, what
//! became of each -- and all of that is gone the moment the window closes or
//! the next run starts. What the engine had to say underneath it never
//! reached the screen at all: it was printed to a terminal the window does
//! not have. So each run is also written down, as plain text, one file a
//! run: the window's own account of it, the engine's notes, and whatever
//! libav was allowed to say by 環境設定. It is the file to read when a run
//! failed, and the one to attach to a bug report.
//!
//! The lines themselves go through [`smartcut_core::log`], which is what the
//! engine already writes through; this is where the file is opened, named,
//! headed and, once there are too many, cleared away.

use std::path::{Path, PathBuf};

use tauri::Manager;

/// How many runs are kept. The oldest go first.
const KEEP: usize = 100;

/// Where they are kept: the platform's place for a program's logs.
fn dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_log_dir().map_err(|e| e.to_string())?.join("runs");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// Clear away all but the newest [`KEEP`] of them.
fn prune(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut logs: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "log"))
        .filter_map(|p| Some((std::fs::metadata(&p).ok()?.modified().ok()?, p)))
        .collect();
    if logs.len() <= KEEP {
        return;
    }
    logs.sort_by_key(|l| std::cmp::Reverse(l.0));
    for (_, p) in logs.into_iter().skip(KEEP) {
        let _ = std::fs::remove_file(p);
    }
}

/// Begin writing a run down. Answers the file's path.
///
/// `stamp` is when the run began as the window tells the time -- the local
/// time somebody will look for the file by -- and becomes its name. `head`
/// is what the window wants said first; what this program is and what it is
/// running on is said above it here, since that is the same for every run.
#[tauri::command(async)]
pub fn run_log_open(app: tauri::AppHandle, stamp: String, head: Vec<String>) -> Result<String, String> {
    let dir = dir(&app)?;
    prune(&dir);
    let stem: String = stamp
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    let stem = if stem.is_empty() { "run".to_string() } else { format!("run-{stem}") };
    let mut at = dir.join(format!("{stem}.log"));
    for n in 2..1000 {
        if !at.exists() {
            break;
        }
        at = dir.join(format!("{stem}-{n}.log"));
    }
    smartcut_core::log::open(&at).map_err(|e| format!("{}: {e}", at.display()))?;
    let libav = smartcut_core::libav();
    smartcut_core::log::write_only(&format!(
        "SmartCut {}  ({} {}; libavformat {}, libavcodec {}, libavutil {})",
        smartcut_core::VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH,
        libav.avformat,
        libav.avcodec,
        libav.avutil,
    ));
    for line in head {
        smartcut_core::log::write_only(&line);
    }
    Ok(at.to_string_lossy().into_owned())
}

/// Add the window's own lines to the run being written down.
#[tauri::command]
pub fn run_log_line(lines: Vec<String>) {
    for line in lines {
        smartcut_core::log::write_only(&line);
    }
}

/// The run is over.
#[tauri::command]
pub fn run_log_close() {
    smartcut_core::log::close();
}

/// Where the logs are, for 環境設定's button.
#[tauri::command(async)]
pub fn logs_folder(app: tauri::AppHandle) -> Result<String, String> {
    dir(&app).map(|d| d.to_string_lossy().into_owned())
}

/// Open one in whatever the desktop opens a text file with.
#[tauri::command(async)]
pub fn open_log(app: tauri::AppHandle, path: String) -> Result<(), String> {
    // Only a file of this program's own logs: the path comes from the
    // window, and this runs the desktop's opener on it.
    let dir = dir(&app)?;
    let at = Path::new(&path);
    let inside = at.parent().zip(dir.canonicalize().ok()).is_some_and(|(p, d)| {
        p.canonicalize().is_ok_and(|p| p == d)
    });
    if !inside || !at.is_file() || at.extension().is_none_or(|x| x != "log") {
        return Err(format!("not a run log: {path}"));
    }
    let opener = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener)
        .arg(at)
        .spawn()
        .map(crate::reap)
        .map_err(|e| format!("{opener}: {e}"))
}
