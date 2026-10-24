use super::{proc_stat, sysfs};
use crate::{
    error::InterfaceStatus,
    raw::{
        cpu::{CpuCapabilities, RawCpuSnapshot},
        cpu_monitor::*,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug)]
struct TemperatureSource {
    path: PathBuf,
    device: String,
    driver: String,
    channel: String,
    label: Option<String>,
}

#[derive(Debug)]
struct PowerSource {
    path: PathBuf,
    zone: String,
    domain: String,
    has_power: bool,
    max_energy_range_uj: Result<String, InterfaceStatus>,
}

#[derive(Debug)]
pub struct CpuMonitorCollector {
    proc_root: PathBuf,
    cpu_root: PathBuf,
    temperatures: Vec<TemperatureSource>,
    power: Vec<PowerSource>,
    known_cpus: BTreeSet<u32>,
    reader: DynamicReader,
}

/// Retry failed paths at most once a second, without caching live values.
/// Both errors and successes are local to a file, not the entire collector.
#[derive(Debug, Default)]
struct DynamicReader {
    failures: BTreeMap<PathBuf, (Instant, InterfaceStatus)>,
}

impl DynamicReader {
    fn directories(&mut self, path: &Path, now: Instant) -> Option<Vec<(u32, PathBuf)>> {
        if self
            .failures
            .get(path)
            .is_some_and(|(retry_at, _)| now < *retry_at)
        {
            return None;
        }
        let mut caps = CpuCapabilities::new();
        let directories = sysfs::directories(path, "cpu", &mut caps);
        if let Some(status) = caps.remove(path)
            && status != InterfaceStatus::Available
        {
            self.failures
                .insert(path.into(), (now + Duration::from_secs(1), status));
        }
        directories
    }
    fn read(&mut self, path: &Path, now: Instant) -> Result<String, InterfaceStatus> {
        if let Some((retry_at, status)) = self.failures.get(path)
            && now < *retry_at
        {
            return Err(status.clone());
        }
        let value = read(path);
        if let Err(status) = &value {
            self.failures
                .insert(path.into(), (now + Duration::from_secs(1), status.clone()));
        } else {
            self.failures.remove(path);
        }
        value
    }
}

fn read(path: &Path) -> Result<String, InterfaceStatus> {
    let mut caps = CpuCapabilities::new();
    sysfs::read(path, &mut caps)
        .ok_or_else(|| caps.remove(path).unwrap_or(InterfaceStatus::NotAvailable))
}

