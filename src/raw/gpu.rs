//! Unnormalised Linux/NVML readings, with explicit source and failure per field.
use std::{collections::BTreeMap, time::Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuError {
    NotSupported,
    NotAvailable,
    PermissionDenied,
    ReadError(String),
    InvalidData,
}
impl From<std::io::Error> for GpuError {
    fn from(error: std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::NotFound => Self::NotAvailable,
            std::io::ErrorKind::PermissionDenied => Self::PermissionDenied,
            _ => Self::ReadError(error.to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuUnit {
    PcieSpeed,
    PcieWidth,
    Byte,
    Percent,
    Celsius,
    MilliCelsius,
    Watt,
    MilliWatt,
    MicroWatt,
    MicroJoule,
    Megahertz,
    Hertz,
    Rpm,
    Boolean,
    Bits,
    Pwm,
}

#[derive(Debug, Clone)]
pub struct RawGpuValue {
    pub value: Result<String, GpuError>,
    pub unit: GpuUnit,
    pub source: String,
}

#[derive(Debug, Clone)]
pub struct RawEngineCounter {
    pub busy: u64,
    pub total: u64,
}

#[derive(Debug, Clone)]
pub struct RawGpu {
    pub identity: String,
    pub metadata: BTreeMap<String, String>,
    pub values: BTreeMap<String, RawGpuValue>,
    pub engines: BTreeMap<String, Result<RawEngineCounter, GpuError>>,
    pub amd_metrics: Result<Vec<u8>, GpuError>,
    pub energy_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct RawGpuSample {
    pub at: Instant,
    pub devices: Result<Vec<RawGpu>, GpuError>,
}
