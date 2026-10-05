//! Container probe fixture binary used only by the containment integration
//! tests.
//!
//! Modes:
//! - `copy-handle <decimal handle> <path>`: copy up to 256 bytes read through
//!   that handle number into `path`. A handle that was not inherited is not
//!   usable — the process may even hard-terminate — so a missing or empty
//!   output file is the denial, and the caller never depends on graceful
//!   error reporting for a stale handle number.
//! - `sleep <seconds>`: sleep, so a test can observe deadline behavior without
//!   tools the container denies.
//! - `sleep-write <seconds> <path> <text>`: sleep, then write `text` to `path`,
//!   so a test can detect a process that outlived the run.

use std::io::Read;
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("sleep") => sleep(args.get(2)),
        Some("sleep-write") => {
            sleep(args.get(2));
            let path = args.get(3).cloned().unwrap_or_default();
            let text = args.get(4).cloned().unwrap_or_default();
            std::fs::write(&path, text).expect("sleep-write target");
        }
        Some("copy-handle") => copy_handle(args.get(2), args.get(3)),
        _ => eprintln!("handle_probe: unknown mode"),
    }
}

fn sleep(seconds: Option<&String>) {
    let seconds: u64 = seconds.and_then(|value| value.parse().ok()).unwrap_or(1);
    std::thread::sleep(Duration::from_secs(seconds));
}

fn copy_handle(handle: Option<&String>, path: Option<&String>) {
    let raw: usize = handle.and_then(|value| value.parse().ok()).unwrap_or(0);
    let path = path.cloned().unwrap_or_default();
    let mut buffer = Vec::new();
    #[cfg(windows)]
    {
        use std::os::windows::io::FromRawHandle;
        // ManuallyDrop: the probe must not close a handle it merely guesses at.
        let file = std::mem::ManuallyDrop::new(unsafe {
            std::fs::File::from_raw_handle(raw as *mut core::ffi::c_void)
        });
        let _ = Read::take(&*file, 256).read_to_end(&mut buffer);
    }
    #[cfg(not(windows))]
    let _ = raw;
    std::fs::write(&path, &buffer).expect("copy-handle target");
}
