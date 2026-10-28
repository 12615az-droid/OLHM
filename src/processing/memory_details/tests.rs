use super::*;
use std::collections::BTreeMap;

fn raw(text: &str) -> RawMemoryDetails {
    RawMemoryDetails {
        swaps: Ok(text.into()),
        zram: BTreeMap::new(),
        temperatures: vec![],
    }
}

#[test]
fn separates_zram_and_swapfile_and_keeps_allocated_ram_distinct_from_capacity() {
    let mut raw = raw(
        "Filename Type Size Used Priority\n/dev/zram0 partition 33554428 1024 100\n/swap\\040file file 16777212 0 -2\n",
    );
    raw.zram.insert(
        "/dev/zram0".into(),
        BTreeMap::from([
            ("disksize".into(), Ok("34359738368".into())),
            (
                "mm_stat".into(),
                Ok("1048576 4096 8192 0 8192 0 0 0 0".into()),
            ),
            ("comp_algorithm".into(), Ok("lzo [zstd] lz4".into())),
        ]),
    );
    let (devices, _) = swap::readings(&raw);
    assert_eq!(devices.len(), 2);
    assert!(devices[0].device.name.contains("zram (сжатая RAM)"));
    assert_eq!(
        devices[0]
            .metrics
            .iter()
            .find(|m| m.id == "ram_used")
            .unwrap()
            .value,
        MetricValue::Available(8192.0)
    );
    assert!(
        devices[0]
            .device
            .properties
            .iter()
            .any(|p| p.id == "algorithm" && p.value.as_deref() == Some("zstd"))
    );
    assert!(devices[1].device.name.contains("/swap file; swap-файл"));
    assert_eq!(
        devices[1].metrics[0].value,
        MetricValue::Available(16777212.0 * 1024.0)
    );
    assert!(
        devices[1]
            .device
            .properties
            .iter()
            .any(|p| p.id == "priority" && p.value.as_deref() == Some("-2"))
    );
    assert_ne!(devices[0].device.id, devices[1].device.id);
}

#[test]
fn empty_missing_and_corrupt_swap_lists_are_not_conflated() {
    let header = "Filename Type Size Used Priority\n";
    let empty = swap::readings(&raw(header));
    assert!(empty.0.is_empty());
    assert_eq!(empty.1, "активных swap-источников нет");
    for line in [
        "/swap file 1 2 0",
        "/swap file 1 0 bad",
        "/swap file 18446744073709551615 0 0",
        "/swap\\999 file 1 0 0",
    ] {
        let failed = swap::readings(&raw(&format!("{header}{line}\n")));
        assert!(failed.0.is_empty());
        assert_eq!(failed.1, "некорректные данные");
    }
    let mut missing = raw(header);
    missing.swaps = Err(InterfaceStatus::PermissionDenied("fixture".into()));
    assert_eq!(swap::readings(&missing).1, "нет доступа");
    let named_like_zram = swap::readings(&raw(&format!("{header}/var/zram0 file 1 0 0\n"))).0;
    assert!(named_like_zram[0].device.name.contains("swap-файл"));
}

#[test]
fn counts_installed_not_empty_slots_and_merges_only_matching_identity() {
    let inventory = inventory(&RawMemoryInventory {
        dmi: Ok(vec![
            smbios::tests::fixture(16384),
            smbios::tests::fixture(0),
        ]),
        spd: Ok(vec![spd::tests::fixture(true)]),
    });
    assert_eq!(inventory.modules.len(), 1);
    assert_eq!(inventory.modules[0].serial.as_deref(), Some("00010203"));
    assert!(inventory.modules[0].spd_device.is_some());
    assert!(
        inventory
            .properties
            .iter()
            .any(|p| p.id == "modules" && p.value.as_deref() == Some("1"))
    );
    assert!(
        inventory.modules[0]
            .device
            .properties
            .iter()
            .any(|p| p.id == "configured_speed")
    );
    assert!(
        inventory.modules[0]
            .device
            .properties
            .iter()
            .any(|p| p.id == "spd_speed")
    );
}

#[test]
fn no_dmi_access_still_allows_spd_and_thermal_association_requires_same_device() {
    let inventory = inventory(&RawMemoryInventory {
        dmi: Err(InterfaceStatus::PermissionDenied("fixture".into())),
        spd: Ok(vec![spd::tests::fixture(true)]),
    });
    assert!(
        inventory
            .properties
            .iter()
            .any(|p| p.id == "modules" && p.value.as_deref() == Some("нет доступа"))
    );
    let mut raw = raw("Filename Type Size Used Priority\n");
    raw.temperatures.push(RawMemoryTemperature {
        path: "/fixture/hwmon0".into(),
        device: "/fixture/i2c/1-0051".into(),
        driver: "spd5118".into(),
        input: Ok("41250".into()),
        enabled: Ok("1".into()),
        fault: Err(InterfaceStatus::NotAvailable),
    });
    let (devices, _) = readings(&inventory, &raw);
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].metrics[0].value, MetricValue::Available(41.25));
    raw.temperatures[0].device = "/fixture/i2c/1-0053".into();
    let (devices, _) = readings(&inventory, &raw);
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[0].metrics[0].value, MetricValue::NotAvailable);
    assert_eq!(devices[1].device.kind, DeviceKind::MemoryTemperature);
    raw.temperatures[0].enabled = Ok("0".into());
    assert_eq!(temperature(&raw.temperatures[0]), MetricValue::NotAvailable);
    raw.temperatures[0].enabled = Ok("1".into());
    raw.temperatures[0].fault = Ok("1".into());
    assert_eq!(temperature(&raw.temperatures[0]), MetricValue::InvalidData);
}
