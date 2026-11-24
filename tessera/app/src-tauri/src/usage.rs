//! CPU and RAM of the engine and this app, sampled once a second for the status bar
//! (event `usage`).

use std::time::Duration;

use sysinfo::{CpuRefreshKind, MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tauri::{AppHandle, Emitter, Manager};

use crate::engine_client::Engine;

/// One process: `cpu` as a share of the whole machine (0-100), `ram` resident bytes.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessUsage {
    pub cpu: f32,
    pub ram: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// `None` while no engine process is running.
    pub engine: Option<ProcessUsage>,
    pub app: ProcessUsage,
    pub machine_cpu: f32,
    pub ram_used: u64,
    pub ram_total: u64,
}

/// A process's `cpu_usage` is per core (a busy 4-thread process reads 400); the status bar
/// wants a share of the machine.
fn share_of_machine(per_core: f32, cores: usize) -> f32 {
    let share = if cores == 0 { per_core } else { per_core / cores as f32 };
    share.clamp(0.0, 100.0)
}

struct Sampler {
    sys: System,
    own: Pid,
    cores: usize,
    /// The engine pid of the last sample, so a changed or gone one is listed once more and
    /// sysinfo drops it (on Windows that releases its process handle).
    prev: Option<Pid>,
}

impl Sampler {
    fn new() -> Sampler {
        let mut sys = System::new().expect("sysinfo is supported on this platform");
        let own = sysinfo::get_current_pid().expect("own pid");
        // CPU figures are deltas: prime the machine and this process so the first sample,
        // one interval later, is real. (The engine pid is primed by its first sample.)
        sys.refresh_cpu_list(CpuRefreshKind::nothing().with_usage());
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[own]), true, ProcessRefreshKind::nothing().with_cpu());
        let cores = sys.cpus().len();
        Sampler { sys, own, cores, prev: None }
    }

    fn sample(&mut self, engine: Option<u32>) -> Usage {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory_specifics(MemoryRefreshKind::nothing().with_ram());
        let engine = engine.map(Pid::from_u32);
        let stale = self.prev.filter(|p| Some(*p) != engine);
        self.prev = engine;
        let pids: Vec<Pid> = std::iter::once(self.own).chain(engine).chain(stale).collect();
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::Some(&pids),
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        let of = |pid: Pid| {
            self.sys
                .process(pid)
                .map(|p| ProcessUsage { cpu: share_of_machine(p.cpu_usage(), self.cores), ram: p.memory() })
        };
        Usage {
            engine: engine.and_then(of),
            app: of(self.own).unwrap_or(ProcessUsage { cpu: 0.0, ram: 0 }),
            machine_cpu: self.sys.global_cpu_usage().clamp(0.0, 100.0),
            // What the OS calls used (macOS: Activity Monitor's Memory Used; Windows: total
            // minus available). Not total minus available_memory, which on macOS leaves out
            // active memory.
            ram_used: self.sys.used_memory().min(self.sys.total_memory()),
            ram_total: self.sys.total_memory(),
        }
    }
}

/// Sample every second on its own thread for the life of the app.
pub fn spawn(app: AppHandle) {
    let _ = std::thread::Builder::new().name("usage".into()).spawn(move || {
        let mut sampler = Sampler::new();
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let usage = sampler.sample(app.state::<Engine>().pid());
            let _ = app.emit("usage", usage);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_of_machine_is_clamped() {
        assert_eq!(share_of_machine(200.0, 8), 25.0); // 2 cores busy of 8
        assert_eq!(share_of_machine(900.0, 8), 100.0);
        assert_eq!(share_of_machine(-1.0, 8), 0.0);
        assert_eq!(share_of_machine(50.0, 0), 50.0); // core count unknown: as is
    }

    #[test]
    fn own_process_is_sampled_and_gone_pid_is_none() {
        let mut s = Sampler::new();
        let u = s.sample(Some(1_073_741_820)); // no such process (positive as an i32 pid, a multiple of 4 for Windows)
        assert_eq!(u.engine, None, "gone pid");
        assert!(u.app.ram > 0, "own process has resident memory");
        assert!(u.ram_total > 0 && u.ram_used <= u.ram_total);
    }
}
