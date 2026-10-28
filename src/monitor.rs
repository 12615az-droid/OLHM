//! Collection/Processing orchestration. Presentation consumes only this API
//! and the common model; the polling loop does not dispatch on device types.
use crate::{
    collector::{
        Collector,
        cpu::{CpuCollector, CpuMonitorCollector},
        gpu::GpuCollector,
        memory::MemoryCollector,
        storage::StorageCollector,
    },
    error::CollectError,
    model::{Device, MonitorSnapshot},
    processing::{
        cpu, cpu_model,
        cpu_monitor::{self, CpuMonitor},
        gpu, memory, memory_details, storage,
    },
};
use std::time::Instant;

pub trait MonitorSession {
    fn device(&self) -> &Device;
    fn info_devices(&self) -> Vec<Device> {
        vec![self.device().clone()]
    }
    fn sample(&mut self) -> MonitorSnapshot;
}

/// Register a new category here; CLI parsing and rendering are category-agnostic.
pub const TARGETS: &[&str] = &["cpu", "ram", "storage", "gpu"];

pub fn open(target: &str) -> Result<Box<dyn MonitorSession>, Box<dyn std::error::Error>> {
    match target {
        "cpu" => Ok(Box::new(CpuSession::new(CpuCollector::default())?)),
        "ram" => Ok(Box::new(MemorySession::new(MemoryCollector::default()))),
        "storage" => Ok(Box::new(StorageSession::new(StorageCollector::default()))),
        "gpu" => Ok(Box::new(GpuSession::new(GpuCollector::default()))),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "unsupported hardware category",
        )
        .into()),
    }
}

pub struct CpuSession {
    device: Device,
    collector: CpuMonitorCollector,
    processor: CpuMonitor,
}

impl CpuSession {
    pub fn new(collector: CpuCollector) -> Result<Self, CollectError> {
        let initial = collector.collect()?;
        let device = cpu_model::device(&cpu::process(&initial));
        let mut collector = collector.monitor(&initial);
        collector.retain_sources(
            cpu_monitor::supports_temperature,
            cpu_monitor::supports_power,
        );
        Ok(Self {
            device,
            collector,
            processor: CpuMonitor::default(),
        })
    }
}

impl MonitorSession for CpuSession {
    fn device(&self) -> &Device {
        &self.device
    }
    fn sample(&mut self) -> MonitorSnapshot {
        cpu_model::snapshot(&self.device, self.processor.update(self.collector.sample()))
    }
}

pub struct MemorySession {
    device: Device,
    collector: MemoryCollector,
    previous: Option<Instant>,
    inventory: memory_details::Inventory,
}

impl MemorySession {
    pub fn new(mut collector: MemoryCollector) -> Self {
        let initial = collector.sample();
        let inventory = memory_details::inventory(&collector.inventory());
        let mut device = memory::device(&memory::process(&initial));
        device.properties.extend(inventory.properties.clone());
        Self {
            device,
            collector,
            previous: None,
            inventory,
        }
    }
}

impl MonitorSession for MemorySession {
    fn device(&self) -> &Device {
        &self.device
    }

    fn info_devices(&self) -> Vec<Device> {
        std::iter::once(self.device.clone())
            .chain(
                self.inventory
                    .modules
                    .iter()
                    .map(|module| module.device.clone()),
            )
            .collect()
    }

    fn sample(&mut self) -> MonitorSnapshot {
        // The memory collector retains read failures in its sample.
        let raw = self.collector.sample();
        let interval = self
            .previous
            .and_then(|previous| raw.at.checked_duration_since(previous));
        self.previous = Some(raw.at);
        let mut devices = memory::readings(&self.device, memory::process(&raw));
        let details = self.collector.sample_details();
        let (extra, properties) = memory_details::readings(&self.inventory, &details);
        devices[0].device.properties.extend(properties);
        devices.extend(extra);
        MonitorSnapshot { interval, devices }
    }
}

