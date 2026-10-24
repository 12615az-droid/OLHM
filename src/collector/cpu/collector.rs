use super::{
    cache, cpufreq, cpuidle, cpuinfo, hwmon, powercap, proc_stat, sysfs, thermal, topology,
    vulnerabilities,
};
use crate::{
    collector::Collector,
    error::{CollectError, InterfaceStatus},
    raw::cpu::*,
};
use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Debug)]
pub struct CpuCollector {
    proc_root: PathBuf,
    cpu_root: PathBuf,
    class_roots: Option<CpuClassRoots>,
}

#[derive(Debug)]
struct CpuClassRoots {
    hwmon: PathBuf,
    thermal: PathBuf,
    powercap: PathBuf,
}
impl Default for CpuCollector {
    fn default() -> Self {
        Self::with_roots_and_classes(
            "/proc",
            "/sys/devices/system/cpu",
            "/sys/class/hwmon",
            "/sys/class/thermal",
            "/sys/class/powercap",
        )
    }
}
impl CpuCollector {
    /// Pin discovered sensors to canonical paths; static identity is read once.
    pub fn monitor(&self, initial: &RawCpuSnapshot) -> super::CpuMonitorCollector {
        super::CpuMonitorCollector::new(&self.proc_root, &self.cpu_root, initial)
    }

    /// Alternate roots permit deterministic fixtures and mounted Linux trees.
    pub fn with_roots(proc_root: impl Into<PathBuf>, cpu_root: impl Into<PathBuf>) -> Self {
        Self {
            proc_root: proc_root.into(),
            cpu_root: cpu_root.into(),
            class_roots: None,
        }
    }

    /// Alternate roots including optional Linux sensor classes. This keeps
    /// fixture tests independent from the host's real /sys tree.
    pub fn with_roots_and_classes(
        proc_root: impl Into<PathBuf>,
        cpu_root: impl Into<PathBuf>,
        hwmon_root: impl Into<PathBuf>,
        thermal_root: impl Into<PathBuf>,
        powercap_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            proc_root: proc_root.into(),
            cpu_root: cpu_root.into(),
            class_roots: Some(CpuClassRoots {
                hwmon: hwmon_root.into(),
                thermal: thermal_root.into(),
                powercap: powercap_root.into(),
            }),
        }
    }
}

fn parsed<T>(
    path: &Path,
    caps: &mut CpuCapabilities,
    parser: impl FnOnce(&str) -> Result<T, String>,
) -> Option<T> {
    let text = sysfs::read(path, caps)?;
    match parser(&text) {
        Ok(value) => Some(value),
        Err(error) => {
            caps.insert(path.to_owned(), InterfaceStatus::ParseError(error));
            None
        }
    }
}

