//! PCI/DRM discovery and vendor adapters. All access is read-only.
use crate::raw::gpu::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
mod drm;
mod nvml;
mod perf;
mod sysfs;
#[cfg(test)]
mod tests;

pub(super) fn text(path: &Path) -> Result<String, GpuError> {
    fs::read_to_string(path)
        .map(|s| s.trim().to_owned())
        .map_err(Into::into)
}
pub(super) fn entries(path: &Path) -> Result<Vec<PathBuf>, GpuError> {
    let mut paths = fs::read_dir(path)
        .map_err(GpuError::from)?
        .map(|e| e.map(|e| e.path()).map_err(GpuError::from))
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    Ok(paths)
}
fn numbered(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()))
}
pub(super) fn binary(path: &Path) -> Result<Vec<u8>, GpuError> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(GpuError::from)?
        .take(16385)
        .read_to_end(&mut bytes)
        .map_err(GpuError::from)?;
    if bytes.len() > 16384 {
        return Err(GpuError::InvalidData);
    }
    Ok(bytes)
}
pub(super) fn put(
    raw: &mut RawGpu,
    id: &str,
    unit: GpuUnit,
    value: Result<String, GpuError>,
    source: &str,
) {
    raw.values.insert(
        id.into(),
        RawGpuValue {
            value,
            unit,
            source: source.into(),
        },
    );
}

struct CachedGpu {
    path: PathBuf,
    card: Option<PathBuf>,
    render: Option<PathBuf>,
    sources: sysfs::Sources,
    raw: RawGpu,
    pmu: Option<perf::GpuPmu>,
    probe_at: Instant,
}

pub struct GpuCollector {
    sys_root: PathBuf,
    dev_root: PathBuf,
    native: bool,
    cached: BTreeMap<String, CachedGpu>,
    nvml: Option<nvml::Nvml>,
    nvml_attempt: Option<Instant>,
    nvml_error: Option<GpuError>,
    pci_ids: String,
}

impl Default for GpuCollector {
    fn default() -> Self {
        let mut collector = Self::with_roots("/sys", "/dev");
        collector.native = true;
        collector.pci_ids = [
            "/usr/share/hwdata/pci.ids",
            "/usr/share/misc/pci.ids",
            "/run/current-system/sw/share/hwdata/pci.ids",
        ]
        .into_iter()
        .find_map(|path| fs::read_to_string(path).ok())
        .unwrap_or_default();
        collector
    }
}

impl GpuCollector {
    /// Fixture roots never load host vendor libraries or open real DRM/perf devices.
    pub fn with_roots(sys_root: impl Into<PathBuf>, dev_root: impl Into<PathBuf>) -> Self {
        Self {
            sys_root: sys_root.into(),
            dev_root: dev_root.into(),
            native: false,
            cached: BTreeMap::new(),
            nvml: None,
            nvml_attempt: None,
            nvml_error: None,
            pci_ids: String::new(),
        }
    }

