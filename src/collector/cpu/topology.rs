use super::sysfs;
use crate::raw::cpu::{CpuCapabilities, RawAttributes};
use std::path::Path;
pub(super) fn collect(path: &Path, caps: &mut CpuCapabilities) -> Option<RawAttributes> {
    sysfs::attributes(path, caps)
}