pub struct GpuSession {
    collector: GpuCollector,
    processor: gpu::GpuMonitor,
    root: Device,
    inventory: Vec<Device>,
    initial: Option<MonitorSnapshot>,
}
impl GpuSession {
    pub fn new(mut collector: GpuCollector) -> Self {
        let mut processor = gpu::GpuMonitor::default();
        let initial = processor.update(collector.sample());
        let root = initial.devices[0].device.clone();
        let inventory = initial.devices.iter().map(|r| r.device.clone()).collect();
        Self {
            collector,
            processor,
            root,
            inventory,
            initial: Some(initial),
        }
    }
}
impl MonitorSession for GpuSession {
    fn device(&self) -> &Device {
        &self.root
    }
    fn info_devices(&self) -> Vec<Device> {
        self.inventory.clone()
    }
    fn sample(&mut self) -> MonitorSnapshot {
        self.initial
            .take()
            .unwrap_or_else(|| self.processor.update(self.collector.sample()))
    }
}

pub struct StorageSession {
    collector: StorageCollector,
    processor: storage::StorageMonitor,
    root: Device,
    inventory: Vec<Device>,
    initial: Option<MonitorSnapshot>,
}

impl StorageSession {
    pub fn new(mut collector: StorageCollector) -> Self {
        let mut processor = storage::StorageMonitor::default();
        let initial = processor.update(collector.sample());
        let root = initial.devices[0].device.clone();
        let inventory = initial
            .devices
            .iter()
            .map(|reading| reading.device.clone())
            .collect();
        Self {
            collector,
            processor,
            root,
            inventory,
            initial: Some(initial),
        }
    }
}

