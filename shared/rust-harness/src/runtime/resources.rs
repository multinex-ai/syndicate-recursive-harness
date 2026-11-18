//! Host resource sampling, admission, reclaim, watchdog, and process-group cleanup.

use crate::jobs::root;
use serde_json::{json, Value as Json};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{sleep, JoinHandle};
use std::time::{Duration, Instant};

const GIB: u64 = 1 << 30;

fn sysctl_u64(name: &std::ffi::CStr) -> Option<u64> {
    let mut v: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            &mut v as *mut u64 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    // Some keys are 32-bit ints.
    Some(if len == 4 { v & 0xffff_ffff } else { v })
}

/// psutil-compatible "available" on macOS: (free + inactive) pages.
pub fn available_bytes() -> Option<u64> {
    let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    #[allow(deprecated)]
    let rc = unsafe {
        libc::host_statistics64(
            libc::mach_host_self(),
            libc::HOST_VM_INFO64,
            &mut stats as *mut _ as *mut i32,
            &mut count,
        )
    };
    if rc != 0 {
        return None;
    }
    #[allow(deprecated)]
    let page = unsafe { libc::vm_page_size } as u64;
    Some((stats.free_count as u64 + stats.inactive_count as u64) * page)
}

fn swap_used_bytes() -> Option<u64> {
    let mut x: libc::xsw_usage = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::xsw_usage>();
    let rc = unsafe {
        libc::sysctlbyname(
            c"vm.swapusage".as_ptr(),
            &mut x as *mut _ as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0).then_some(x.xsu_used)
}

fn disk_free_bytes(path: &Path) -> Option<u64> {
    let c = std::ffi::CString::new(path.to_string_lossy().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statvfs(c.as_ptr(), &mut s) } == 0)
        .then(|| s.f_bavail as u64 * s.f_frsize as u64)
}

pub fn resident_bytes(pid: i32) -> u64 {
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskinfo>() as i32;
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    if n == size {
        info.pti_resident_size
    } else {
        0
    }
}

/// macOS physical footprint: what Activity Monitor calls Memory. Unlike RSS it
/// includes GPU (Metal/MLX) allocations, compressed and IOKit memory. 0 if unreadable.
pub fn footprint_bytes(pid: i32) -> u64 {
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V2,
            &mut info as *mut _ as *mut libc::rusage_info_t,
        )
    };
    if rc == 0 {
        info.ri_phys_footprint
    } else {
        0
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub time: f64,
    pub physical_bytes: u64,
    pub available_bytes: u64,
    pub pressure_level: u64,
    pub swap_used_bytes: u64,
    pub disk_free_bytes: u64,
    pub worker_rss_bytes: u64,
    /// Physical footprint of the same processes; counts GPU memory RSS misses.
    pub worker_footprint_bytes: u64,
}

impl Sample {
    pub fn take(server_pid: Option<i32>) -> Result<Sample, String> {
        let mut pids = vec![std::process::id() as i32];
        if let Some(p) = server_pid {
            pids.push(p);
            pids.extend(crate::supervisor::descendants(p).iter().map(|o| o.pid));
        }
        let rss = pids.iter().map(|p| resident_bytes(*p)).sum();
        let footprint = pids.iter().map(|p| footprint_bytes(*p)).sum();
        Ok(Sample {
            time: crate::jobs::now(),
            physical_bytes: sysctl_u64(c"hw.memsize").ok_or("hw.memsize unavailable")?,
            available_bytes: available_bytes().ok_or("host_statistics64 failed")?,
            pressure_level: sysctl_u64(c"kern.memorystatus_vm_pressure_level")
                .ok_or("pressure level unavailable")?,
            swap_used_bytes: swap_used_bytes().ok_or("vm.swapusage unavailable")?,
            disk_free_bytes: disk_free_bytes(&root()).ok_or("statvfs failed")?,
            worker_rss_bytes: rss,
            worker_footprint_bytes: footprint,
        })
    }

    pub fn to_json(&self) -> Json {
        json!({"time": self.time, "physical_bytes": self.physical_bytes, "available_bytes": self.available_bytes,
               "pressure_level": self.pressure_level, "swap_used_bytes": self.swap_used_bytes,
               "disk_free_bytes": self.disk_free_bytes, "worker_rss_bytes": self.worker_rss_bytes,
               "worker_footprint_bytes": self.worker_footprint_bytes})
    }
}

/// Same thresholds as media_policy.admission_reason.
pub fn admission_reason(s: &Sample) -> Option<&'static str> {
    if s.pressure_level != 1 {
        Some("macOS memory pressure is not normal")
    } else if s.available_bytes < 4 * GIB {
        Some("Less than 4 GiB available before model loading")
    } else if s.disk_free_bytes < 12 * GIB {
        Some("Less than 12 GiB disk reserve")
    } else {
        None
    }
}

