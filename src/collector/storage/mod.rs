//! Read-only block topology, mounted filesystems and optional SMART adapters.
use crate::{error::InterfaceStatus, raw::storage::*};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
mod mounts;
#[cfg(test)]
mod tests;
mod tools;

fn status(error: io::Error) -> InterfaceStatus {
    match error.kind() {
        io::ErrorKind::NotFound => InterfaceStatus::NotAvailable,
        io::ErrorKind::PermissionDenied => InterfaceStatus::PermissionDenied(error.to_string()),
        _ => InterfaceStatus::ReadError(error.to_string()),
    }
}

fn read(path: &Path) -> Text {
    fs::read_to_string(path).map_err(status)
}

fn directories(path: &Path) -> Result<Vec<PathBuf>, InterfaceStatus> {
    let mut paths = fs::read_dir(path)
        .map_err(status)?
        .map(|entry| entry.map(|entry| entry.path()).map_err(status))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    Ok(paths)
}

struct CachedBlock {
    raw: RawBlock,
    physical: bool,
}

pub struct StorageCollector {
    proc_root: PathBuf,
    sys_root: PathBuf,
    tools: bool,
    blocks: BTreeMap<String, CachedBlock>,
    metadata: BTreeMap<String, BTreeMap<String, String>>,
    metadata_status: Result<(), InterfaceStatus>,
    metadata_at: Option<Instant>,
    sensors: Vec<(PathBuf, PathBuf, String)>,
    sensors_at: Option<Instant>,
    device_names: BTreeSet<String>,
}

impl Default for StorageCollector {
    fn default() -> Self {
        let mut collector = Self::with_roots("/proc", "/sys");
        collector.tools = true;
        collector
    }
}

impl StorageCollector {
    /// Custom roots never run host tools or probe real devices in fixture tests.
    pub fn with_roots(proc_root: impl Into<PathBuf>, sys_root: impl Into<PathBuf>) -> Self {
        Self {
            proc_root: proc_root.into(),
            sys_root: sys_root.into(),
            tools: false,
            blocks: BTreeMap::new(),
            metadata: BTreeMap::new(),
            metadata_status: Err(InterfaceStatus::NotAvailable),
            metadata_at: None,
            sensors: Vec::new(),
            sensors_at: None,
            device_names: BTreeSet::new(),
        }
    }

    fn sensors(&mut self, now: Instant) {
        if self
            .sensors_at
            .is_some_and(|at| now.duration_since(at) < Duration::from_secs(5))
        {
            return;
        }
        self.sensors_at = Some(now);
        self.sensors.clear();
        for path in directories(&self.sys_root.join("class/hwmon")).unwrap_or_default() {
            let Ok(name) = read(&path.join("name")) else {
                continue;
            };
            if !matches!(name.trim(), "nvme" | "drivetemp") {
                continue;
            }
            let Ok(device) = fs::canonicalize(path.join("device")) else {
                continue;
            };
            self.sensors.push((path, device, name.trim().into()));
        }
    }

