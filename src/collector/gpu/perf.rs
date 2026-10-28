//! Device-wide Intel engine counters. No /proc process scraping or privilege changes.
use super::*;
use std::{io::Read, os::fd::FromRawFd};

#[repr(C)]
#[derive(Default)]
struct PerfAttr {
    kind: u32,
    size: u32,
    config: u64,
    sample_period: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup: u32,
    bp_type: u32,
    config1: u64,
}
struct Event {
    file: fs::File,
}
impl Event {
    fn open(kind: u32, config: u64, cpu: i32) -> Result<Self, GpuError> {
        let attr = PerfAttr {
            kind,
            size: 64,
            config,
            read_format: 3,
            ..Default::default()
        };
        // SAFETY: PerfAttr is the 64-byte perf_event_attr v0 ABI. The kernel only
        // reads it during this call. pid=-1 requests a device-wide event on cpu.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_perf_event_open,
                std::ptr::from_ref(&attr),
                -1i32,
                cpu,
                -1i32,
                8u64,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: successful perf_event_open returned a fresh owned descriptor.
        Ok(Self {
            file: unsafe { fs::File::from_raw_fd(fd as i32) },
        })
    }
    fn read(&mut self) -> Result<(u64, u64), GpuError> {
        let mut data = [0; 24];
        self.file.read_exact(&mut data).map_err(GpuError::from)?;
        let n = |start| u64::from_ne_bytes(data[start..start + 8].try_into().unwrap());
        // Reject multiplexing rather than silently using unscheduled time as idle.
        if n(8) != n(16) {
            return Err(GpuError::NotAvailable);
        }
        Ok((n(0), n(8)))
    }
}
struct Engine {
    busy: Result<Event, GpuError>,
    total: Option<Result<Event, GpuError>>,
}
pub(super) struct GpuPmu {
    engines: BTreeMap<String, Engine>,
}

fn config(path: &Path) -> Result<u64, GpuError> {
    let s = text(path)?;
    let s = s
        .strip_prefix("event=")
        .or_else(|| s.strip_prefix("config="))
        .ok_or(GpuError::InvalidData)?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|_| GpuError::InvalidData)
}

impl GpuPmu {
    pub(super) fn open(sys: &Path, cached: &CachedGpu) -> Option<Self> {
        let driver = cached.raw.metadata.get("driver")?;
        if !matches!(driver.as_str(), "i915" | "xe") {
            return None;
        }
        let pci = cached.raw.metadata.get("pci")?;
        if !valid_bdf(pci) {
            return None;
        }
        let base = sys.join("bus/event_source/devices");
        let mut path = base.join(format!("{driver}_{}", pci.replace(':', "_")));
        if !path.exists() && driver == "i915" {
            // The unqualified i915 PMU belongs to integrated i915, not a dGPU.
            if cached.raw.metadata.get("memory_kind").map(String::as_str) != Some("shared") {
                return None;
            }
            path = base.join("i915");
        }
        let kind = text(&path.join("type")).ok()?.parse().ok()?;
        let cpu = text(&path.join("cpumask"))
            .or_else(|_| text(&sys.join("devices/system/cpu/online")))
            .ok()
            .and_then(|s| s.split([',', '-']).next()?.parse::<i32>().ok())
            .unwrap_or(0);
        let mut engines = BTreeMap::new();
        if driver == "i915" {
            for file in entries(&path.join("events")).ok()? {
                let name = file.file_name()?.to_str()?;
                let Some(name) = name.strip_suffix("-busy") else {
                    continue;
                };
                if !text(&file.with_file_name(format!("{name}-busy.unit"))).is_ok_and(|s| s == "ns")
                {
                    continue;
                }
                let event = config(&file).and_then(|c| Event::open(kind, c, cpu));
                engines.insert(
                    name.into(),
                    Engine {
                        busy: event,
                        total: None,
                    },
                );
            }
        } else {
            let active = config(&path.join("events/engine-active-ticks")).ok()?;
            let total = config(&path.join("events/engine-total-ticks")).ok()?;
            let data = drm::xe_query(cached, 0).ok()?;
            let count = u32::from_ne_bytes(data.get(..4)?.try_into().ok()?) as usize;
            if count > 128 || data.len() < 8 + count * 32 {
                return None;
            }
            for bytes in data[8..8 + count * 32].as_chunks::<32>().0 {
                let n = |offset| u16::from_ne_bytes(bytes[offset..offset + 2].try_into().unwrap());
                let (class, instance, gt) = (n(0), n(2), n(4));
                if class > 4 || gt > 15 || instance > 255 {
                    continue;
                }
                // PMU ABI: gt[60:63], engine_class[20:27], engine_instance[12:19].
                let select =
                    (u64::from(gt) << 60) | (u64::from(class) << 20) | (u64::from(instance) << 12);
                let name = format!(
                    "GT{gt}/{}-{instance}",
                    match class {
                        0 => "render",
                        1 => "copy",
                        2 => "video (encode/decode)",
                        3 => "video-enhance",
                        _ => "compute",
                    }
                );
                engines.insert(
                    name,
                    Engine {
                        busy: Event::open(kind, active | select, cpu),
                        total: Some(Event::open(kind, total | select, cpu)),
                    },
                );
            }
        }
        if engines.is_empty() {
            None
        } else {
            Some(Self { engines })
        }
    }
    pub(super) fn sample(&mut self) -> BTreeMap<String, Result<RawEngineCounter, GpuError>> {
        self.engines
            .iter_mut()
            .map(|(name, engine)| {
                let value = (|| {
                    let (busy, time) = engine.busy.as_mut().map_err(|e| e.clone())?.read()?;
                    let total = if let Some(event) = &mut engine.total {
                        event.as_mut().map_err(|e| e.clone())?.read()?.0
                    } else {
                        time
                    };
                    Ok(RawEngineCounter { busy, total })
                })();
                (name.clone(), value)
            })
            .collect()
    }
    pub(super) fn failed(&self) -> bool {
        self.engines.values().all(|e| e.busy.is_err())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn event_abi_matches_perf_attr_version_zero() {
        assert_eq!(std::mem::size_of::<PerfAttr>(), 64);
    }
}
