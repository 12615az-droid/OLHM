use super::sysfs;
use crate::raw::cpu::{CpuCapabilities, RawSysfsDevice};
use std::path::Path;

pub(super) struct RawThermalClasses {
    pub zones: Option<Vec<RawSysfsDevice>>,
    pub cooling_devices: Option<Vec<RawSysfsDevice>>,
}

pub(super) fn collect(path: &Path, caps: &mut CpuCapabilities) -> RawThermalClasses {
    RawThermalClasses {
        zones: sysfs::devices(path, Some("thermal_zone"), caps),
        cooling_devices: sysfs::devices(path, Some("cooling_device"), caps),
    }
}
