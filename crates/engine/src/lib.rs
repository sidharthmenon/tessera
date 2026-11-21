/// `eprintln!` that never panics. The controller owns our stderr pipe; when it dies the pipe
/// breaks and `eprintln!` would panic, but the outputs must keep running.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        // The crate's unit tests: the harness captures the print macros (and nothing else).
        #[cfg(test)]
        eprintln!($($arg)*);
        #[cfg(not(test))]
        {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr().lock(), $($arg)*);
        }
    }};
}

pub mod app;
pub mod audio;
pub mod buses;
pub mod capture;
pub mod compositor;
pub mod frame_clock;
pub mod gpu;
pub mod live;
pub mod ndi;
pub mod net;
pub mod outputs;
pub mod player;
pub mod probe;
pub mod video_decode;
