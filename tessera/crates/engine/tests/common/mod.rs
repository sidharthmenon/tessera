//! Shared by the GPU test files.

use std::cell::RefCell;
use std::sync::{Mutex, MutexGuard, PoisonError};

use tessera_engine::gpu::Gpu;

/// A device for the calling test, which from here on has the GPU to itself until its thread ends:
/// each device reserves its own blocks of GPU memory (hundreds of MB once it draws or uploads), and
/// a dozen side by side ran a 1 GB integrated GPU out of memory. Not one device for all: the engine
/// records and submits from one thread, and tests running at once would interleave their submits.
pub fn gpu() -> Gpu {
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());
    thread_local! {
        static TURN: RefCell<Option<MutexGuard<'static, ()>>> = const { RefCell::new(None) };
    }
    TURN.with_borrow_mut(|turn| {
        if turn.is_none() {
            *turn = Some(ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner));
        }
    });
    Gpu::new_headless().expect("no GPU adapter: these tests need a real GPU (headless Metal on macOS)")
}
