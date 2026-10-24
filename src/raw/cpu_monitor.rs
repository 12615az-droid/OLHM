use crate::{error::InterfaceStatus, raw::cpu::RawProcStat};
use std::{collections::BTreeMap, path::PathBuf, time::Instant};

/// Dynamic readings only. Instant is monotonic and is not part of JSON export.
#[derive(Debug)]
pub struct RawCpuSample {
    pub at: Instant,
    pub stat: Result<RawProcStat, InterfaceStatus>,
    pub cpus: BTreeMap<u32, RawCpuSampleEntry>,
    pub temperatures: Vec<RawCpuTemperature>,
    pub power: Vec<RawCpuPower>,
}

#[derive(Debug)]
pub struct RawCpuSampleEntry {
    pub online: Option<bool>,
    pub frequency_khz: Result<String, InterfaceStatus>,
    pub throttle_counts: BTreeMap<String, Result<String, InterfaceStatus>>,
}

#[derive(Debug)]
pub struct RawCpuTemperature {
    pub path: PathBuf,
    pub device: String,
    pub driver: String,
    pub channel: String,
    pub label: Option<String>,
    pub input: Result<String, InterfaceStatus>,
    pub fault: Result<String, InterfaceStatus>,
    pub critical: Result<String, InterfaceStatus>,
    pub critical_alarm: Result<String, InterfaceStatus>,
}

/// Powercap data retains the zone's identity; Processing decides which domains
/// are CPU packages. Limits (constraint_*_power_limit_uw) are not measurements.
#[derive(Debug)]
pub struct RawCpuPower {
    pub path: PathBuf,
    pub zone: String,
    pub domain: String,
    pub at: Instant,
    pub power_uw: Result<String, InterfaceStatus>,
    pub energy_uj: Result<String, InterfaceStatus>,
    pub max_energy_range_uj: Result<String, InterfaceStatus>,
}
