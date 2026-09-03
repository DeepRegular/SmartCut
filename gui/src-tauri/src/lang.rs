//! Which language this side's own sentences are written in.
//!
//! Most of what the user reads is the webview's, and lives in `i18n.js`.
//! What is left over is what only this side can say: why a recording would
//! not open, why a share is not there, and the phase names that go under a
//! progress bar while a pass runs. Those are made here, so the choice has to
//! reach here too.
//!
//! One global, because there is one user looking at one program: a language
//! is not a property of a window or of a call. It starts at whatever the
//! machine is set to, so that anything said before the frontend has finished
//! starting up is already right, and the frontend sets it for good the
//! moment it knows -- it is the one that holds the preference, and a user
//! who chose English on a Japanese machine means it.

use std::sync::atomic::{AtomicU8, Ordering};

const JA: u8 = 0;
const EN: u8 = 1;

static LANG: AtomicU8 = AtomicU8::new(JA);

/// Whether this side is speaking English. Anything that is not English is
/// Japanese, which is the language the program was written in and the one
/// every string is guaranteed to exist in.
pub fn is_en() -> bool {
    LANG.load(Ordering::Relaxed) == EN
}

/// Take a language, however it is named -- "en", "en-GB", "en_US.UTF-8".
/// Anything the program has no words for leaves the setting alone.
pub fn set(tag: &str) {
    let base = tag.split(['-', '_', '.']).next().unwrap_or("").to_ascii_lowercase();
    match base.as_str() {
        "en" => LANG.store(EN, Ordering::Relaxed),
        "ja" => LANG.store(JA, Ordering::Relaxed),
        _ => {}
    }
}

/// One of two literals, by the language in force.
///
/// Written as a macro rather than a function so that both wordings stay at
/// the point they are printed: a sentence and its translation read together,
/// and a table somewhere else is a table that goes stale.
macro_rules! tr {
    ($ja:expr, $en:expr $(,)?) => {
        if $crate::lang::is_en() {
            $en
        } else {
            $ja
        }
    };
}

/// The same, for a sentence with something filled into it.
///
/// `format!` wants a literal, so the choice cannot be made inside the call
/// and is made around it instead. Both wordings name the same holes -- they
/// are filled from the scope, as `format!` does -- so a translation that
/// forgets one is a compile error rather than a blank on screen.
macro_rules! trf {
    ($ja:literal, $en:literal $(,)?) => {
        if $crate::lang::is_en() { format!($en) } else { format!($ja) }
    };
    ($ja:literal, $en:literal, $($args:tt)+) => {
        if $crate::lang::is_en() { format!($en, $($args)+) } else { format!($ja, $($args)+) }
    };
}

/// What the machine is set to, as this process sees it.
///
/// The environment on Unix, which is where the desktop's language setting
/// ends up and what libc's own locale is read from. On Windows the webview's
/// `navigator.language` is the better answer and the frontend already has
/// it, so nothing is claimed here.
///
/// `LC_ALL` beats `LC_MESSAGES` beats `LANG`, which is the order the C
/// library reads them in; the "C" and "POSIX" locales name no language and
/// are passed over rather than answered with.
pub fn from_os() -> Option<String> {
    #[cfg(unix)]
    {
        for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            let Ok(value) = std::env::var(key) else { continue };
            let tag = value.trim();
            if tag.is_empty() || tag == "C" || tag == "POSIX" {
                continue;
            }
            return Some(tag.to_string());
        }
    }
    None
}
