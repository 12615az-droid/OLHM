use super::sysfs;
use crate::raw::cpu::{CpuCapabilities, RawIdle};
use std::path::Path;
pub(super) fn collect(path: &Path, caps: &mut CpuCapabilities) -> Option<RawIdle> {
    let states = sysfs::indexed(path, "state", caps)?;
    let attributes = sysfs::attributes(path, caps);
    Some(RawIdle { attributes, states })
}
