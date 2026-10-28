//! One unmodified /proc/meminfo reading with its availability and sample time.
use crate::error::InterfaceStatus;
use std::{collections::BTreeMap, path::PathBuf, time::Instant};

#[derive(Debug)]
pub struct RawMemorySample {
    pub at: Instant,
    pub meminfo: Result<String, InterfaceStatus>,
}

#[derive(Debug)]
pub struct RawMemoryBinary {
    pub path: PathBuf,
    pub data: Result<Vec<u8>, InterfaceStatus>,
}

#[derive(Debug)]
pub struct RawMemoryInventory {
    pub dmi: Result<Vec<RawMemoryBinary>, InterfaceStatus>,
    pub spd: Result<Vec<RawMemoryBinary>, InterfaceStatus>,
}

#[derive(Debug)]
pub struct RawMemoryTemperature {
    pub path: PathBuf,
    pub device: PathBuf,
    pub driver: String,
    pub input: Result<String, InterfaceStatus>,
    pub fault: Result<String, InterfaceStatus>,
    pub enabled: Result<String, InterfaceStatus>,
}

#[derive(Debug)]
pub struct RawMemoryDetails {
    pub swaps: Result<String, InterfaceStatus>,
    /// Keyed by the decoded /proc/swaps filename, never by listing order.
    pub zram: BTreeMap<String, BTreeMap<String, Result<String, InterfaceStatus>>>,
    pub temperatures: Vec<RawMemoryTemperature>,
}
