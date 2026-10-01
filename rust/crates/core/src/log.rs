//! Where this program's own sentences go, and where libav's go with them.
//!
//! Everything the engine has to say about a run -- a track carried through
//! untouched, a caption stream it could not place, the seam it re-encoded --
//! is a line on standard error. That is the right place on the command line
//! and nowhere at all in the window: a webview on a desktop has no terminal,
//! and the sentence that would have explained a failed export was printed to
//! nobody.
//!
//! So a line goes through [`line`], which prints it where it always went and
//! also writes it into a file when one is open. The window opens one per run
//! (`logs/` beside the program's other files, one file a run) and the CLI
//! opens one for `--log`. Nothing else changes: with no file open this is
//! `eprintln!` by another name.
//!
//! **libav's lines join them.** What libav prints goes through
//! [`install_libav`]'s callback, which lets through exactly what the level
//! chosen in 環境設定 (or `SMARTCUT_FFMPEG_LOG`) lets through -- to the
//! terminal as before, and into the file. The level is the one answer for
//! both, so a log file never holds commentary somebody had asked not to see,
//! and turning the level up for a bug report puts it in the file that is
//! attached to the report.

use std::io::Write;
use std::sync::Mutex;

/// The file a run is being written down in, if one is.
static SINK: Mutex<Option<Sink>> = Mutex::new(None);

struct Sink {
    file: std::fs::File,
    /// The lines [`write_once`] has put in this file.
    once: std::collections::HashSet<String>,
}

/// Print a line of the engine's, and write it down if a run is being logged.
///
/// Use through [`crate::say!`], which formats like `eprintln!`.
pub fn line(text: &str) {
    // Not `eprintln!`, which panics when standard error has gone -- the
    // terminal a window was started from, closed with the window still up --
    // and every line said after that took down the thread saying it, an
    // export half written among them. See `libav_line`, which says the same.
    let _ = writeln!(std::io::stderr().lock(), "{}", plain(text));
    write_only(text);
}

/// `text` with its control characters made `?`, line breaks and tabs apart.
///
/// What [`libav_line`] does to libav's lines, done to the engine's: a line
/// can carry the file's own words -- its name, a track's title, a
/// broadcast's programme name -- and an escape sequence in one of them is not
/// to reach the terminal or the log.
pub fn plain(text: &str) -> std::borrow::Cow<'_, str> {
    let bad = |c: char| c.is_control() && c != '\n' && c != '\t';
    if text.contains(bad) {
        text.chars().map(|c| if bad(c) { '?' } else { c }).collect::<String>().into()
    } else {
        text.into()
    }
}

/// Write a line into the open log and nowhere else.
///
/// For what the window says about a run -- which clip, where it went, what
/// became of it. Those are already on screen; printing them to standard error
/// as well would be printing them twice to a terminal nobody has open.
pub fn write_only(text: &str) {
    let Ok(mut sink) = SINK.lock() else { return };
    if let Some(sink) = sink.as_mut() {
        put(&mut sink.file, text);
    }
}

/// Write a line into the open log unless this log already has it.
///
/// For a note the engine says once a process (see `note_once`): the window
/// is one process for many runs, and a note said at an earlier run -- or
/// when the recording was first added to the list -- belongs in each run's
/// log all the same.
pub fn write_once(text: &str) {
    let Ok(mut sink) = SINK.lock() else { return };
    if let Some(sink) = sink.as_mut() {
        if sink.once.insert(text.to_string()) {
            put(&mut sink.file, text);
        }
    }
}

fn put(file: &mut std::fs::File, text: &str) {
    // Each line whole, with its own newline, so that two threads saying
    // something at once leave two lines rather than one interleaved one.
    let mut buf = String::with_capacity(text.len() + 1);
    buf.push_str(&plain(text.trim_end_matches(['\r', '\n'])));
    buf.push('\n');
    let _ = file.write_all(buf.as_bytes());
}

/// Start writing lines down in `path`, in place of any file that was open.
///
/// Created, or added to where it is there already: the CLI's `--log` named
/// twice for two runs is one file with both in it, which is what somebody
/// who named the same file twice meant.
pub fn open(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    if let Ok(mut sink) = SINK.lock() {
        *sink = Some(Sink { file, once: Default::default() });
    }
    Ok(())
}

/// Stop writing lines down. The file is flushed and closed.
pub fn close() {
    if let Ok(mut sink) = SINK.lock() {
        if let Some(mut sink) = sink.take() {
            let _ = sink.file.flush();
        }
    }
}

