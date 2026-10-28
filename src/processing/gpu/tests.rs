use super::*;
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};
fn gpu() -> RawGpu {
    RawGpu {
        identity: "fixture".into(),
        metadata: BTreeMap::new(),
        values: BTreeMap::new(),
        engines: BTreeMap::new(),
        amd_metrics: Err(GpuError::NotSupported),
        energy_at: None,
    }
}
fn set(gpu: &mut RawGpu, id: &str, n: &str, unit: GpuUnit) {
    gpu.values.insert(
        id.into(),
        RawGpuValue {
            value: Ok(n.into()),
            unit,
            source: "fixture".into(),
        },
    );
}
fn get<'a>(r: &'a DeviceReading, id: &str) -> &'a MetricValue {
    &r.metrics.iter().find(|m| m.id == id).unwrap().value
}

#[test]
fn pcie_units_errors_and_width_validation() {
    let mut g = gpu();
    set(&mut g, "pcie_speed", "16.0 GT/s PCIe", GpuUnit::PcieSpeed);
    set(&mut g, "pcie_width", "8", GpuUnit::PcieWidth);
    let r = reading(g.clone(), None, None);
    assert_eq!(*get(&r, "pcie_speed"), MetricValue::Available(16.0));
    assert_eq!(*get(&r, "pcie_width"), MetricValue::Available(8.0));
    assert_eq!(*get(&r, "pcie_max_speed"), MetricValue::NotSupported);
    set(&mut g, "pcie_speed", "Unknown", GpuUnit::PcieSpeed);
    set(&mut g, "pcie_width", "255", GpuUnit::PcieWidth);
    let r = reading(g, None, None);
    assert_eq!(*get(&r, "pcie_speed"), MetricValue::NotAvailable);
    assert_eq!(*get(&r, "pcie_width"), MetricValue::InvalidData);
}

#[test]
fn nvidia_clock_event_masks_do_not_classify_generic_hw_slowdown_as_thermal() {
    let mut g = gpu();
    set(&mut g, "nvml_supported_reasons", "255", GpuUnit::Bits);
    set(&mut g, "nvml_active_reasons", "8", GpuUnit::Bits);
    let r = reading(g.clone(), None, None);
    assert_eq!(*get(&r, "thermal_throttle"), MetricValue::NotAvailable);
    assert_eq!(*get(&r, "power_throttle"), MetricValue::NotAvailable);
    set(&mut g, "nvml_active_reasons", "0", GpuUnit::Bits);
    let r = reading(g.clone(), None, None);
    assert_eq!(*get(&r, "thermal_throttle"), MetricValue::Available(0.0));
    set(&mut g, "nvml_active_reasons", "36", GpuUnit::Bits);
    let r = reading(g, None, None);
    assert_eq!(*get(&r, "thermal_throttle"), MetricValue::Available(1.0));
    assert_eq!(*get(&r, "power_throttle"), MetricValue::Available(1.0));
}

#[test]
fn intel_partial_gt_coverage_is_not_a_confirmed_global_no() {
    let mut g = gpu();
    g.metadata.insert("intel_gt_count".into(), "2".into());
    set(&mut g, "gt0_throttle_thermal", "0", GpuUnit::Boolean);
    assert_eq!(
        *get(&reading(g.clone(), None, None), "thermal_throttle"),
        MetricValue::NotSupported
    );
    set(&mut g, "gt1_throttle_thermal", "0", GpuUnit::Boolean);
    assert_eq!(
        *get(&reading(g.clone(), None, None), "thermal_throttle"),
        MetricValue::Available(0.0)
    );
    set(&mut g, "gt1_throttle_thermal", "1", GpuUnit::Boolean);
    assert_eq!(
        *get(&reading(g, None, None), "thermal_throttle"),
        MetricValue::Available(1.0)
    );
}

#[test]
fn memory_pool_scope_capacity_relations_and_power_domains_are_checked() {
    let mut g = gpu();
    g.metadata.insert("memory_kind".into(), "shared".into());
    set(&mut g, "memory_total", "1024", GpuUnit::Byte);
    set(&mut g, "memory_used", "256", GpuUnit::Byte);
    set(&mut g, "power", "50000000", GpuUnit::MicroWatt);
    set(&mut g, "power_limit", "100000000", GpuUnit::MicroWatt);
    for key in ["power_scope", "power_limit_scope"] {
        g.metadata.insert(key.into(), "soc".into());
    }
    for key in ["power_domain", "power_limit_domain"] {
        g.metadata.insert(key.into(), "PPT1".into());
    }
    let r = reading(g.clone(), None, None);
    assert_eq!(*get(&r, "memory_free"), MetricValue::Available(768.0));
    assert_eq!(*get(&r, "memory_usage"), MetricValue::Available(25.0));
    assert_eq!(*get(&r, "power_usage"), MetricValue::Available(50.0));
    assert!(
        r.metrics
            .iter()
            .find(|m| m.id == "memory_total")
            .unwrap()
            .label
            .contains("UMA")
    );
    g.metadata
        .insert("power_limit_domain".into(), "different".into());
    assert_eq!(
        *get(&reading(g.clone(), None, None), "power_usage"),
        MetricValue::NotSupported
    );
    set(&mut g, "memory_used", "2048", GpuUnit::Byte);
    let r = reading(g, None, None);
    assert_eq!(*get(&r, "memory_usage"), MetricValue::InvalidData);
}

