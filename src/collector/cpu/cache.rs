use super::sysfs;
use crate::raw::cpu::{CpuCapabilities, RawIndexedAttributes};
use std::path::Path;
pub(super) fn collect(
    path: &Path,
    caps: &mut CpuCapabilities,
) -> Option<Vec<RawIndexedAttributes>> {
    sysfs::indexed(path, "index", caps)
}
