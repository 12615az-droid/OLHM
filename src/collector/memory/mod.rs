//! RAM and swap collection through the standard Linux proc interface.
use crate::{
    collector::Collector,
    error::{CollectError, InterfaceStatus},
    raw::memory::RawMemorySample,
};
use std::{fs, io, path::PathBuf, time::Instant};
mod details;
pub(crate) use details::unescape_path;

pub struct MemoryCollector {
    proc_root: PathBuf,
    sys_root: PathBuf,
    temperatures: Vec<(PathBuf, PathBuf, String)>,
}

impl Default for MemoryCollector {
    fn default() -> Self {
        Self::with_roots("/proc", "/sys")
    }
}

impl MemoryCollector {
    pub fn with_root(proc_root: impl Into<PathBuf>) -> Self {
        let proc_root = proc_root.into();
        let sys_root = proc_root
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .join("sys");
        Self::with_roots(proc_root, sys_root)
    }

    pub fn with_roots(proc_root: impl Into<PathBuf>, sys_root: impl Into<PathBuf>) -> Self {
        Self {
            proc_root: proc_root.into(),
            sys_root: sys_root.into(),
            temperatures: Vec::new(),
        }
    }
    pub fn sample(&self) -> RawMemorySample {
        let at = Instant::now();
        let meminfo =
            fs::read_to_string(self.proc_root.join("meminfo")).map_err(|error| {
                match error.kind() {
                    io::ErrorKind::NotFound => InterfaceStatus::NotAvailable,
                    io::ErrorKind::PermissionDenied => {
                        InterfaceStatus::PermissionDenied(error.to_string())
                    }
                    _ => InterfaceStatus::ReadError(error.to_string()),
                }
            });
        // Keep interface failures in the sample so a session can recover.
        RawMemorySample { at, meminfo }
    }
}

impl Collector for MemoryCollector {
    type Output = RawMemorySample;

    fn collect(&self) -> Result<Self::Output, CollectError> {
        Ok(self.sample())
    }
}