pub fn max_swap_growth() -> u64 {
    if let Ok(v) = std::env::var("LOCAL_AGENTS_MAX_SWAP_GROWTH_GIB") {
        if let Ok(g) = v.parse::<f64>() {
            if g.is_finite() && g > 0.0 {
                return (g * GIB as f64) as u64;
            }
        }
    }
    if std::env::var("LOCAL_AGENTS_TOLERATE_WARNING_GIB").is_ok()
        || std::env::var("LOCAL_AGENTS_LOW_MEMORY").as_deref() == Ok("1")
    {
        return 3 * GIB;
    }
    3 * GIB / 4
}

/// Same thresholds as media_policy.runtime_reason.
pub fn runtime_reason(s: &Sample, initial: &Sample) -> Option<&'static str> {
    if s.pressure_level >= 4 {
        Some("Critical macOS memory pressure")
    } else if s.worker_rss_bytes.max(s.worker_footprint_bytes) > 8 * GIB {
        Some("Worker memory (resident or physical footprint, including GPU) exceeded 8 GiB")
    } else if s.swap_used_bytes.saturating_sub(initial.swap_used_bytes) > max_swap_growth() {
        Some("System swap grew beyond allowed limit during this job")
    } else if s.disk_free_bytes < 12 * GIB {
        Some("Disk reserve fell below 12 GiB")
    } else {
        None
    }
}