impl MonitorSession for StorageSession {
    fn device(&self) -> &Device {
        &self.root
    }
    fn info_devices(&self) -> Vec<Device> {
        self.inventory.clone()
    }
    fn sample(&mut self) -> MonitorSnapshot {
        self.initial
            .take()
            .unwrap_or_else(|| self.processor.update(self.collector.sample()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DeviceKind, MetricValue, Unit};
    use std::{
        collections::BTreeSet,
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "olhm-session-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn write(&self, name: &str, value: &str) {
            let path = self.0.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, value).unwrap();
        }
        fn collector(&self) -> CpuCollector {
            CpuCollector::with_roots_and_classes(
                self.0.join("proc"),
                self.0.join("cpu"),
                self.0.join("hwmon"),
                self.0.join("thermal"),
                self.0.join("powercap"),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn intel_and_amd_fixtures_flow_through_one_session_and_common_model() {
        for (vendor, driver) in [("GenuineIntel", "coretemp"), ("AuthenticAMD", "k10temp")] {
            let fixture = Fixture::new();
            fixture.write("proc/cpuinfo", &format!("processor: 0\nvendor_id: {vendor}\nmodel name: Fixture CPU\nflags: lm\n\nprocessor: 7\nvendor_id: {vendor}\n"));
            fixture.write(
                "proc/stat",
                "cpu 20 0 0 180\ncpu0 10 0 0 90\ncpu7 10 0 0 90\nbtime 1\n",
            );
            fixture.write("cpu/present", "0,7\n");
            fixture.write("cpu/online", "0,7\n");
            fixture.write("cpu/cpu0/cpufreq/scaling_cur_freq", "4200000\n");
            fixture.write("cpu/cpu7/online", "1\n");
            for name in ["hwmon1", "hwmon2"] {
                fixture.write(&format!("hwmon/{name}/name"), driver);
                fixture.write(&format!("hwmon/{name}/temp1_label"), "Same label\n");
                fixture.write(&format!("hwmon/{name}/temp1_input"), "42500\n");
            }
            fixture.write("hwmon/hwmon3/name", "nvme\n");
            fixture.write("hwmon/hwmon3/temp1_label", "CPU\n");
            fixture.write("hwmon/hwmon3/temp1_input", "99000\n");
            let mut session: Box<dyn MonitorSession> =
                Box::new(CpuSession::new(fixture.collector()).unwrap());
            let first = session.sample();
            assert_eq!(first.devices[0].metrics[0].value, MetricValue::Waiting);
            fixture.write(
                "proc/stat",
                "cpu 70 0 0 330\ncpu0 35 0 0 165\ncpu7 35 0 0 165\nbtime 1\n",
            );
            let second = session.sample();
            assert_eq!(
                second.devices[0].metrics[0].value,
                MetricValue::Available(25.0)
            );
            let cpus: Vec<_> = second
                .devices
                .iter()
                .filter(|reading| reading.device.kind == DeviceKind::LogicalCpu)
                .collect();
            assert_eq!(cpus.len(), 2);
            assert_eq!(cpus[0].metrics[1].value, MetricValue::Available(4200.0));
            assert_eq!(cpus[0].metrics[1].unit, Unit::Megahertz);
            assert_eq!(cpus[1].metrics[1].value, MetricValue::NotAvailable);
            let temperatures: Vec<_> = second
                .devices
                .iter()
                .filter(|reading| reading.device.kind == DeviceKind::TemperatureSensor)
                .collect();
            assert_eq!(temperatures.len(), 2);
            assert_eq!(
                temperatures[0].metrics[0].value,
                MetricValue::Available(42.5)
            );
            let ids = |snapshot: &MonitorSnapshot| {
                snapshot
                    .devices
                    .iter()
                    .map(|reading| reading.device.id.clone())
                    .collect::<BTreeSet<_>>()
            };
            assert_eq!(ids(&first), ids(&second));
            assert_eq!(ids(&second).len(), second.devices.len());
            let json = serde_json::to_value(&second).unwrap();
            assert_eq!(
                json["devices"][0]["metrics"][0]["value"]["status"],
                "available"
            );
            fixture.write("hwmon/hwmon1/temp1_input", "broken\n");
            let failed = session.sample();
            assert!(failed.devices.iter().any(|reading| {
                reading
                    .metrics
                    .iter()
                    .any(|m| m.value == MetricValue::InvalidData)
            }));
            fixture.write("hwmon/hwmon1/temp1_input", "44000\n");
            let recovered = session.sample();
            assert!(recovered.devices.iter().any(|reading| {
                reading
                    .metrics
                    .iter()
                    .any(|m| m.value == MetricValue::Available(44.0))
            }));
        }
    }

    #[test]
    fn cpu_session_separates_latched_alarm_and_recorded_throttle_time() {
        let fixture = Fixture::new();
        fixture.write("proc/cpuinfo", "processor: 0\nvendor_id: GenuineIntel\n");
        fixture.write("proc/stat", "cpu 1 0 0 9\ncpu0 1 0 0 9\n");
        fixture.write("cpu/cpu0/thermal_throttle/core_throttle_count", "5");
        fixture.write(
            "cpu/cpu0/thermal_throttle/core_throttle_total_time_ms",
            "1000",
        );
        fixture.write("hwmon/hwmon0/name", "coretemp");
        fixture.write("hwmon/hwmon0/temp1_input", "40000");
        fixture.write("hwmon/hwmon0/temp1_crit_alarm", "1");
        let mut session = CpuSession::new(fixture.collector()).unwrap();
        let value = |snapshot: &MonitorSnapshot, id: &str| {
            snapshot.devices[0]
                .metrics
                .iter()
                .find(|m| m.id == id)
                .unwrap()
                .value
                .clone()
        };
        let first = session.sample();
        assert_eq!(value(&first, "throttling"), MetricValue::Waiting);
        assert_eq!(value(&first, "critical_alarm"), MetricValue::Available(1.0));
        fixture.write(
            "cpu/cpu0/thermal_throttle/core_throttle_total_time_ms",
            "2000",
        );
        let second = session.sample();
        assert_eq!(value(&second, "throttling"), MetricValue::Available(1.0));
        let third = session.sample();
        assert_eq!(value(&third, "throttling"), MetricValue::Available(0.0));
        assert_eq!(value(&third, "critical_alarm"), MetricValue::Available(1.0));

        let mut output = Vec::new();
        crate::presentation::text::write_snapshot(&mut output, &third).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Тепловой троттлинг: новые события/учтённое время за интервал: нет"));
        assert!(text.contains("Критическая температурная тревога (может быть сохранённой): да"));
    }

    #[test]
    fn ram_session_updates_recovers_and_renders_without_any_cpu_source() {
        let fixture = Fixture::new();
        let mut session = MemorySession::new(MemoryCollector::with_root(fixture.0.join("proc")));
        let missing = session.sample();
        assert_eq!(
            missing.devices[0].metrics[0].value,
            MetricValue::NotAvailable
        );
        fixture.write(
            "proc/meminfo",
            "MemTotal: 1048576 kB\nMemAvailable: 524288 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n",
        );
        let ready = session.sample();
        assert_eq!(ready.devices[0].device.kind, DeviceKind::Memory);
        assert_eq!(
            ready.devices[0].metrics[1].value,
            MetricValue::Available(536870912.0)
        );
        assert_eq!(
            ready.devices[0].metrics[3].value,
            MetricValue::Available(50.0)
        );
        assert_eq!(ready.devices[1].device.kind, DeviceKind::Swap);
        assert_eq!(ready.devices[1].device.parent_id.as_deref(), Some("ram"));
        let mut output = Vec::new();
        crate::presentation::text::write_snapshot(&mut output, &ready).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("Всего RAM (доступно Linux): 1.00 GiB"));
        assert!(text.contains("Использовано RAM: 512.00 MiB"));
        assert!(text.contains("Использование RAM: 50.0 %"));
        assert!(text.contains("Всего swap: 0 B"));
        fixture.write(
            "proc/meminfo",
            "MemTotal: 2097152 kB\nMemAvailable: 1572864 kB\n",
        );
        let changed = session.sample();
        assert_eq!(
            changed.devices[0].metrics[0].value,
            MetricValue::Available(2147483648.0)
        );
        assert_eq!(
            changed.devices[0].metrics[3].value,
            MetricValue::Available(25.0)
        );
        assert_eq!(
            changed.devices[1].metrics[0].value,
            MetricValue::NotAvailable
        );
        std::fs::remove_file(fixture.0.join("proc/meminfo")).unwrap();
        let lost = session.sample();
        assert_eq!(lost.devices[0].metrics[0].value, MetricValue::NotAvailable);
    }

    #[test]
    fn memory_details_collect_live_swap_and_only_confirmed_memory_sensors() {
        let fixture = Fixture::new();
        fixture.write(
            "proc/meminfo",
            "MemTotal: 1024 kB\nMemAvailable: 512 kB\nSwapTotal: 2048 kB\nSwapFree: 2048 kB\n",
        );
        fixture.write("proc/swaps", "Filename Type Size Used Priority\n/dev/zram2 partition 1024 0 100\n/swap file 1024 0 10\n");
        fixture.write("sys/block/zram2/disksize", "1048576");
        fixture.write("sys/block/zram2/mm_stat", "4096 64 8192 0 8192 0 0 0");
        fixture.write("sys/block/zram2/comp_algorithm", "lzo [zstd]");
        fixture.write("sys/bus/i2c/devices/1-0050/name", "spd5118");
        fixture.write("sys/bus/i2c/devices/1-0050/eeprom", "corrupt");
        fixture.write("sys/class/hwmon/hwmon1/name", "spd5118");
        fixture.write("sys/class/hwmon/hwmon1/temp1_input", "42000");
        fixture.write("sys/class/hwmon/hwmon2/name", "k10temp");
        fixture.write("sys/class/hwmon/hwmon2/temp1_input", "99000");
        let mut collector = MemoryCollector::with_root(fixture.0.join("proc"));
        let raw_inventory = collector.inventory();
        assert_eq!(raw_inventory.spd.unwrap().len(), 1);
        let raw = collector.sample_details();
        assert_eq!(raw.temperatures.len(), 1);
        assert!(raw.zram.contains_key("/dev/zram2"));
        assert!(!raw.zram.contains_key("/swap"));
        let mut session = MemorySession::new(collector);
        let first = session.sample();
        let swap: Vec<_> = first
            .devices
            .iter()
            .filter(|d| d.device.kind == DeviceKind::Swap)
            .collect();
        assert_eq!(swap.len(), 3); // Combined summary and two individual sources.
        let temperature = first
            .devices
            .iter()
            .find(|d| d.device.kind == DeviceKind::MemoryTemperature)
            .unwrap();
        assert_eq!(temperature.metrics[0].value, MetricValue::Available(42.0));
        fixture.write("sys/class/hwmon/hwmon1/temp1_input", "43000");
        fixture.write(
            "proc/swaps",
            "Filename Type Size Used Priority\n/swap file 1024 128 10\n",
        );
        let second = session.sample();
        let swap: Vec<_> = second
            .devices
            .iter()
            .filter(|d| d.device.kind == DeviceKind::Swap)
            .collect();
        assert_eq!(swap.len(), 2);
        assert_eq!(swap[1].metrics[1].value, MetricValue::Available(131072.0));
        assert_eq!(
            swap[1].device.id,
            first
                .devices
                .iter()
                .find(|d| d.device.id == swap[1].device.id)
                .unwrap()
                .device
                .id
        );
        assert_eq!(
            second
                .devices
                .iter()
                .find(|d| d.device.kind == DeviceKind::MemoryTemperature)
                .unwrap()
                .metrics[0]
                .value,
            MetricValue::Available(43.0)
        );
    }
}
