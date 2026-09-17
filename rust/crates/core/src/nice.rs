//! The passes that read a recording, put behind the rest of the machine.
//!
//! Reading a recording for its entry pictures decodes them on every core the
//! pool is allowed -- see [`crate::entrypool`] -- and building a proxy runs a
//! decoder and an encoder that each thread themselves. Either one is the
//! whole machine for as long as it runs, and the machine is not this
//! program's alone: somebody is compiling, or watching something, or waiting
//! on a browser, and a file added in the background is the one thing here
//! that nobody is sitting and watching.
//!
//! So a pass asks for what is left over rather than for its share. Nice 10
//! carries about a ninth of the weight of ordinary work, which on a busy
//! machine is the difference between a pass nobody notices and a pass
//! everybody does. **On an idle machine it is no difference at all**: a
//! thread with nothing competing for the core gets the core whatever its nice
//! value is, so the figures the pool was measured against still stand. The
//! pass is slower only where being faster would have been at somebody else's
//! expense.
//!
//! ## It is asked for once, by the thread, and never given back
//!
//! On Linux the nice value only goes one way for a program without
//! privileges: `RLIMIT_NICE` is 0 unless somebody has raised it, so 10 can be
//! reached and 0 cannot be reached again. That is why this is only ever
//! called at the top of a thread that exists for one pass and ends with it. A
//! thread out of a pool -- the one `spawn_blocking` hands out, say -- would
//! carry the nice value into whatever it was handed next, and what it is
//! handed next may well be the export.
//!
//! ## What it reaches
//!
//! On Linux the nice value belongs to the *thread* and not to the process,
//! and a thread started by a niced thread inherits it. So one call at the top
//! of a pass covers everything the pass goes on to start: the entry pool's
//! workers, the proxy's writing half, and libavcodec's own threads inside
//! either.
//!
//! On Windows nothing is inherited, so every thread this program makes for a
//! pass asks for itself, and the threads libavcodec makes are out of reach
//! and stay where the process is. For the entry pictures that costs nothing:
//! a pool worker decodes with one thread, so all of the decoding is on
//! threads of ours. The proxy's encode is the case that keeps its priority
//! there.

/// How far behind, as a Unix nice value. The traditional value for work that
/// is welcome to whatever is going spare and has no claim on anything else.
const BEHIND: i32 = 10;

/// What the environment has to say about it: `SMARTCUT_NICE=0` leaves every
/// pass where it is, and any other number is the nice value to take on Unix.
/// On Windows the number only says yes or no -- thread priorities there are a
/// handful of names rather than a scale.
fn asked() -> i32 {
    match std::env::var("SMARTCUT_NICE") {
        Ok(v) => v.trim().parse().unwrap_or(BEHIND),
        Err(_) => BEHIND,
    }
}

/// Put the calling thread behind the rest of the machine, for the rest of its
/// life.
///
/// Call it as the first thing a pass thread does. See the module note for why
/// it must not be called on a thread that will be handed other work.
pub fn behind() {
    let nice = asked();
    if nice == 0 {
        return;
    }
    set(nice);
}

#[cfg(unix)]
fn set(nice: i32) {
    use std::ffi::c_int;
    // `int setpriority(int which, id_t who, int value)`, with `id_t` the
    // 32-bit identifier it is everywhere this builds.
    //
    // `PRIO_PROCESS` with `who` 0 is "the caller", and on Linux the caller is
    // the calling *thread*: the kernel takes 0 as the current task, a nice
    // value belongs to a task, and every thread is a task. That is the whole
    // of why this can be asked for one pass at a time rather than for the
    // program. POSIX describes it as the process, and a system that read it
    // that way would put the window behind the machine as well -- so the
    // callers are the ones this was checked on, which is Linux and Windows.
    extern "C" {
        fn setpriority(which: c_int, who: u32, value: c_int) -> c_int;
    }
    const PRIO_PROCESS: c_int = 0;
    // Nothing is done about a refusal. The one way this fails is a value the
    // limit will not allow, and a pass that goes on at the priority it
    // already had is the pass exactly as it was before any of this.
    unsafe { setpriority(PRIO_PROCESS, 0, nice.clamp(0, 19)) };
}

#[cfg(windows)]
fn set(_nice: i32) {
    use std::ffi::{c_int, c_void};
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThread() -> *mut c_void;
        fn SetThreadPriority(thread: *mut c_void, priority: c_int) -> c_int;
    }
    // `THREAD_PRIORITY_LOWEST`: two steps below the process's ordinary
    // priority, and as far down as a thread goes without asking for
    // background mode as well.
    //
    // Background mode is deliberately not asked for. It takes the thread's
    // *I/O* priority down with it, and Windows throttles background I/O
    // whether or not anything else wants the disc -- which would make the
    // pass slower on an idle machine, the one thing this is not meant to do.
    const THREAD_PRIORITY_LOWEST: c_int = -2;
    // The handle is a pseudo-handle standing for "this thread". It is not
    // owned and there is nothing to close.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_LOWEST) };
}

#[cfg(not(any(unix, windows)))]
fn set(_nice: i32) {}