    fn sample_blocks(&mut self, now: Instant) -> Result<Vec<RawBlock>, InterfaceStatus> {
        let paths = directories(&self.sys_root.join("class/block"))?;
        let mut names = Vec::new();
        let device_names: BTreeSet<_> = paths
            .iter()
            .filter_map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect();
        let new_device = device_names != self.device_names;
        self.device_names = device_names;
        if self.tools
            && (new_device
                || self
                    .metadata_at
                    .is_none_or(|at| now.duration_since(at) >= Duration::from_secs(60)))
        {
            self.metadata_at = Some(now);
            match tools::lsblk() {
                Ok(metadata) => {
                    self.metadata = metadata;
                    self.metadata_status = Ok(());
                }
                Err(error) => {
                    self.metadata.clear();
                    self.metadata_status = Err(error);
                }
            }
        }
        self.sensors(now);
        let mut result = Vec::new();
        for path in paths {
            let Some(name) = path.file_name().and_then(|s| s.to_str()).map(str::to_owned) else {
                continue;
            };
            // RAM-backed swap is already in the RAM page; unused loop nodes are noise.
            if name
                .strip_prefix("zram")
                .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
            {
                continue;
            }
            let size = read(&path.join("size"));
            if name.starts_with("loop") && size.as_ref().is_ok_and(|n| n.trim() == "0") {
                continue;
            }
            names.push(name.clone());
            let Ok(canonical) = fs::canonicalize(&path) else {
                continue;
            };
            let dev = read(&path.join("dev"));
            let partition = path.join("partition").exists();
            let parent = partition
                .then(|| {
                    canonical
                        .parent()
                        .and_then(|p| p.file_name())
                        .map(|s| s.to_string_lossy().into_owned())
                })
                .flatten();
            let sequence_path = parent
                .as_ref()
                .map(|name| self.sys_root.join("class/block").join(name).join("diskseq"))
                .unwrap_or_else(|| path.join("diskseq"));
            let sequence = read(&sequence_path);
            let identity = format!(
                "{}|{}|{}",
                canonical.display(),
                dev.as_deref().unwrap_or("?").trim(),
                sequence.as_deref().unwrap_or("?").trim()
            );
            if self
                .blocks
                .get(&name)
                .is_none_or(|old| old.raw.identity != identity)
            {
                if self.blocks.contains_key(&name) {
                    self.metadata.remove(&name);
                    self.metadata_at = None;
                }
                let attrs = [
                    "device/model",
                    "device/vendor",
                    "device/rev",
                    "device/serial",
                    "device/firmware_rev",
                    "device/transport",
                    "queue/rotational",
                    "removable",
                    "queue/logical_block_size",
                    "queue/physical_block_size",
                ]
                .into_iter()
                .map(|key| (key.into(), read(&path.join(key))))
                .collect();
                let physical =
                    !partition && !canonical.starts_with(self.sys_root.join("devices/virtual"));
                self.blocks.insert(
                    name.clone(),
                    CachedBlock {
                        physical,
                        raw: RawBlock {
                            name: name.clone(),
                            path: canonical.clone(),
                            identity,
                            parent,
                            partition,
                            attrs,
                            metadata: BTreeMap::new(),
                            stat_at: now,
                            stat: Err(InterfaceStatus::NotAvailable),
                            temperature: Err(InterfaceStatus::NotAvailable),
                            temperature_source: None,
                            smart: Err(InterfaceStatus::NotAvailable),
                            smart_at: now,
                        },
                    },
                );
                if self.tools && physical {
                    let cached = self.blocks.get_mut(&name).unwrap();
                    cached.raw.smart = tools::smart(&name, &canonical);
                    cached.raw.smart_at = Instant::now();
                }
            }
            let cached = self.blocks.get_mut(&name).unwrap();
            if self.tools
                && cached.physical
                && now
                    .checked_duration_since(cached.raw.smart_at)
                    .is_some_and(|age| age >= Duration::from_secs(60))
            {
                cached.raw.smart = tools::smart(&name, &canonical);
                cached.raw.smart_at = Instant::now();
            }
            cached.raw.metadata = self.metadata.get(&name).cloned().unwrap_or_default();
            cached.raw.attrs.insert("dev".into(), dev);
            cached.raw.attrs.insert("size".into(), size);
            cached.raw.attrs.insert("ro".into(), read(&path.join("ro")));
            cached
                .raw
                .attrs
                .insert("start".into(), read(&path.join("start")));
            cached
                .raw
                .attrs
                .insert("partition".into(), read(&path.join("partition")));
            cached
                .raw
                .attrs
                .insert("device/state".into(), read(&path.join("device/state")));
            cached.raw.attrs.insert(
                "slaves".into(),
                directories(&path.join("slaves")).map(|paths| {
                    paths
                        .iter()
                        .filter_map(|path| path.file_name())
                        .map(|name| name.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(", ")
                }),
            );
            cached.raw.stat_at = Instant::now();
            cached.raw.stat = read(&path.join("stat"));
            cached.raw.temperature = Err(InterfaceStatus::NotAvailable);
            cached.raw.temperature_source = None;
            if !partition {
                let device =
                    fs::canonicalize(path.join("device")).unwrap_or_else(|_| canonical.clone());
                let sensors: Vec<_> = self
                    .sensors
                    .iter()
                    .filter(|(_, sensor_device, _)| {
                        &device == sensor_device || canonical.starts_with(sensor_device)
                    })
                    .collect();
                if sensors.len() == 1 {
                    let (sensor, expected, driver) = sensors[0];
                    if fs::canonicalize(sensor.join("device")).ok().as_ref() == Some(expected) {
                        cached.raw.temperature = read(&sensor.join("temp1_input"));
                        cached.raw.temperature_source = Some(format!(
                            "{driver}; {}{}",
                            sensor.display(),
                            if driver == "nvme" {
                                "; контроллер NVMe"
                            } else {
                                ""
                            }
                        ));
                    }
                }
            }
            result.push(cached.raw.clone());
        }
        self.blocks.retain(|name, _| names.contains(name));
        Ok(result)
    }

    pub fn sample(&mut self) -> RawStorageSample {
        let at = Instant::now();
        let blocks = self.sample_blocks(at);
        let mounts = mounts::collect(&self.proc_root.join("self/mountinfo"));
        RawStorageSample {
            at,
            blocks,
            mounts,
            metadata_status: self.metadata_status.clone(),
        }
    }
}