/// Whether a run is being written down now.
pub fn is_open() -> bool {
    SINK.lock().map(|s| s.is_some()).unwrap_or(false)
}

/// Say something: to standard error, and into the run's log when one is open.
///
/// `eprintln!` in every other respect, which is what every line of the
/// engine's was written with.
#[macro_export]
macro_rules! say {
    ($($arg:tt)*) => {
        $crate::log::line(&format!($($arg)*))
    };
}

// --- libav --------------------------------------------------------------

/// What libav hands its log callback as the argument list: a pointer to the
/// one `__va_list_tag` on x86-64 System V, and a plain `char *` on Windows.
/// Nothing else is built, and a platform not named here keeps libav's own
/// callback -- its lines reach the terminal as before, and not the file.
#[cfg(all(unix, target_arch = "x86_64"))]
type VaList = *mut ffmpeg_next::ffi::__va_list_tag;
#[cfg(windows)]
type VaList = ffmpeg_next::ffi::va_list;

/// Route libav's lines through [`line`]'s two destinations. Once per process.
///
/// libav calls its callback for every line whatever the level, and leaves the
/// level to the callback -- which is what its own default does. So this one
/// asks the same question the default would, and a line the chosen level
/// turns away goes nowhere.
pub fn install_libav() {
    #[cfg(any(all(unix, target_arch = "x86_64"), windows))]
    {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| unsafe {
            ffmpeg_next::ffi::av_log_set_callback(Some(libav_line));
        });
    }
}

#[cfg(any(all(unix, target_arch = "x86_64"), windows))]
unsafe extern "C" fn libav_line(
    avcl: *mut std::ffi::c_void,
    level: std::ffi::c_int,
    fmt: *const std::ffi::c_char,
    vl: VaList,
) {
    use std::sync::atomic::{AtomicI32, Ordering};
    // A colour tint may ride in the second byte, as libav's default knows.
    let level = if level >= 0 { level & 0xff } else { level };
    if level > ffmpeg_next::ffi::av_log_get_level() {
        return;
    }
    // One line at a time, as libav's default holds its own lock: the prefix
    // flag below is shared, and a decoder's threads log at once.
    static ONE: Mutex<()> = Mutex::new(());
    let _one = ONE.lock().unwrap_or_else(|e| e.into_inner());
    // Whether the next piece begins a line and should carry the `[h264 @ …]`
    // prefix. libav's own default keeps the same one flag for every thread.
    static PREFIX: AtomicI32 = AtomicI32::new(1);
    let mut buf = [0 as std::ffi::c_char; 1024];
    let mut prefix = PREFIX.load(Ordering::Relaxed);
    let n = ffmpeg_next::ffi::av_log_format_line2(
        avcl,
        level,
        fmt,
        vl,
        buf.as_mut_ptr(),
        buf.len() as std::ffi::c_int,
        &mut prefix,
    );
    PREFIX.store(prefix, Ordering::Relaxed);
    if n <= 0 {
        return;
    }
    // What libav's default does before printing: a control character is
    // made a `?`. The text can be the file's own -- a tag, a name, a URL --
    // and an escape sequence in it is not to reach the terminal or the log.
    for c in buf.iter_mut().take_while(|c| **c != 0) {
        let b = *c as u8;
        if b < 0x08 || (0x0e..0x20).contains(&b) {
            *c = b'?' as std::ffi::c_char;
        }
    }
    let text = std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy();
    // libav writes a line in pieces and ends it with its own newline. The
    // terminal is given the pieces as they come, as libav's default gave them;
    // the file is given whole lines, which is what one line a piece would not
    // have been.
    // Not `eprint!`, which panics when standard error has gone (a closed
    // pipe) -- and a panic here, inside a C callback, aborts the process.
    // libav's own `fprintf` shrugged that off, and so does this.
    let mut err = std::io::stderr();
    let _ = err.write_all(text.as_bytes());
    let _ = err.flush();
    if is_open() {
        // `try_with`: a thread on its way out has no storage left, and
        // `with` would panic -- here, an abort.
        let _ = PARTIAL.try_with(|partial| {
            let mut partial = partial.borrow_mut();
            partial.push_str(&text);
            while let Some(end) = partial.find('\n') {
                let whole: String = partial.drain(..=end).collect();
                write_only(&whole);
            }
        });
    }
}

thread_local! {
    /// The piece of a libav line written so far on this thread.
    static PARTIAL: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}