pub struct Watchdog {
    stop: Arc<AtomicBool>,
    reason: Arc<Mutex<Option<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl Watchdog {
    /// Why the watchdog stopped the process group, if it did.
    pub fn reason(&self) -> Option<String> {
        self.reason.lock().unwrap().clone()
    }

    /// Stop sampling and wait for the thread.
    pub fn finish(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Reopens reclaimed apps when dropped: after the server stops, or on a failed start.
/// Shared between start-up reclaim, the in-run watchdog and the final restore.
#[derive(Clone)]
pub struct RestoreGuard {
    apps: Arc<Mutex<Vec<String>>>,
    enabled: bool,
    receipt_path: PathBuf,
    receipt: Arc<Mutex<Json>>,
}

impl RestoreGuard {
    /// Reopens what reclaim quits, if the policy says so; receipt at `receipt_path`.
    pub fn new(policy: Option<&crate::reclaim::Policy>, receipt_path: PathBuf) -> RestoreGuard {
        RestoreGuard {
            apps: Default::default(),
            enabled: policy.map(|p| p.restore_after).unwrap_or(false),
            receipt_path,
            receipt: Arc::new(Mutex::new(Json::Null)),
        }
    }

    /// Before a heavy load: quit apps per policy until the start target is met.
    pub fn reclaim_before(&self, policy: &crate::reclaim::Policy) {
        let avail = || available_bytes().unwrap_or(0);
        let r = crate::reclaim::reclaim(
            policy,
            &avail,
            &pressure_normal,
            &crate::reclaim::app_running,
            &crate::reclaim::quit_app,
        );
        self.record("before_load", &r);
    }

    fn record(&self, phase: &str, r: &crate::reclaim::Receipt) {
        self.apps
            .lock()
            .unwrap()
            .extend(r.quit_apps.iter().cloned());
        let mut rec = self.receipt.lock().unwrap();
        if !rec.is_object() {
            *rec = json!({"method": "Normal Quit via Apple Events; apps can show save prompts; nothing is force-killed.", "phases": []});
        }
        let mut entry = r.to_json();
        entry["phase"] = json!(phase);
        entry["time"] = json!(crate::jobs::now());
        rec["phases"].as_array_mut().unwrap().push(entry);
        rec["quit_apps"] = json!(*self.apps.lock().unwrap());
        super::write_json(&self.receipt_path, &rec);
    }
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        // Clones live in the watchdog thread; only the last owner restores.
        if Arc::strong_count(&self.apps) > 1 {
            return;
        }
        let apps = self.apps.lock().unwrap().clone();
        if self.enabled && !apps.is_empty() {
            let mut rec = self.receipt.lock().unwrap();
            rec["restored"] = json!(crate::reclaim::restore(&apps));
            super::write_json(&self.receipt_path, &rec);
        }
    }
}

pub fn pressure_normal() -> bool {
    sysctl_u64(c"kern.memorystatus_vm_pressure_level") == Some(1)
}

pub fn save_resources(path: &Path, status: &str, reason: Option<&str>, samples: &[Json]) {
    super::write_json(
        path,
        &json!({
            "status": status, "reason": reason, "controller": "rust",
            "limits": {"min_start_available_gib": 4, "min_disk_free_gib": 12, "max_worker_rss_gib": 8,
                       "max_swap_growth_gib": 0.75, "sample_seconds": 2, "warning_grace_seconds": 6},
            "limitation": "Sampled safeguards; not an OS memory reservation or hard allocation ceiling. System swap includes other apps.",
            "samples": samples,
        }),
    );
}

/// Send SIGTERM to the server's process group, then SIGKILL after 8 s.
pub fn terminate_group(child: &mut Child) {
    if !matches!(child.try_wait(), Ok(None)) {
        return;
    }
    let pgid = child.id() as i32;
    unsafe { libc::killpg(pgid, libc::SIGTERM) };
    let deadline = Instant::now() + Duration::from_secs(8);
    while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
        sleep(Duration::from_millis(100));
    }
    if matches!(child.try_wait(), Ok(None)) {
        unsafe { libc::killpg(pgid, libc::SIGKILL) };
        let _ = child.wait();
    }
}

/// Opt-in: LOCAL_AGENTS_TOLERATE_WARNING_GIB=N lets a run continue through macOS *warning*
/// pressure while at least N GiB (never less than 1.5) is available. Use it on a Mac whose swap
/// is already full, where macOS reports warning with gigabytes free. Default: off.
pub fn warning_tolerance() -> Option<u64> {
    if let Ok(v) = std::env::var("LOCAL_AGENTS_TOLERATE_WARNING_GIB") {
        if let Ok(g) = v.parse::<f64>() {
            if g.is_finite() && g > 0.0 {
                return Some(((g.max(1.5)) * GIB as f64) as u64);
            }
        }
    }
    if let Ok(Some(policy)) = crate::reclaim::Policy::load(&root()) {
        if let Some(g) = policy.tolerate_warning_gib {
            if g.is_finite() && g > 0.0 {
                return Some(((g.max(1.5)) * GIB as f64) as u64);
            }
        }
    }
    None
}

/// Whether a sample counts as a memory warning. Critical pressure (4) always counts, and so does
/// less than 1.5 GiB available. Warning pressure (2) counts unless a tolerance is set and met.
pub fn memory_warning(pressure_level: u64, available: u64, tolerate_from: Option<u64>) -> bool {
    if available < 3 * GIB / 2 {
        return true;
    }
    match pressure_level {
        1 => false,
        2 => !tolerate_from.map(|t| available >= t).unwrap_or(false),
        _ => true,
    }
}

/// Samples every 2 s the process group led by `pid` (plus this process). On a memory
/// warning it first reclaims (quits the next eligible app) when a reclaim policy is set;
/// if the warning lasts 6 s with nothing left to reclaim, or a hard limit is breached,
/// it records why and SIGTERMs the group. Used by the model server and the media workers.
pub fn spawn_watchdog(
    pid: i32,
    initial: Sample,
    in_run: Option<(crate::reclaim::Policy, RestoreGuard)>,
    samples: Arc<Mutex<Vec<Json>>>,
    path: PathBuf,
) -> Watchdog {
    let stop = Arc::new(AtomicBool::new(false));
    let reason: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let (s2, r2) = (stop.clone(), reason.clone());
    let handle = std::thread::spawn(move || {
        let mut warning_since: Option<Instant> = None;
        let mut tried: Vec<String> = vec![];
        let mut exhausted = in_run.is_none();
        loop {
            for _ in 0..20 {
                if s2.load(Ordering::SeqCst) {
                    return;
                }
                sleep(Duration::from_millis(100));
            }
            let found = match Sample::take(Some(pid)) {
                Ok(s) => {
                    samples.lock().unwrap().push(s.to_json());
                    let warn =
                        memory_warning(s.pressure_level, s.available_bytes, warning_tolerance());
                    warning_since = if warn {
                        Some(warning_since.unwrap_or_else(Instant::now))
                    } else {
                        None
                    };
                    if warn && !exhausted {
                        if let Some((policy, guard)) = &in_run {
                            // Only chase "no warning" (normal pressure, >= 1.5 GiB), not the
                            // 4 GiB start target, so a loaded model doesn't empty the Mac.
                            let mut p = policy.clone();
                            p.target_available_bytes = 3 * GIB / 2;
                            p.never.extend(tried.iter().cloned());
                            let avail = || available_bytes().unwrap_or(0);
                            let r = crate::reclaim::reclaim(
                                &p,
                                &avail,
                                &pressure_normal,
                                &crate::reclaim::app_running,
                                &crate::reclaim::quit_app,
                            );
                            tried.extend(
                                r.entries
                                    .iter()
                                    .filter_map(|e| e["app"].as_str().map(String::from)),
                            );
                            if r.entries.is_empty() {
                                exhausted = true;
                            } else {
                                guard.record("during_run", &r);
                                warning_since = None; // fresh grace after freeing memory
                            }
                        }
                    }
                    runtime_reason(&s, &initial).map(String::from).or_else(|| {
                        warning_since
                            .filter(|t| t.elapsed() >= Duration::from_secs(6))
                            .map(|_| {
                                "Sustained memory warning or less than 1.5 GiB available"
                                    .to_string()
                            })
                    })
                }
                Err(e) => Some(format!("Resource monitoring failed: {e}")),
            };
            if let Some(r) = found {
                save_resources(
                    &path,
                    "resource_stopped",
                    Some(&r),
                    &samples.lock().unwrap(),
                );
                *r2.lock().unwrap() = Some(r);
                unsafe { libc::killpg(pid, libc::SIGTERM) };
                return;
            }
            save_resources(&path, "running", None, &samples.lock().unwrap());
        }
    });
    Watchdog {
        stop,
        reason,
        handle: Some(handle),
    }
}
