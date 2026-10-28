use super::*;
use serde_json::json;
use std::time::{Duration, Instant};

fn disk(at: Instant, read: u64, write: u64) -> RawBlock {
    RawBlock {
        name: "nvme0n1".into(),
        path: "/sys/devices/pci/nvme/nvme0/nvme0n1".into(),
        identity: "nvme;259:0;1".into(),
        parent: None,
        partition: false,
        attrs: BTreeMap::from([
            ("size".into(), Ok("2097152".into())),
            ("dev".into(), Ok("259:0".into())),
            ("queue/logical_block_size".into(), Ok("4096".into())),
        ]),
        metadata: BTreeMap::new(),
        stat_at: at,
        stat: Ok(format!("1 0 {read} 0 2 0 {write} 0 0 0 0")),
        temperature: Err(InterfaceStatus::NotAvailable),
        temperature_source: None,
        smart: Err(InterfaceStatus::NotAvailable),
        smart_at: at,
    }
}

fn sample(at: Instant, blocks: Vec<RawBlock>) -> RawStorageSample {
    RawStorageSample {
        at,
        blocks: Ok(blocks),
        mounts: Ok(Vec::new()),
        metadata_status: Err(InterfaceStatus::NotAvailable),
    }
}

fn value<'a>(reading: &'a DeviceReading, id: &str) -> &'a MetricValue {
    &reading.metrics.iter().find(|m| m.id == id).unwrap().value
}

#[test]
fn io_uses_512_byte_sectors_and_per_device_monotonic_interval() {
    let at = Instant::now();
    let mut monitor = StorageMonitor::default();
    let first = monitor.update(sample(at, vec![disk(at, 100, 200)]));
    assert_eq!(value(&first.devices[1], "read"), &MetricValue::Waiting);
    let second = monitor.update(sample(
        at + Duration::from_secs(1),
        vec![disk(at + Duration::from_secs(2), 104, 208)],
    ));
    assert_eq!(
        value(&second.devices[1], "read"),
        &MetricValue::Available(1024.0)
    );
    assert_eq!(
        value(&second.devices[1], "write"),
        &MetricValue::Available(2048.0)
    );
    assert_eq!(
        value(&second.devices[1], "capacity"),
        &MetricValue::Available(1073741824.0)
    );
    let idle = monitor.update(sample(
        at + Duration::from_secs(2),
        vec![disk(at + Duration::from_secs(3), 104, 208)],
    ));
    assert_eq!(
        value(&idle.devices[1], "read"),
        &MetricValue::Available(0.0)
    );
}

#[test]
fn io_resets_errors_removal_and_device_replacement_require_new_baselines() {
    let at = Instant::now();
    let mut monitor = StorageMonitor::default();
    monitor.update(sample(at, vec![disk(at, 100, 200)]));
    let reset = monitor.update(sample(at, vec![disk(at + Duration::from_secs(1), 1, 2)]));
    assert_eq!(value(&reset.devices[1], "read"), &MetricValue::CounterReset);
    let recovered = monitor.update(sample(at, vec![disk(at + Duration::from_secs(2), 3, 4)]));
    assert_eq!(
        value(&recovered.devices[1], "read"),
        &MetricValue::Available(1024.0)
    );
    let mut failed_disk = disk(at + Duration::from_secs(3), 4, 5);
    failed_disk.stat = Err(InterfaceStatus::PermissionDenied("fixture".into()));
    let failed = monitor.update(sample(at, vec![failed_disk]));
    assert_eq!(
        value(&failed.devices[1], "read"),
        &MetricValue::PermissionDenied
    );
    let restored = monitor.update(sample(at, vec![disk(at + Duration::from_secs(4), 5, 6)]));
    assert_eq!(value(&restored.devices[1], "read"), &MetricValue::Waiting);
    monitor.update(sample(at, vec![]));
    let returned = monitor.update(sample(at, vec![disk(at + Duration::from_secs(5), 6, 7)]));
    assert_eq!(value(&returned.devices[1], "read"), &MetricValue::Waiting);
    let mut replacement = disk(at + Duration::from_secs(6), 8, 9);
    replacement.identity = "nvme;259:0;2".into();
    let changed = monitor.update(sample(at, vec![replacement]));
    assert_eq!(value(&changed.devices[1], "read"), &MetricValue::Waiting);
}

#[test]
fn filesystem_reserved_space_matches_df_and_rejects_impossible_values() {
    let stats = RawFsStat {
        fragment_size: 1024,
        blocks: 100,
        free: 30,
        available: 20,
    };
    let values = filesystems::values(&Ok(stats));
    assert_eq!(values[0], MetricValue::Available(102400.0));
    assert_eq!(values[1], MetricValue::Available(71680.0));
    assert_eq!(values[2], MetricValue::Available(20480.0));
    assert_eq!(values[3], MetricValue::Available(30720.0));
    assert_eq!(values[4], MetricValue::Available(70.0 / 90.0 * 100.0));
    for stats in [
        RawFsStat {
            fragment_size: 0,
            blocks: 1,
            free: 0,
            available: 0,
        },
        RawFsStat {
            fragment_size: 1024,
            blocks: 1,
            free: 2,
            available: 0,
        },
        RawFsStat {
            fragment_size: u64::MAX,
            blocks: 2,
            free: 1,
            available: 0,
        },
    ] {
        assert_eq!(filesystems::values(&Ok(stats))[0], MetricValue::InvalidData);
    }
    assert_eq!(
        filesystems::values(&Err(InterfaceStatus::PermissionDenied("fixture".into())))[2],
        MetricValue::PermissionDenied
    );
}