impl Collector for CpuCollector {
    type Output = RawCpuSnapshot;
    fn collect(&self) -> Result<Self::Output, CollectError> {
        let timestamp = SystemTime::now();
        let mut capabilities = CpuCapabilities::new();
        let cpuinfo = parsed(
            &self.proc_root.join("cpuinfo"),
            &mut capabilities,
            cpuinfo::parse,
        );
        let proc_stat = parsed(
            &self.proc_root.join("stat"),
            &mut capabilities,
            proc_stat::parse,
        );
        let mut cpu_lists = std::collections::BTreeMap::new();
        for name in ["online", "offline", "present", "possible"] {
            let path = self.cpu_root.join(name);
            if let Some(text) = sysfs::read(&path, &mut capabilities) {
                let ids = match sysfs::parse_cpu_list(&text) {
                    Ok(ids) => Some(ids),
                    Err(error) => {
                        capabilities.insert(path, InterfaceStatus::ParseError(error));
                        None
                    }
                };
                cpu_lists.insert(name.to_owned(), RawCpuList { text, ids });
            }
        }
        let directories = sysfs::directories(&self.cpu_root, "cpu", &mut capabilities);
        // Missing optional sources produce a partial snapshot. Only total loss
        // of the three primary discovery sources prevents meaningful collection.
        if cpuinfo.is_none()
            && proc_stat.is_none()
            && directories.as_ref().is_none_or(Vec::is_empty)
        {
            return Err(CollectError {
                path: self.cpu_root.clone(),
                message: format!("no CPU source available: {capabilities:?}"),
            });
        }
        let mut logical_cpus = Vec::new();
        for (id, path) in directories.into_iter().flatten() {
            let attributes = sysfs::attributes(&path, &mut capabilities);
            let online = parsed(&path.join("online"), &mut capabilities, sysfs::parse_online);
            logical_cpus.push(RawLogicalCpu {
                id,
                attributes,
                online,
                topology: topology::collect(&path.join("topology"), &mut capabilities),
                caches: cache::collect(&path.join("cache"), &mut capabilities),
                frequency: cpufreq::collect(&path.join("cpufreq"), &mut capabilities),
                idle: cpuidle::collect(&path.join("cpuidle"), &mut capabilities),
                thermal_throttle: sysfs::attributes(
                    &path.join("thermal_throttle"),
                    &mut capabilities,
                ),
            });
        }
        let (hardware_monitors, thermal_zones, cooling_devices, powercap_zones) =
            match &self.class_roots {
                Some(roots) => {
                    let thermal = thermal::collect(&roots.thermal, &mut capabilities);
                    (
                        hwmon::collect(&roots.hwmon, &mut capabilities),
                        thermal.zones,
                        thermal.cooling_devices,
                        powercap::collect(&roots.powercap, &mut capabilities),
                    )
                }
                None => (None, None, None, None),
            };
        let vulnerabilities =
            vulnerabilities::collect(&self.cpu_root.join("vulnerabilities"), &mut capabilities);
        Ok(RawCpuSnapshot {
            timestamp,
            cpuinfo,
            proc_stat,
            cpu_lists,
            logical_cpus,
            hardware_monitors,
            thermal_zones,
            cooling_devices,
            powercap_zones,
            vulnerabilities,
            capabilities,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "olhm-fixture-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn write(&self, name: &str, value: &str) {
            let path = self.0.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, value).unwrap();
        }
        fn collect(&self) -> Result<RawCpuSnapshot, CollectError> {
            CpuCollector::with_roots(self.0.join("proc"), self.0.join("cpu")).collect()
        }
        fn collect_with_classes(&self) -> Result<RawCpuSnapshot, CollectError> {
            CpuCollector::with_roots_and_classes(
                self.0.join("proc"),
                self.0.join("cpu"),
                self.0.join("hwmon"),
                self.0.join("thermal"),
                self.0.join("powercap"),
            )
            .collect()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn sparse_cpus_optional_interfaces_and_exact_attributes() {
        let fixture = Fixture::new();
        fixture.write("proc/cpuinfo", "processor: 0\n\nprocessor: 12\n");
        fixture.write("proc/stat", "cpu 1 2 3 4\ncpu0 1 2 3 4\n");
        fixture.write("cpu/online", "0\n");
        fixture.write("cpu/offline", "12\n");
        fixture.write("cpu/present", "0,12\n");
        fixture.write("cpu/possible", "0-12\n");
        fixture.write("cpu/cpu0/topology/core_id", "7\n");
        fixture.write("cpu/cpu0/topology/future", "unknown value\n");
        fixture.write("cpu/cpu0/cache/index3/size", "32K\n");
        fixture.write("cpu/cpu0/cpufreq/scaling_cur_freq", "4200000\n");
        fixture.write("cpu/cpu0/cpuidle/state2/usage", "19\n");
        fixture.write("cpu/cpu0/cpuidle/state2/name", "C2\n");
        fixture.write("cpu/cpu12/online", "0\n");
        fixture.write("cpu/vulnerabilities/example", "Mitigation: example\n");
        fixture.write("cpu/cpux/online", "1\n");
        let snapshot = fixture.collect().unwrap();
        assert_eq!(
            snapshot
                .logical_cpus
                .iter()
                .map(|cpu| cpu.id)
                .collect::<Vec<_>>(),
            vec![0, 12]
        );
        let cpu = &snapshot.logical_cpus[0];
        assert_eq!(cpu.online, None);
        assert_eq!(cpu.topology.as_ref().unwrap()["future"], "unknown value\n");
        assert_eq!(cpu.caches.as_ref().unwrap()[0].index, 3);
        assert_eq!(
            cpu.frequency.as_ref().unwrap()["scaling_cur_freq"],
            "4200000\n"
        );
        assert_eq!(cpu.idle.as_ref().unwrap().states[0].index, 2);
        assert_eq!(snapshot.logical_cpus[1].online, Some(false));
        assert!(snapshot.logical_cpus[1].frequency.is_none());
        assert_eq!(
            snapshot.capabilities[&fixture.0.join("cpu/cpu12/cpufreq")],
            InterfaceStatus::NotAvailable
        );
        assert_eq!(
            snapshot.vulnerabilities.unwrap()["example"],
            "Mitigation: example\n"
        );
    }

    #[test]
    fn monitor_reads_live_values_without_recollecting_static_identity() {
        let fixture = Fixture::new();
        fixture.write("proc/cpuinfo", "processor: 0\nvendor_id: AuthenticAMD\n");
        fixture.write("proc/stat", "cpu 10 0 10 80\ncpu0 10 0 10 80\n");
        fixture.write("cpu/online", "0\n");
        fixture.write("cpu/cpu0/cpufreq/scaling_cur_freq", "4200000\n");
        fixture.write("cpu/cpu0/thermal_throttle/core_throttle_count", "7\n");
        fixture.write("hwmon/hwmon3/name", "k10temp\n");
        fixture.write("hwmon/hwmon3/temp1_label", "Tctl\n");
        fixture.write("hwmon/hwmon3/temp1_input", "45000\n");
        fixture.write("hwmon/hwmon3/temp1_crit", "95000\n");
        fixture.write("hwmon/hwmon3/temp1_crit_alarm", "0\n");
        let initial = fixture.collect_with_classes().unwrap();
        let collector = CpuCollector::with_roots(fixture.0.join("proc"), fixture.0.join("cpu"));
        let mut sampler = collector.monitor(&initial);
        let first = sampler.sample();
        assert_eq!(
            first.cpus[&0].frequency_khz.as_deref().unwrap(),
            "4200000\n"
        );
        assert_eq!(first.temperatures[0].input.as_deref().unwrap(), "45000\n");
        assert_eq!(
            first.temperatures[0].critical.as_deref().unwrap(),
            "95000\n"
        );
        assert_eq!(
            first.temperatures[0].critical_alarm.as_deref().unwrap(),
            "0\n"
        );
        assert_eq!(
            first.cpus[&0].throttle_counts["core_throttle_count"]
                .as_deref()
                .unwrap(),
            "7\n"
        );
        fixture.write("proc/cpuinfo", "broken static data");
        fixture.write("hwmon/hwmon3/name", "changed identity");
        fixture.write("hwmon/hwmon3/temp1_label", "changed label");
        fixture.write("hwmon/hwmon3/temp1_input", "47000\n");
        fixture.write("hwmon/hwmon3/temp1_crit", "99000\n");
        fixture.write("hwmon/hwmon3/temp1_crit_alarm", "1\n");
        fixture.write("cpu/cpu0/thermal_throttle/core_throttle_count", "8\n");
        fixture.write("cpu/cpu0/cpufreq/scaling_cur_freq", "3800000\n");
        fixture.write("proc/stat", "cpu 20 0 20 160\ncpu0 20 0 20 160\n");
        let second = sampler.sample();
        assert_eq!(
            second.stat.unwrap().aggregate.unwrap().counters,
            [20, 0, 20, 160]
        );
        assert_eq!(
            second.cpus[&0].frequency_khz.as_deref().unwrap(),
            "3800000\n"
        );
        assert_eq!(second.temperatures[0].driver, "k10temp");
        assert_eq!(second.temperatures[0].label.as_deref(), Some("Tctl"));
        assert_eq!(second.temperatures[0].input.as_deref().unwrap(), "47000\n");
        assert_eq!(
            second.temperatures[0].critical.as_deref().unwrap(),
            "95000\n"
        );
        assert_eq!(
            second.temperatures[0].critical_alarm.as_deref().unwrap(),
            "1\n"
        );
        assert_eq!(
            second.cpus[&0].throttle_counts["core_throttle_count"]
                .as_deref()
                .unwrap(),
            "8\n"
        );
        std::fs::remove_file(fixture.0.join("hwmon/hwmon3/temp1_input")).unwrap();
        fixture.write("proc/stat", "malformed");
        let missing = sampler.sample();
        assert_eq!(
            missing.temperatures[0].input,
            Err(InterfaceStatus::NotAvailable)
        );
        assert!(matches!(missing.stat, Err(InterfaceStatus::ParseError(_))));
        fixture.write("hwmon/hwmon3/temp1_input", "49000\n");
        assert_eq!(
            sampler
                .sample_with_retry_clock(
                    std::time::Instant::now() + std::time::Duration::from_secs(2)
                )
                .temperatures[0]
                .input
                .as_deref()
                .unwrap(),
            "49000\n"
        );
    }

    #[test]
    fn monitor_preserves_unreadable_temperature_channels_without_labels() {
        let fixture = Fixture::new();
        fixture.write("proc/cpuinfo", "processor: 0\n");
        fixture.write("hwmon/hwmon1/name", "coretemp\n");
        std::fs::write(fixture.0.join("hwmon/hwmon1/temp1_input"), [0xff]).unwrap();
        let initial = fixture.collect_with_classes().unwrap();
        let collector = CpuCollector::with_roots(fixture.0.join("proc"), fixture.0.join("cpu"));
        let mut sampler = collector.monitor(&initial);
        let first = sampler.sample();
        assert_eq!(first.temperatures.len(), 1);
        assert!(matches!(
            first.temperatures[0].input,
            Err(InterfaceStatus::ReadError(_))
        ));
        fixture.write("hwmon/hwmon1/temp1_input", "42000\n");
        assert_eq!(
            sampler
                .sample_with_retry_clock(
                    std::time::Instant::now() + std::time::Duration::from_secs(2)
                )
                .temperatures[0]
                .input
                .as_deref()
                .unwrap(),
            "42000\n"
        );
    }

    #[test]
    fn monitor_power_reads_updates_and_preserves_errors_without_using_limits() {
        let fixture = Fixture::new();
        fixture.write("proc/cpuinfo", "processor: 0\n");
        fixture.write("powercap/intel-rapl:0/name", "package-0\n");
        fixture.write("powercap/intel-rapl:0/energy_uj", "10000000\n");
        fixture.write("powercap/intel-rapl:0/max_energy_range_uj", "1000000000\n");
        fixture.write(
            "powercap/intel-rapl:0/constraint_0_power_limit_uw",
            "999000000\n",
        );
        let initial = fixture.collect_with_classes().unwrap();
        let collector = CpuCollector::with_roots(fixture.0.join("proc"), fixture.0.join("cpu"));
        let mut sampler = collector.monitor(&initial);
        let first = sampler.sample();
        assert_eq!(first.power.len(), 1);
        assert_eq!(first.power[0].energy_uj.as_deref().unwrap(), "10000000\n");
        assert_eq!(first.power[0].power_uw, Err(InterfaceStatus::NotAvailable));
        fixture.write("powercap/intel-rapl:0/energy_uj", "35000000\n");
        assert_eq!(
            sampler.sample().power[0].energy_uj.as_deref().unwrap(),
            "35000000\n"
        );
        fs::remove_file(fixture.0.join("powercap/intel-rapl:0/energy_uj")).unwrap();
        assert_eq!(
            sampler.sample().power[0].energy_uj,
            Err(InterfaceStatus::NotAvailable)
        );
        fixture.write("powercap/intel-rapl:0/energy_uj", "40000000\n");
        assert_eq!(
            sampler
                .sample_with_retry_clock(
                    std::time::Instant::now() + std::time::Duration::from_secs(2)
                )
                .power[0]
                .energy_uj
                .as_deref()
                .unwrap(),
            "40000000\n"
        );
    }

    #[test]
    fn monitor_retries_missing_sensor_after_backoff_without_caching_live_values() {
        use std::time::{Duration, Instant};
        let fixture = Fixture::new();
        fixture.write("proc/cpuinfo", "processor: 0\n");
        fixture.write("hwmon/hwmon1/name", "coretemp\n");
        fixture.write("hwmon/hwmon1/temp1_label", "Package id 0\n");
        let initial = fixture.collect_with_classes().unwrap();
        let collector = CpuCollector::with_roots(fixture.0.join("proc"), fixture.0.join("cpu"));
        let mut sampler = collector.monitor(&initial);
        let now = Instant::now();
        assert_eq!(
            sampler.sample_with_retry_clock(now).temperatures[0].input,
            Err(InterfaceStatus::NotAvailable)
        );
        fixture.write("hwmon/hwmon1/temp1_input", "40000\n");
        assert_eq!(
            sampler
                .sample_with_retry_clock(now + Duration::from_millis(500))
                .temperatures[0]
                .input,
            Err(InterfaceStatus::NotAvailable)
        );
        assert_eq!(
            sampler
                .sample_with_retry_clock(now + Duration::from_secs(1))
                .temperatures[0]
                .input
                .as_deref()
                .unwrap(),
            "40000\n"
        );
        fixture.write("hwmon/hwmon1/temp1_input", "42000\n");
        assert_eq!(
            sampler
                .sample_with_retry_clock(now + Duration::from_millis(1100))
                .temperatures[0]
                .input
                .as_deref()
                .unwrap(),
            "42000\n"
        );
    }

    #[test]
    fn monitor_keeps_known_cpus_during_discovery_failure_and_recovers_metadata() {
        use std::time::{Duration, Instant};
        let fixture = Fixture::new();
        fixture.write("proc/cpuinfo", "processor: 0\n");
        fixture.write("proc/stat", "cpu 1 0 0 9\ncpu0 1 0 0 9\n");
        fixture.write("cpu/cpu0/online", "1\n");
        fixture.write("powercap/intel-rapl:0/name", "package-0\n");
        fixture.write("powercap/intel-rapl:0/energy_uj", "1000000\n");
        fixture.write("powercap/intel-rapl:0/max_energy_range_uj", "placeholder");
        fs::write(
            fixture.0.join("powercap/intel-rapl:0/max_energy_range_uj"),
            [0xff],
        )
        .unwrap();
        let initial = fixture.collect_with_classes().unwrap();
        let collector = CpuCollector::with_roots(fixture.0.join("proc"), fixture.0.join("cpu"));
        let mut sampler = collector.monitor(&initial);
        let now = Instant::now();
        let first = sampler.sample_with_retry_clock(now);
        assert!(matches!(
            first.power[0].max_energy_range_uj,
            Err(InterfaceStatus::ReadError(_))
        ));
        fs::rename(fixture.0.join("cpu"), fixture.0.join("cpu-hidden")).unwrap();
        fs::remove_file(fixture.0.join("proc/stat")).unwrap();
        let failed = sampler.sample_with_retry_clock(now + Duration::from_millis(100));
        assert!(failed.cpus.contains_key(&0));
        assert_eq!(failed.stat.unwrap_err(), InterfaceStatus::NotAvailable);
        fs::rename(fixture.0.join("cpu-hidden"), fixture.0.join("cpu")).unwrap();
        fixture.write("proc/stat", "cpu 2 0 0 18\ncpu0 2 0 0 18\n");
        fixture.write("powercap/intel-rapl:0/max_energy_range_uj", "100000000\n");
        let recovered = sampler.sample_with_retry_clock(now + Duration::from_secs(2));
        assert!(recovered.stat.is_ok());
        assert_eq!(
            recovered.power[0].max_energy_range_uj.as_deref().unwrap(),
            "100000000\n"
        );
    }
    #[test]
    fn malformed_optional_data_and_failed_file_do_not_abort() {
        let fixture = Fixture::new();
        fixture.write("cpu/cpu8/online", "bad\n");
        fixture.write("cpu/online", "2-0\n");
        fixture.write("cpu/cpu8/topology/non_utf8", "placeholder");
        fs::write(fixture.0.join("cpu/cpu8/topology/non_utf8"), [0xff]).unwrap();
        let snapshot = fixture.collect().unwrap();
        assert!(snapshot.logical_cpus[0].online.is_none());
        assert!(snapshot.cpu_lists["online"].ids.is_none());
        assert_eq!(snapshot.cpu_lists["online"].text, "2-0\n");
        assert!(matches!(
            snapshot.capabilities[&fixture.0.join("cpu/cpu8/online")],
            InterfaceStatus::ParseError(_)
        ));
        assert!(matches!(
            snapshot.capabilities[&fixture.0.join("cpu/cpu8/topology/non_utf8")],
            InterfaceStatus::ReadError(_)
        ));
    }
    #[test]
    fn sensor_classes_preserve_temperature_voltage_power_and_throttling() {
        let fixture = Fixture::new();
        fixture.write("proc/cpuinfo", "processor: 0\n");
        fixture.write("proc/stat", "cpu 1 2 3 4\ncpu0 1 2 3 4\n");
        fixture.write("cpu/cpu0/thermal_throttle/core_throttle_count", "17\n");
        fixture.write("cpu/cpu0/thermal_throttle/package_throttle_count", "23\n");
        fixture.write("hwmon/hwmon7/name", "example_cpu\n");
        fixture.write("hwmon/hwmon7/temp1_input", "51625\n");
        fixture.write("hwmon/hwmon7/in0_input", "1198\n");
        fixture.write("hwmon/hwmon7/power1_input", "65432100\n");
        fixture.write("hwmon/hwmon7/temp1_crit_alarm", "0\n");
        fixture.write("thermal/thermal_zone2/type", "x86_pkg_temp\n");
        fixture.write("thermal/thermal_zone2/temp", "52000\n");
        fixture.write("thermal/thermal_zone2/trip_point_0_type", "critical\n");
        fixture.write("thermal/cooling_device1/type", "Processor\n");
        fixture.write("thermal/cooling_device1/cur_state", "2\n");
        fixture.write("powercap/package-0/name", "package-0\n");
        fixture.write("powercap/package-0/energy_uj", "123456789\n");
        fixture.write(
            "powercap/package-0/constraint_0_power_limit_uw",
            "88000000\n",
        );

        let snapshot = fixture.collect_with_classes().unwrap();
        let cpu = &snapshot.logical_cpus[0];
        assert_eq!(
            cpu.thermal_throttle.as_ref().unwrap()["core_throttle_count"],
            "17\n"
        );
        let hwmon = &snapshot.hardware_monitors.as_ref().unwrap()[0];
        assert_eq!(hwmon.name, "hwmon7");
        assert_eq!(hwmon.attributes.as_ref().unwrap()["temp1_input"], "51625\n");
        assert_eq!(hwmon.attributes.as_ref().unwrap()["in0_input"], "1198\n");
        assert_eq!(
            hwmon.attributes.as_ref().unwrap()["power1_input"],
            "65432100\n"
        );
        assert_eq!(
            snapshot.thermal_zones.as_ref().unwrap()[0]
                .attributes
                .as_ref()
                .unwrap()["trip_point_0_type"],
            "critical\n"
        );
        assert_eq!(
            snapshot.cooling_devices.as_ref().unwrap()[0]
                .attributes
                .as_ref()
                .unwrap()["cur_state"],
            "2\n"
        );
        assert_eq!(
            snapshot.powercap_zones.as_ref().unwrap()[0]
                .attributes
                .as_ref()
                .unwrap()["energy_uj"],
            "123456789\n"
        );
    }
    #[test]
    fn virtual_machine_snapshot_json_preserves_raw_data_and_missing_interfaces() {
        let fixture = Fixture::new();
        let cpuinfo = "processor: 0\nflags: hypervisor\nfuture: first\nfuture: second\n\nprocessor: 1\nflags: hypervisor\n";
        fixture.write("proc/cpuinfo", cpuinfo);
        fixture.write(
            "proc/stat",
            "cpu 11 0 7 90 0 0 0 3 0 0\ncpu0 5 0 3 45 0 0 0 1 0 0\ncpu1 6 0 4 45 0 0 0 2 0 0\n",
        );
        fixture.write("cpu/online", "0-1\n");
        for id in 0..2 {
            fixture.write(&format!("cpu/cpu{id}/topology/core_id"), "0\n");
            fixture.write(&format!("cpu/cpu{id}/cache/index0/size"), "64K\n");
        }
        fixture.write("cpu/cpu1/online", "1\n");
        let snapshot = fixture.collect().unwrap();
        let json = serde_json::to_string(&snapshot).unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["cpuinfo"]["text"], cpuinfo);
        assert_eq!(
            value["cpuinfo"]["records"][0][2],
            serde_json::json!(["future", "first"])
        );
        assert_eq!(
            value["cpuinfo"]["records"][0][3],
            serde_json::json!(["future", "second"])
        );
        assert_eq!(
            value["proc_stat"]["aggregate"]["counters"],
            serde_json::json!([11, 0, 7, 90, 0, 0, 0, 3, 0, 0])
        );
        assert_eq!(value["proc_stat"]["cpus"]["1"]["counters"][7], 2);
        assert_eq!(
            value["cpu_lists"]["online"]["ids"],
            serde_json::json!([0, 1])
        );
        assert!(value["logical_cpus"][0]["online"].is_null());
        assert_eq!(value["logical_cpus"][1]["online"], true);
        for (id, cpu) in value["logical_cpus"].as_array().unwrap().iter().enumerate() {
            assert!(cpu["frequency"].is_null());
            assert!(cpu["idle"].is_null());
            assert_eq!(cpu["caches"][0]["attributes"]["size"], "64K\n");
            for interface in ["cpufreq", "cpuidle"] {
                let path = fixture.0.join(format!("cpu/cpu{id}/{interface}"));
                assert_eq!(snapshot.capabilities[&path], InterfaceStatus::NotAvailable);
                assert_eq!(
                    value["capabilities"][path.to_str().unwrap()]["status"],
                    "not_available"
                );
            }
        }
    }

    #[test]
    fn completely_unavailable_sources_are_fatal() {
        assert!(Fixture::new().collect().is_err());
    }
}
