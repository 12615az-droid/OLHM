use crate::error::InterfaceStatus;
use std::{collections::BTreeMap, path::PathBuf, time::SystemTime};

/// Exact text read from files, including whitespace and kernel units.
pub type RawAttributes = BTreeMap<String, String>;
pub type CpuCapabilities = BTreeMap<PathBuf, InterfaceStatus>;

#[derive(Debug, serde::Serialize)]
pub struct RawCpuSnapshot {
    /// Collection start time; reads across files are not atomic.
    pub timestamp: SystemTime,
    pub cpuinfo: Option<RawCpuInfo>,
    pub proc_stat: Option<RawProcStat>,
    pub cpu_lists: BTreeMap<String, RawCpuList>,
    pub logical_cpus: Vec<RawLogicalCpu>,
    /// Standard hwmon devices are unclassified at Collection time. Processing
    /// can later associate temperature, voltage, power, and alarm channels.
    pub hardware_monitors: Option<Vec<RawSysfsDevice>>,
    pub thermal_zones: Option<Vec<RawSysfsDevice>>,
    pub cooling_devices: Option<Vec<RawSysfsDevice>>,
    pub powercap_zones: Option<Vec<RawSysfsDevice>>,
    pub vulnerabilities: Option<RawAttributes>,
    pub capabilities: CpuCapabilities,
}

#[derive(Debug, serde::Serialize)]
pub struct RawCpuInfo {
    pub text: String,
    /// Preserve record order, duplicate keys, and architecture-specific fields.
    pub records: Vec<Vec<(String, String)>>,
}

#[derive(Debug, serde::Serialize)]
pub struct RawProcStat {
    pub text: String,
    pub aggregate: Option<RawCpuCounters>,
    pub cpus: BTreeMap<u32, RawCpuCounters>,
    /// Non-CPU lines are preserved without interpretation.
    pub other: Vec<(String, String)>,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct RawCpuCounters {
    /// Kernel order: user, nice, system, idle, iowait, irq, softirq,
    /// steal, guest, guest_nice, followed by any future counters.
    /// Values are kernel clock ticks; no sums or percentages are computed.
    pub counters: Vec<u64>,
}

#[derive(Debug, serde::Serialize)]
pub struct RawCpuList {
    pub text: String,
    pub ids: Option<Vec<u32>>,
}

#[derive(Debug, serde::Serialize)]
pub struct RawLogicalCpu {
    pub id: u32,
    pub attributes: Option<RawAttributes>,
    /// Only an explicitly supplied per-CPU online file is parsed here.
    /// If absent, consult the global online list; do not assume true.
    pub online: Option<bool>,
    pub topology: Option<RawAttributes>,
    pub caches: Option<Vec<RawIndexedAttributes>>,
    pub frequency: Option<RawAttributes>,
    pub idle: Option<RawIdle>,
    /// Architecture-dependent raw thermal throttle counters when exported.
    pub thermal_throttle: Option<RawAttributes>,
}

#[derive(Debug, serde::Serialize)]
pub struct RawSysfsDevice {
    /// Directory entry name inside the corresponding sysfs class.
    pub name: String,
    /// Resolved device location helps later association without interpreting it.
    pub canonical_path: Option<PathBuf>,
    pub attributes: Option<RawAttributes>,
}

#[derive(Debug, serde::Serialize)]
pub struct RawIndexedAttributes {
    pub index: u32,
    pub attributes: Option<RawAttributes>,
}

#[derive(Debug, serde::Serialize)]
pub struct RawIdle {
    pub attributes: Option<RawAttributes>,
    pub states: Vec<RawIndexedAttributes>,
}