#[test]
fn partitions_link_to_disks_and_mount_aliases_are_grouped_without_summing() {
    let at = Instant::now();
    let disk = disk(at, 1, 2);
    let mut part = disk.clone();
    part.name = "nvme0n1p2".into();
    part.identity = "partition-2".into();
    part.partition = true;
    part.parent = Some(disk.name.clone());
    part.attrs.insert("dev".into(), Ok("259:2".into()));
    let mount = |id: &str, target: &str| RawMount {
        id: id.into(),
        major_minor: "0:33".into(),
        root: "/".into(),
        target: target.into(),
        fs_type: "btrfs".into(),
        source: "/dev/nvme0n1p2".into(),
        options: "rw".into(),
        stats: Ok(RawFsStat {
            fragment_size: 4096,
            blocks: 100,
            free: 50,
            available: 40,
        }),
    };
    let mut raw = sample(at, vec![disk, part]);
    raw.mounts = Ok(vec![mount("1", "/"), mount("2", "/bind alias")]);
    let snapshot = StorageMonitor::default().update(raw);
    assert_eq!(
        snapshot.devices[2].device.parent_id.as_ref(),
        Some(&snapshot.devices[1].device.id)
    );
    assert_eq!(
        snapshot.devices[3].device.parent_id.as_ref(),
        Some(&snapshot.devices[2].device.id)
    );
    assert_eq!(snapshot.devices.len(), 4);
    assert!(
        snapshot.devices[3]
            .device
            .properties
            .iter()
            .any(|p| p.id == "mountpoints" && p.value.as_deref() == Some("/; /bind alias"))
    );
    assert_eq!(
        value(&snapshot.devices[3], "total"),
        &MetricValue::Available(409600.0)
    );
}

#[test]
fn smart_failure_exit_bit_is_health_data_and_missing_permissions_are_not_healthy() {
    let at = Instant::now();
    let mut raw = disk(at, 0, 0);
    raw.smart = Ok(
        json!({"smartctl":{"exit_status":8},"smart_status":{"passed":false},"temperature":{"current":42},"nvme_smart_health_information_log":{"critical_warning":1,"percentage_used":125,"available_spare":8,"media_errors":2}}),
    );
    let reading = block(&raw, None, &[]);
    assert_eq!(
        value(&reading, "smart_failed"),
        &MetricValue::Available(1.0)
    );
    assert_eq!(
        value(&reading, "nvme_warning"),
        &MetricValue::Available(1.0)
    );
    assert_eq!(
        value(&reading, "percentage_used"),
        &MetricValue::Available(125.0)
    );
    assert_eq!(
        value(&reading, "temperature"),
        &MetricValue::Available(42.0)
    );
    raw.smart =
        Ok(json!({"smartctl":{"exit_status":2,"messages":[{"string":"Permission denied"}]}}));
    assert_eq!(
        value(&block(&raw, None, &[]), "smart_failed"),
        &MetricValue::PermissionDenied
    );
    raw.smart = Ok(
        json!({"smartctl":{"exit_status":3,"messages":[{"string":"Device is in STANDBY mode, exit(3)"}]}}),
    );
    let standby = block(&raw, None, &[]);
    assert_eq!(value(&standby, "smart_failed"), &MetricValue::NotAvailable);
    assert!(
        standby
            .device
            .properties
            .iter()
            .any(|p| p.id == "smart_assessment" && p.value.as_ref().unwrap().contains("standby"))
    );
    raw.smart = Err(InterfaceStatus::NotAvailable);
    assert_eq!(
        value(&block(&raw, None, &[]), "smart_failed"),
        &MetricValue::NotAvailable
    );
}

#[test]
fn hwmon_temperature_has_priority_and_fallback_keeps_correct_provenance() {
    let at = Instant::now();
    let mut raw = disk(at, 0, 0);
    raw.smart = Ok(json!({"smart_status":{"passed":true},"temperature":{"current":40}}));
    raw.temperature = Ok("42500".into());
    raw.temperature_source = Some("nvme hwmon".into());
    let current = block(&raw, None, &[]);
    assert_eq!(
        value(&current, "temperature"),
        &MetricValue::Available(42.5)
    );
    raw.temperature = Err(InterfaceStatus::NotAvailable);
    let fallback = block(&raw, None, &[]);
    assert_eq!(
        value(&fallback, "temperature"),
        &MetricValue::Available(40.0)
    );
    assert!(
        fallback
            .device
            .properties
            .iter()
            .any(|p| p.id == "temperature_source"
                && p.value.as_ref().unwrap().starts_with("smartctl"))
    );
    raw.temperature = Ok("NaN".into());
    assert_eq!(
        value(&block(&raw, None, &[]), "temperature"),
        &MetricValue::InvalidData
    );
}