impl CpuMonitorCollector {
    pub(super) fn new(proc_root: &Path, cpu_root: &Path, initial: &RawCpuSnapshot) -> Self {
        let mut temperatures = Vec::new();
        for device in initial.hardware_monitors.iter().flatten() {
            let (Some(path), Some(attrs)) = (&device.canonical_path, &device.attributes) else {
                continue;
            };
            let Some(driver) = attrs.get("name") else {
                continue;
            };
            // List names too: an unreadable input without a label is absent
            // from raw attributes but must remain visible as a failed reading.
            let names = std::fs::read_dir(path)
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    entry
                        .ok()
                        .map(|entry| entry.file_name().to_string_lossy().into_owned())
                });
            let channels: BTreeSet<_> = attrs
                .keys()
                .cloned()
                .chain(names)
                .filter_map(|key| {
                    let channel = key
                        .strip_suffix("_input")
                        .or_else(|| key.strip_suffix("_label"))?;
                    let id = channel.strip_prefix("temp")?;
                    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
                        .then_some(channel.to_owned())
                })
                .collect();
            for channel in channels {
                temperatures.push(TemperatureSource {
                    path: path.clone(),
                    device: device.name.clone(),
                    driver: driver.trim().into(),
                    label: attrs
                        .get(&format!("{channel}_label"))
                        .map(|s| s.trim().to_owned()),
                    channel,
                });
            }
        }
        let mut seen = BTreeSet::new();
        let power = initial
            .powercap_zones
            .iter()
            .flatten()
            .filter_map(|zone| {
                let path = zone.canonical_path.as_ref()?;
                let domain = zone.attributes.as_ref()?.get("name")?.trim();
                if domain.is_empty() || !seen.insert(path.clone()) {
                    return None;
                }
                Some(PowerSource {
                    path: path.clone(),
                    zone: zone.name.clone(),
                    domain: domain.into(),
                    has_power: path.join("power_uw").exists(),
                    max_energy_range_uj: read(&path.join("max_energy_range_uj")),
                })
            })
            .collect();
        Self {
            proc_root: proc_root.into(),
            cpu_root: cpu_root.into(),
            temperatures,
            power,
            known_cpus: initial
                .logical_cpus
                .iter()
                .map(|cpu| cpu.id)
                .chain(
                    initial
                        .proc_stat
                        .iter()
                        .flat_map(|stat| stat.cpus.keys().copied()),
                )
                .chain(
                    initial
                        .cpu_lists
                        .get("present")
                        .and_then(|list| list.ids.as_ref())
                        .into_iter()
                        .flatten()
                        .copied(),
                )
                .collect(),
            reader: DynamicReader::default(),
        }
    }

    /// Selection is supplied by Processing; Collection does not classify drivers.
    pub fn retain_sources(
        &mut self,
        mut temperature: impl FnMut(&str) -> bool,
        mut power: impl FnMut(&str, &str) -> bool,
    ) {
        self.temperatures
            .retain(|source| temperature(&source.driver));
        self.power
            .retain(|source| power(&source.zone, &source.domain));
    }

    /// Read dynamic values only; failures remain local to their readings.
    pub fn sample(&mut self) -> RawCpuSample {
        self.sample_with_retry_clock(Instant::now())
    }

    pub(super) fn sample_with_retry_clock(&mut self, now: Instant) -> RawCpuSample {
        let at = Instant::now();
        let reader = &mut self.reader;
        reader.failures.retain(|_, (retry_at, _)| now < *retry_at);
        let stat = reader
            .read(&self.proc_root.join("stat"), now)
            .and_then(|text| proc_stat::parse_monitor(&text).map_err(InterfaceStatus::ParseError));
        let online = reader
            .read(&self.cpu_root.join("online"), now)
            .ok()
            .and_then(|s| sysfs::parse_cpu_list(&s).ok());
        let mut ids: BTreeSet<u32> = reader
            .directories(&self.cpu_root, now)
            .map(|dirs| dirs.into_iter().map(|(id, _)| id).collect())
            .unwrap_or_else(|| self.known_cpus.clone());
        if let Ok(stat) = &stat {
            ids.extend(stat.cpus.keys().copied());
        }
        if let Some(online) = &online {
            ids.extend(online.iter().copied());
        }
        self.known_cpus = ids.clone();
        let cpus = ids
            .into_iter()
            .map(|id| {
                let path = self.cpu_root.join(format!("cpu{id}"));
                let online = online.as_ref().map(|ids| ids.contains(&id)).or_else(|| {
                    reader
                        .read(&path.join("online"), now)
                        .ok()
                        .and_then(|s| sysfs::parse_online(&s).ok())
                });
                let frequency_khz = if online == Some(false) {
                    Err(InterfaceStatus::NotAvailable)
                } else {
                    reader.read(&path.join("cpufreq/scaling_cur_freq"), now)
                };
                (
                    id,
                    RawCpuSampleEntry {
                        online,
                        frequency_khz,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        let temperatures = self
            .temperatures
            .iter()
            .map(|source| RawCpuTemperature {
                path: source.path.join(format!("{}_input", source.channel)),
                device: source.device.clone(),
                driver: source.driver.clone(),
                channel: source.channel.clone(),
                label: source.label.clone(),
                input: reader.read(&source.path.join(format!("{}_input", source.channel)), now),
                fault: reader.read(&source.path.join(format!("{}_fault", source.channel)), now),
            })
            .collect();
        let power = self
            .power
            .iter_mut()
            .map(|source| {
                // Timestamp each energy read separately: preceding CPU/sysfs work
                // must not distort the elapsed time used for joules / seconds.
                let at = Instant::now();
                let energy_uj = reader.read(&source.path.join("energy_uj"), now);
                if source.max_energy_range_uj.is_err() {
                    source.max_energy_range_uj =
                        reader.read(&source.path.join("max_energy_range_uj"), now);
                }
                RawCpuPower {
                    path: source.path.clone(),
                    zone: source.zone.clone(),
                    domain: source.domain.clone(),
                    at,
                    power_uw: if source.has_power {
                        reader.read(&source.path.join("power_uw"), now)
                    } else {
                        Err(InterfaceStatus::NotAvailable)
                    },
                    energy_uj,
                    max_energy_range_uj: source.max_energy_range_uj.clone(),
                }
            })
            .collect();
        RawCpuSample {
            at,
            stat,
            cpus,
            temperatures,
            power,
        }
    }
}
