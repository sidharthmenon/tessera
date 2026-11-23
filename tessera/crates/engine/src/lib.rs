/// A log line that never panics and never blocks (see `logger`). The controller owns our stderr
/// pipe; when it dies the pipe breaks (`eprintln!` would panic), and when it stalls the pipe fills
/// (a plain write would stop the render loop), but the outputs must keep running.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {{
        // The crate's unit tests: the harness captures the print macros (and nothing else).
        #[cfg(test)]
        eprintln!($($arg)*);
        #[cfg(not(test))]
        $crate::logger::write(format_args!($($arg)*));
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
pub mod logger;
pub mod ndi;
pub mod net;
pub mod os;
pub mod outputs;
pub mod player;
pub mod probe;
pub mod staging;
pub mod video_decode;
pub mod zero_copy;