#[test]
fn unsupported_permissions_sensor_faults_and_real_zero_fans_remain_distinct() {
    let mut g = gpu();
    set(&mut g, "temperature", "44000", GpuUnit::MilliCelsius);
    set(&mut g, "load", "101", GpuUnit::Percent);
    set(&mut g, "fan1_rpm", "0", GpuUnit::Rpm);
    set(&mut g, "fan1_pwm", "0", GpuUnit::Pwm);
    g.values.insert(
        "power".into(),
        RawGpuValue {
            value: Err(GpuError::PermissionDenied),
            unit: GpuUnit::MicroWatt,
            source: "fixture".into(),
        },
    );
    let r = reading(g, None, None);
    assert_eq!(*get(&r, "temperature"), MetricValue::Available(44.0));
    assert_eq!(*get(&r, "hotspot"), MetricValue::NotSupported);
    assert_eq!(*get(&r, "power"), MetricValue::PermissionDenied);
    assert_eq!(*get(&r, "load"), MetricValue::InvalidData);
    assert_eq!(*get(&r, "fan1_rpm"), MetricValue::Available(0.0));
    assert_eq!(*get(&r, "thermal_throttle"), MetricValue::NotSupported);
    assert_eq!(*get(&r, "power_throttle"), MetricValue::NotSupported);
    let ids: std::collections::BTreeSet<_> = r.metrics.iter().map(|m| &m.id).collect();
    assert_eq!(ids.len(), r.metrics.len());
}

#[test]
fn intel_pmu_needs_two_valid_measurements_and_resets_on_replacement_or_failure() {
    let at = Instant::now();
    let mut monitor = GpuMonitor::default();
    let mut g = gpu();
    g.engines.insert(
        "rcs0".into(),
        Ok(RawEngineCounter {
            busy: 10,
            total: 100,
        }),
    );
    let first = monitor.update(RawGpuSample {
        at,
        devices: Ok(vec![g.clone()]),
    });
    assert_eq!(*get(&first.devices[1], "engine/rcs0"), MetricValue::Waiting);
    g.engines.insert(
        "rcs0".into(),
        Ok(RawEngineCounter {
            busy: 60,
            total: 200,
        }),
    );
    let second = monitor.update(RawGpuSample {
        at: at + Duration::from_secs(1),
        devices: Ok(vec![g.clone()]),
    });
    assert_eq!(
        *get(&second.devices[1], "engine/rcs0"),
        MetricValue::Available(50.0)
    );
    g.identity = "replacement".into();
    let r = monitor.update(RawGpuSample {
        at: at + Duration::from_secs(2),
        devices: Ok(vec![g.clone()]),
    });
    assert_eq!(*get(&r.devices[1], "engine/rcs0"), MetricValue::Waiting);
    g.engines
        .insert("rcs0".into(), Err(GpuError::PermissionDenied));
    let r = monitor.update(RawGpuSample {
        at: at + Duration::from_secs(3),
        devices: Ok(vec![g]),
    });
    assert_eq!(
        *get(&r.devices[1], "engine/rcs0"),
        MetricValue::PermissionDenied
    );
}

#[test]
fn amd_metrics_layouts_sentinels_and_independent_throttle_bits_are_not_guessed() {
    let mut g = gpu();
    let mut data = vec![255; 120];
    data[..2].copy_from_slice(&120u16.to_le_bytes());
    data[2] = 1;
    data[3] = 3;
    data[6..8].copy_from_slice(&85u16.to_le_bytes());
    data[16..18].copy_from_slice(&50u16.to_le_bytes());
    data[112..120].copy_from_slice(&((1u64 << 36) | 1).to_le_bytes());
    g.amd_metrics = Ok(data.clone());
    let r = reading(g.clone(), None, None);
    assert_eq!(*get(&r, "hotspot"), MetricValue::Available(85.0));
    assert_eq!(*get(&r, "thermal_throttle"), MetricValue::Available(1.0));
    assert_eq!(*get(&r, "power_throttle"), MetricValue::Available(1.0));
    assert_eq!(*get(&r, "temperature"), MetricValue::NotSupported);
    data[3] = 99;
    g.amd_metrics = Ok(data);
    assert_eq!(
        *get(&reading(g, None, None), "thermal_throttle"),
        MetricValue::NotSupported
    );
}

#[test]
fn energy_counter_wrap_and_unknown_power_scope_do_not_invent_watts_or_ratios() {
    let mut old = gpu();
    set(&mut old, "energy", "1000000", GpuUnit::MicroWatt);
    let mut g = old.clone();
    set(&mut g, "energy", "3000000", GpuUnit::MicroWatt);
    let r = reading(g.clone(), Some(&old), Some(Duration::from_secs(2)));
    assert_eq!(*get(&r, "power"), MetricValue::Available(1.0));
    assert_eq!(*get(&r, "power_usage"), MetricValue::NotSupported);
    set(&mut g, "energy", "1", GpuUnit::MicroWatt);
    assert_eq!(
        *get(
            &reading(g, Some(&old), Some(Duration::from_secs(2))),
            "power"
        ),
        MetricValue::CounterReset
    );
}
