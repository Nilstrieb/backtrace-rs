//! A library for acquiring a backtrace at runtime
//!
//! This library is meant to supplement the `RUST_BACKTRACE=1` support of the
//! standard library by allowing an acquisition of a backtrace at runtime
//! programmatically. The backtraces generated by this library do not need to be
//! parsed, for example, and expose the functionality of multiple backend
//! implementations.
//!
//! # Usage
//!
//! First, add this to your Cargo.toml
//!
//! ```toml
//! [dependencies]
//! backtrace = "0.3"
//! ```
//!
//! Next:
//!
//! ```
//! fn main() {
//! # // Unsafe here so test passes on no_std.
//! # #[cfg(feature = "std")] {
//!     backtrace::trace(|frame| {
//!         let ip = frame.ip();
//!         let symbol_address = frame.symbol_address();
//!
//!         // Resolve this instruction pointer to a symbol name
//!         backtrace::resolve_frame(frame, |symbol| {
//!             if let Some(name) = symbol.name() {
//!                 // ...
//!             }
//!             if let Some(filename) = symbol.filename() {
//!                 // ...
//!             }
//!         });
//!
//!         true // keep going to the next frame
//!     });
//! }
//! # }
//! ```
//!
//! # Backtrace accuracy
//!
//! This crate implements best-effort attempts to get the native backtrace. This
//! is not always guaranteed to work, and some platforms don't return any
//! backtrace at all. If your application requires accurate backtraces then it's
//! recommended to closely evaluate this crate to see whether it's suitable
//! for your use case on your target platforms.
//!
//! Even on supported platforms, there's a number of reasons that backtraces may
//! be less-than-accurate, including but not limited to:
//!
//! * Unwind information may not be available. This crate primarily implements
//!   backtraces by unwinding the stack, but not all functions may have
//!   unwinding information (e.g. DWARF unwinding information).
//!
//! * Rust code may be compiled without unwinding information for some
//!   functions. This can also happen for Rust code compiled with
//!   `-Cpanic=abort`. You can remedy this, however, with
//!   `-Cforce-unwind-tables` as a compiler option.
//!
//! * Unwind information may be inaccurate or corrupt. In the worst case
//!   inaccurate unwind information can lead this library to segfault. In the
//!   best case inaccurate information will result in a truncated stack trace.
//!
//! * Backtraces may not report filenames/line numbers correctly due to missing
//!   or corrupt debug information. This won't lead to segfaults unlike corrupt
//!   unwinding information, but missing or malformed debug information will
//!   mean that filenames and line numbers will not be available. This may be
//!   because debug information wasn't generated by the compiler, or it's just
//!   missing on the filesystem.
//!
//! * Not all platforms are supported. For example there's no way to get a
//!   backtrace on WebAssembly at the moment.
//!
//! * Crate features may be disabled. Currently this crate supports using Gimli
//!   libbacktrace on non-Windows platforms for reading debuginfo for
//!   backtraces. If both crate features are disabled, however, then these
//!   platforms will generate a backtrace but be unable to generate symbols for
//!   it.
//!
//! In most standard workflows for most standard platforms you generally don't
//! need to worry about these caveats. We'll try to fix ones where we can over
//! time, but otherwise it's important to be aware of the limitations of
//! unwinding-based backtraces!

#![deny(missing_docs)]
#![no_std]
#![cfg_attr(
    all(feature = "std", target_env = "sgx", target_vendor = "fortanix"),
    feature(sgx_platform)
)]
#![warn(rust_2018_idioms)]
// When we're building as part of libstd, silence all warnings since they're
// irrelevant as this crate is developed out-of-tree.
#![cfg_attr(backtrace_in_libstd, allow(warnings))]
#![cfg_attr(not(feature = "std"), allow(dead_code))]
// We know this is deprecated, it's only here for back-compat reasons.
#![cfg_attr(feature = "rustc-serialize", allow(deprecated))]

#[cfg(feature = "std")]
#[macro_use]
extern crate std;

// This is only used for gimli right now, which is only used on some platforms, and miri
// so don't worry if it's unused in other configurations.
#[allow(unused_extern_crates)]
extern crate alloc;

pub use self::backtrace::{trace_unsynchronized, Frame};
mod backtrace;

pub use self::symbolize::resolve_frame_unsynchronized;
pub use self::symbolize::{resolve_unsynchronized, Symbol, SymbolName};
mod symbolize;

pub use self::types::BytesOrWideString;
mod types;

#[cfg(feature = "std")]
pub use self::symbolize::clear_symbol_cache;

mod print;
pub use print::{BacktraceFmt, BacktraceFrameFmt, PrintFmt};

cfg_if::cfg_if! {
    if #[cfg(feature = "std")] {
        pub use self::backtrace::trace;
        pub use self::symbolize::{resolve, resolve_frame};
        pub use self::capture::{Backtrace, BacktraceFrame, BacktraceSymbol};
        mod capture;
    }
}

cfg_if::cfg_if! {
    if #[cfg(all(target_env = "sgx", target_vendor = "fortanix", not(feature = "std")))] {
        pub use self::backtrace::set_image_base;
    }
}

#[allow(dead_code)]
struct Bomb {
    enabled: bool,
}

#[allow(dead_code)]
impl Drop for Bomb {
    fn drop(&mut self) {
        if self.enabled {
            panic!("cannot panic during the backtrace function");
        }
    }
}

#[used]
pub static CRITICALLY_IMPORTANT: [u8; 1024*1024] = [0; 1024*1024];

#[allow(dead_code)]
#[cfg(feature = "std")]
mod lock {
    use std::boxed::Box;
    use std::cell::Cell;
    use std::ptr;
    use std::sync::{Mutex, MutexGuard, Once};

    pub struct LockGuard(Option<MutexGuard<'static, ()>>);

    static mut LOCK: *mut Mutex<()> = ptr::null_mut();
    static INIT: Once = Once::new();
    thread_local!(static LOCK_HELD: Cell<bool> = Cell::new(false));

    impl Drop for LockGuard {
        fn drop(&mut self) {
            if self.0.is_some() {
                LOCK_HELD.with(|slot| {
                    assert!(slot.get());
                    slot.set(false);
                });
            }
        }
    }

    pub fn lock() -> LockGuard {
        if LOCK_HELD.with(|l| l.get()) {
            return LockGuard(None);
        }
        LOCK_HELD.with(|s| s.set(true));
        unsafe {
            INIT.call_once(|| {
                LOCK = Box::into_raw(Box::new(Mutex::new(())));
            });
            LockGuard(Some((*LOCK).lock().unwrap()))
        }
    }
}

#[cfg(all(
    windows,
    any(
        target_env = "msvc",
        all(target_env = "gnu", any(target_arch = "x86", target_arch = "arm"))
    ),
    not(target_vendor = "uwp")
))]
mod dbghelp;
#[cfg(windows)]
mod windows;
