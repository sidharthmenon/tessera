//! What the OS must not do to a live show: sleep the displays (a show run only from a remote
//! control gives the machine no keyboard or mouse input), nap or throttle the process (macOS App
//! Nap with no visible window, Windows EcoQoS for a process without focus: the controller would
//! see no events for 5 s and restart the engine), or run the render loop at background priority.

/// For the process lifetime: no display or idle sleep, no App Nap / EcoQoS. Call once, from the
/// main thread (on Windows the execution state belongs to the calling thread, which must live as
/// long as the process).
pub fn keep_awake() {
    #[cfg(target_os = "macos")]
    {
        use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
        let options = NSActivityOptions::NSActivityUserInteractive | NSActivityOptions::NSActivityIdleDisplaySleepDisabled;
        let reason = NSString::from_str("Tessera drives live outputs");
        // SAFETY: documented Foundation call; the returned token is kept for the process lifetime.
        let activity = unsafe { NSProcessInfo::processInfo().beginActivityWithOptions_reason(options, &reason) };
        std::mem::forget(activity); // ending it would let the displays sleep again
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Power::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, SetThreadExecutionState};
        use windows_sys::Win32::System::Threading::{
            GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION, PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetProcessInformation,
        };
        // SAFETY: plain Win32 calls on this process/thread with a correctly sized struct.
        unsafe {
            if SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED) == 0 {
                log!("power: SetThreadExecutionState failed; displays may sleep");
            }
            // ControlMask set, StateMask clear: throttling of these is explicitly off.
            let state = PROCESS_POWER_THROTTLING_STATE {
                Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
                ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED | PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION,
                StateMask: 0,
            };
            let size = size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32;
            if SetProcessInformation(GetCurrentProcess(), ProcessPowerThrottling, (&raw const state).cast(), size) == 0 {
                log!("power: EcoQoS opt-out failed");
            }
        }
    }
}

/// How urgent the calling thread's work is.
#[derive(Clone, Copy, Debug)]
pub enum Priority {
    /// The render loop: vsync deadlines.
    Render,
    /// Decode and live-input threads: feed the render loop a few frames ahead. Not lower: they
    /// briefly hold the audio tap lock the real-time callback waits on.
    Decode,
    /// Preview JPEGs, probes: nothing on air waits for them.
    Background,
}

/// Sets the calling thread's priority (macOS QoS class; Windows thread priority). Threads FFmpeg
/// starts from a decode thread inherit its QoS on macOS.
pub fn set_priority(priority: Priority) {
    #[cfg(target_os = "macos")]
    {
        use libc::qos_class_t::*;
        let class = match priority {
            Priority::Render => QOS_CLASS_USER_INTERACTIVE,
            Priority::Decode => QOS_CLASS_USER_INITIATED,
            Priority::Background => QOS_CLASS_UTILITY,
        };
        // SAFETY: sets the calling thread's own QoS class.
        if unsafe { libc::pthread_set_qos_class_self_np(class, 0) } != 0 {
            log!("thread priority {priority:?}: refused");
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{
            GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL, THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_NORMAL,
        };
        let level = match priority {
            Priority::Render => THREAD_PRIORITY_ABOVE_NORMAL,
            Priority::Decode => THREAD_PRIORITY_NORMAL,
            Priority::Background => THREAD_PRIORITY_BELOW_NORMAL,
        };
        // SAFETY: the pseudo-handle of the calling thread.
        if unsafe { SetThreadPriority(GetCurrentThread(), level) } == 0 {
            log!("thread priority {priority:?}: refused");
        }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = priority;
}
