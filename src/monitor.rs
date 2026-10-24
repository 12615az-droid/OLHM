//! Collection/Processing orchestration. Presentation consumes only this API
//! and the common model; the polling loop does not dispatch on device types.
use crate::{
    collector::{
        Collector,
        cpu::{CpuCollector, CpuMonitorCollector},
    },
    error::CollectError,
    model::{Device, MonitorSnapshot},
    processing::{
        cpu, cpu_model,
        cpu_monitor::{self, CpuMonitor},
    },
};

pub trait MonitorSession {
    fn device(&self) -> &Device;
    fn sample(&mut self) -> MonitorSnapshot;
}

/// Register a new category here; CLI parsing and rendering are category-agnostic.
pub const TARGETS: &[&str] = &["cpu"];

pub fn open(target: &str) -> Result<Box<dyn MonitorSession>, Box<dyn std::error::Error>> {
    match target {
        "cpu" => Ok(Box::new(CpuSession::new(CpuCollector::default())?)),
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
}
