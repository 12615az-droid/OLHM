use super::*;
use std::{
    os::unix::fs::symlink,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "olhm-gpu-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn write(&self, path: &str, s: &str) {
        let p = self.0.join(path);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, s).unwrap();
    }
    fn link(&self, target: &str, name: &str) {
        let name = self.0.join(name);
        fs::create_dir_all(name.parent().unwrap()).unwrap();
        symlink(self.0.join(target), name).unwrap();
    }
    fn collector(&self) -> GpuCollector {
        GpuCollector::with_roots(self.0.join("sys"), self.0.join("dev"))
    }
    fn gpu(&self, bdf: &str, vendor: &str, driver: &str) {
        self.write(&format!("sys/devices/{bdf}/class"), "0x030000");
        self.write(&format!("sys/devices/{bdf}/vendor"), vendor);
        self.write(&format!("sys/devices/{bdf}/device"), "0x1234");
        self.write(
            &format!("sys/devices/{bdf}/product_name"),
            &format!("Fixture {driver}"),
        );
        self.write(&format!("sys/devices/{bdf}/drm/card0/dev"), "226:0");
        self.write(&format!("sys/bus/pci/drivers/{driver}/uevent"), "");
        self.link(
            &format!("sys/bus/pci/drivers/{driver}"),
            &format!("sys/devices/{bdf}/driver"),
        );
        self.link(
            &format!("sys/devices/{bdf}"),
            &format!("sys/bus/pci/devices/{bdf}"),
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn pcie_link_is_live_and_capabilities_are_separate() {
    let f = Fixture::new();
    f.gpu("0000:01:00.0", "0x10de", "nvidia");
    let base = "sys/devices/0000:01:00.0";
    for (file, value) in [
        ("current_link_speed", "2.5 GT/s PCIe"),
        ("current_link_width", "8"),
        ("max_link_speed", "16.0 GT/s PCIe"),
        ("max_link_width", "16"),
    ] {
        f.write(&format!("{base}/{file}"), value);
    }
    let mut c = f.collector();
    let first = c.sample().devices.unwrap().remove(0);
    assert_eq!(
        first.values["pcie_speed"].value.as_deref(),
        Ok("2.5 GT/s PCIe")
    );
    assert_eq!(first.values["pcie_max_width"].value.as_deref(), Ok("16"));
    f.write(&format!("{base}/current_link_speed"), "16.0 GT/s PCIe");
    assert_eq!(
        c.sample().devices.unwrap()[0].values["pcie_speed"]
            .value
            .as_deref(),
        Ok("16.0 GT/s PCIe")
    );
}

#[test]
fn pci_and_drm_discovery_deduplicates_render_nodes_and_multiple_vendors() {
    let f = Fixture::new();
    for (bdf, vendor, driver) in [
        ("0000:01:00.0", "0x1002", "amdgpu"),
        ("0000:02:00.0", "0x10de", "nvidia"),
        ("0000:00:02.0", "0x8086", "i915"),
    ] {
        f.gpu(bdf, vendor, driver);
    }
    f.link("sys/devices/0000:01:00.0", "sys/class/drm/card5/device");
    f.link(
        "sys/devices/0000:01:00.0",
        "sys/class/drm/renderD128/device",
    );
    let raw = f.collector().sample().devices.unwrap();
    assert_eq!(raw.len(), 3);
    assert!(
        raw.iter()
            .all(|g| g.metadata.get("model").unwrap().starts_with("Fixture"))
    );
    assert!(raw.iter().all(|g| !g.metadata.contains_key("nvml_name"))); // no host NVML in fixtures
}

#[test]
fn live_hwmon_values_faults_zero_fans_and_device_replacement() {
    let f = Fixture::new();
    f.gpu("0000:01:00.0", "0x1002", "amdgpu");
    let base = "sys/devices/0000:01:00.0";
    for (file, s) in [
        ("gpu_busy_percent", "42"),
        ("hwmon/hwmon7/name", "amdgpu"),
        ("hwmon/hwmon7/temp1_input", "45000"),
        ("hwmon/hwmon7/temp1_label", "edge"),
        ("hwmon/hwmon7/temp1_fault", "0"),
        ("hwmon/hwmon7/temp2_input", "65000"),
        ("hwmon/hwmon7/temp2_label", "junction"),
        ("hwmon/hwmon7/fan1_input", "0"),
        ("hwmon/hwmon7/pwm1", "0"),
    ] {
        f.write(&format!("{base}/{file}"), s);
    }
    f.link(base, &format!("{base}/hwmon/hwmon7/device"));
    let mut c = f.collector();
    let first = c.sample().devices.unwrap().remove(0);
    assert_eq!(first.values["load"].value.as_deref(), Ok("42"));
    assert_eq!(first.values["fan1_rpm"].value.as_deref(), Ok("0"));
    f.write(&format!("{base}/gpu_busy_percent"), "10");
    f.write(&format!("{base}/hwmon/hwmon7/temp1_fault"), "1");
    let next = c.sample().devices.unwrap().remove(0);
    assert_eq!(next.values["load"].value.as_deref(), Ok("10"));
    assert_eq!(next.values["temperature"].value, Err(GpuError::InvalidData));
    fs::remove_file(f.0.join(format!("{base}/hwmon/hwmon7/device"))).unwrap();
    f.write("sys/devices/other/name", "not gpu");
    f.link("sys/devices/other", &format!("{base}/hwmon/hwmon7/device"));
    assert!(
        !c.sample().devices.unwrap()[0]
            .values
            .contains_key("temperature")
    );
    fs::remove_file(f.0.join("sys/bus/pci/devices/0000:01:00.0")).unwrap();
    assert!(c.sample().devices.unwrap().is_empty());
}

#[test]
fn intel_i915_and_xe_frequency_reasons_are_live_and_not_requested_clocks() {
    for driver in ["i915", "xe"] {
        let f = Fixture::new();
        f.gpu("0000:00:02.0", "0x8086", driver);
        let gt = if driver == "xe" {
            "sys/devices/0000:00:02.0/tile0/gt0"
        } else {
            "sys/devices/0000:00:02.0/drm/card0/gt/gt0"
        };
        let clock = if driver == "xe" {
            "freq0/act_freq"
        } else {
            "rps_act_freq_mhz"
        };
        let reason = if driver == "xe" {
            "freq0/throttle/reason_thermal"
        } else {
            "throttle_reason_thermal"
        };
        f.write(&format!("{gt}/{clock}"), "600");
        f.write(&format!("{gt}/{reason}"), "1");
        let mut collector = f.collector();
        let first = collector.sample();
        assert_eq!(
            first.devices.as_ref().unwrap()[0].values["core_clock"]
                .value
                .as_deref(),
            Ok("600")
        );
        let r = crate::processing::gpu::GpuMonitor::default().update(first);
        assert_eq!(
            r.devices[1]
                .metrics
                .iter()
                .find(|m| m.id == "thermal_throttle")
                .unwrap()
                .value,
            crate::model::MetricValue::Available(1.0)
        );
        f.write(&format!("{gt}/{clock}"), "900");
        assert_eq!(
            collector.sample().devices.unwrap()[0].values["core_clock"]
                .value
                .as_deref(),
            Ok("900")
        );
    }
}

#[test]
fn empty_and_unreadable_discovery_and_pci_names_are_not_invented() {
    let f = Fixture::new();
    assert!(f.collector().sample().devices.is_err());
    fs::create_dir_all(f.0.join("sys/bus/pci/devices")).unwrap();
    assert!(f.collector().sample().devices.unwrap().is_empty());
    let ids = "1002  AMD\n\t1234  Fixture Radeon\n8086  Intel\n\t1234  Other\n";
    assert_eq!(
        pci_model(ids, Some(&"0x1002".into()), Some(&"0x1234".into())),
        Some("Fixture Radeon".into())
    );
    assert!(!valid_bdf("../../../etc"));
}
