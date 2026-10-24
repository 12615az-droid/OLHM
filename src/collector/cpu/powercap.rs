use super::sysfs;
use crate::raw::cpu::{CpuCapabilities, RawSysfsDevice};
use std::path::Path;

pub(super) fn collect(path: &Path, caps: &mut CpuCapabilities) -> Option<Vec<RawSysfsDevice>> {
    sysfs::devices(path, None, caps)
}