    fn discover(&self) -> Result<Vec<PathBuf>, GpuError> {
        let pci = entries(&self.sys_root.join("bus/pci/devices"));
        let cards = entries(&self.sys_root.join("class/drm"));
        if let (Err(error), Err(_)) = (&pci, &cards) {
            return Err(error.clone());
        }
        let mut paths = BTreeSet::new();
        for path in pci.into_iter().flatten() {
            let class = text(&path.join("class"))
                .ok()
                .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
            if class.is_some_and(|c| c >> 16 == 3)
                && let Ok(path) = fs::canonicalize(path)
            {
                paths.insert(path);
            }
        }
        for path in cards.into_iter().flatten() {
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| numbered(n, "card"))
                && let Ok(path) = fs::canonicalize(path.join("device"))
            {
                paths.insert(path);
            }
        }
        Ok(paths.into_iter().collect())
    }

    fn create(&self, path: PathBuf, identity: String, now: Instant) -> CachedGpu {
        let mut metadata = BTreeMap::new();
        metadata.insert("path".into(), path.display().to_string());
        let bdf = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        metadata.insert("pci".into(), bdf.clone());
        for (field, file) in [
            ("vendor_id", "vendor"),
            ("device_id", "device"),
            ("revision", "revision"),
            ("model", "product_name"),
        ] {
            if let Ok(s) = text(&path.join(file))
                && !s.is_empty()
            {
                metadata.insert(field.into(), s);
            }
        }
        let driver = fs::canonicalize(path.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()));
        if let Some(driver) = driver {
            metadata.insert("driver".into(), driver.clone());
            let version = text(&self.sys_root.join("module").join(&driver).join("version"))
                .or_else(|_| text(&self.sys_root.join("kernel/osrelease")))
                .or_else(|_| {
                    if self.native {
                        text(Path::new("/proc/sys/kernel/osrelease"))
                    } else {
                        Err(GpuError::NotAvailable)
                    }
                });
            if let Ok(version) = version {
                metadata.insert("driver_version".into(), format!("{version} (модуль/ядро)"));
            }
        }
        if !metadata.contains_key("model") {
            if self.native
                && valid_bdf(&bdf)
                && let Ok(data) = text(&Path::new("/run/udev/data").join(format!("+pci:{bdf}")))
                && let Some(model) = data
                    .lines()
                    .find_map(|s| s.strip_prefix("E:ID_MODEL_FROM_DATABASE="))
            {
                metadata.insert("model".into(), model.into());
                metadata.insert(
                    "model_source".into(),
                    "udev PCI database (может охватывать семейство моделей)".into(),
                );
            }
            if !metadata.contains_key("model")
                && let Some(model) = pci_model(
                    &self.pci_ids,
                    metadata.get("vendor_id"),
                    metadata.get("device_id"),
                )
            {
                metadata.insert("model".into(), model);
                metadata.insert(
                    "model_source".into(),
                    "pci.ids (название PCI-устройства)".into(),
                );
            }
        }
        let drm_nodes = entries(&path.join("drm")).unwrap_or_default();
        let card = drm_nodes
            .iter()
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| numbered(n, "card"))
            })
            .cloned();
        let render = drm_nodes
            .iter()
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| numbered(n, "renderD"))
            })
            .and_then(|p| p.file_name())
            .map(|n| self.dev_root.join("dri").join(n));
        let raw = RawGpu {
            identity,
            metadata,
            values: BTreeMap::new(),
            engines: BTreeMap::new(),
            amd_metrics: Err(GpuError::NotSupported),
            energy_at: None,
        };
        let mut cached = CachedGpu {
            path,
            card,
            render,
            sources: sysfs::Sources::default(),
            raw,
            pmu: None,
            probe_at: now,
        };
        sysfs::discover(&mut cached);
        if self.native {
            drm::inventory(&mut cached);
        }
        if self.native {
            cached.pmu = perf::GpuPmu::open(&self.sys_root, &cached);
        }
        cached
    }

    pub fn sample(&mut self) -> RawGpuSample {
        let at = Instant::now();
        let paths = match self.discover() {
            Ok(paths) => paths,
            Err(e) => {
                return RawGpuSample {
                    at,
                    devices: Err(e),
                };
            }
        };
        let mut active = BTreeSet::new();
        let has_nvidia = paths
            .iter()
            .any(|p| text(&p.join("vendor")).is_ok_and(|v| v == "0x10de"));
        if self.native
            && has_nvidia
            && self.nvml.is_none()
            && self
                .nvml_attempt
                .is_none_or(|t| at.duration_since(t) >= Duration::from_secs(30))
        {
            self.nvml_attempt = Some(at);
            match nvml::Nvml::open() {
                Ok(nvml) => {
                    self.nvml = Some(nvml);
                    self.nvml_error = None;
                }
                Err(error) => {
                    self.nvml_error = Some(error);
                }
            }
        }
        let mut devices = Vec::new();
        for path in paths {
            let inode = match fs::metadata(&path) {
                Ok(s) => s.ino(),
                Err(_) => continue,
            };
            let driver = fs::canonicalize(path.join("driver")).ok();
            let identity = format!(
                "{}|{inode}|{}",
                path.display(),
                driver.as_deref().unwrap_or(Path::new("")).display()
            );
            active.insert(identity.clone());
            if !self.cached.contains_key(&identity) {
                let cached = self.create(path, identity.clone(), at);
                self.cached.insert(identity.clone(), cached);
            }
            let cached = self.cached.get_mut(&identity).unwrap();
            if at.duration_since(cached.probe_at) >= Duration::from_secs(5) {
                sysfs::discover(cached);
                if self.native {
                    drm::inventory(cached);
                }
                if self.native && cached.pmu.as_ref().is_none_or(|pmu| pmu.failed()) {
                    cached.pmu = perf::GpuPmu::open(&self.sys_root, cached);
                }
                cached.probe_at = at;
            }
            cached.raw.values.clear();
            cached.raw.engines.clear();
            sysfs::sample(cached);
            if self.native {
                drm::sample(cached);
            }
            if let Some(pmu) = &mut cached.pmu {
                cached.raw.engines = pmu.sample();
            }
            if cached
                .raw
                .metadata
                .get("vendor_id")
                .is_some_and(|v| v == "0x10de")
            {
                if let Some(nvml) = &self.nvml {
                    nvml.sample(&mut cached.raw);
                } else {
                    cached.raw.metadata.insert(
                        "nvml".into(),
                        format!(
                            "NVML недоступна: {:?}",
                            self.nvml_error.as_ref().unwrap_or(&GpuError::NotSupported)
                        ),
                    );
                    let error = self.nvml_error.clone().unwrap_or(GpuError::NotSupported);
                    for (id, unit) in [
                        ("load", GpuUnit::Percent),
                        ("memory_load", GpuUnit::Percent),
                        ("power", GpuUnit::MilliWatt),
                        ("power_limit", GpuUnit::MilliWatt),
                        ("temperature", GpuUnit::Celsius),
                        ("core_clock", GpuUnit::Megahertz),
                        ("memory_clock", GpuUnit::Megahertz),
                        ("memory_total", GpuUnit::Byte),
                        ("memory_used", GpuUnit::Byte),
                    ] {
                        if !cached.raw.values.contains_key(id) {
                            put(
                                &mut cached.raw,
                                id,
                                unit,
                                Err(error.clone()),
                                "NVML initialization",
                            );
                        }
                    }
                }
            }
            devices.push(cached.raw.clone());
        }
        self.cached.retain(|id, _| active.contains(id));
        RawGpuSample {
            at,
            devices: Ok(devices),
        }
    }
}

pub(super) fn valid_bdf(s: &str) -> bool {
    s.len() == 12
        && s.as_bytes()[4] == b':'
        && s.as_bytes()[7] == b':'
        && s.as_bytes()[10] == b'.'
        && s.bytes()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7 | 10) || c.is_ascii_hexdigit())
}

fn pci_model(data: &str, vendor: Option<&String>, device: Option<&String>) -> Option<String> {
    let vendor = vendor?.trim_start_matches("0x");
    let device = device?.trim_start_matches("0x");
    let mut selected = false;
    for line in data.lines() {
        if !line.starts_with(['\t', '#']) {
            selected = line
                .get(..4)
                .is_some_and(|id| id.eq_ignore_ascii_case(vendor));
        }
        if selected
            && line.starts_with('\t')
            && !line.starts_with("\t\t")
            && line.len() > 6
            && line
                .get(1..5)
                .is_some_and(|id| id.eq_ignore_ascii_case(device))
        {
            return Some(line[5..].trim().into());
        }
    }
    None
}
