//! Processed, interface-independent devices and measurements.
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub enum MetricValue {
    Available(f64),
    Waiting,
    NotAvailable,
    NotSupported,
    PermissionDenied,
    ReadError,
    InvalidData,
    CounterReset,
    NoProgress,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    Percent,
    Megahertz,
    Celsius,
    Watt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Cpu,
    LogicalCpu,
    TemperatureSensor,
    PowerDomain,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Property {
    pub id: String,
    pub label: String,
    pub value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Device {
    /// Stable within a session; children refer to this ID, never list order.
    pub id: String,
    pub parent_id: Option<String>,
    pub kind: DeviceKind,
    pub name: String,
    pub properties: Vec<Property>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Metric {
    pub id: String,
    pub label: String,
    pub unit: Unit,
    pub value: MetricValue,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DeviceReading {
    pub device: Device,
    pub metrics: Vec<Metric>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MonitorSnapshot {
    pub interval: Option<Duration>,
    pub devices: Vec<DeviceReading>,
}
