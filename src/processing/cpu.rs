use crate::raw::cpu::RawCpuSnapshot;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, PartialEq, Eq)]
pub struct CpuInfo {
    pub vendors: Vec<String>,
    pub models: Vec<String>,
    pub architecture: Option<String>,
    /// Present CPUs when sysfs supplies them, otherwise the discovered CPUs.
    /// This describes the system, not an individual process's affinity limit.
    pub logical_processors: Option<usize>,
    pub physical_cores: Option<usize>,
    /// Additional static characteristics, normalized from the initial snapshot.
    pub details: Vec<crate::model::Property>,
}

type Record = [(String, String)];

fn field<'a>(record: &'a Record, key: &str) -> Option<&'a str> {
    record
        .iter()
        .find_map(|(name, value)| (name == key && !value.trim().is_empty()).then_some(value.trim()))
}

fn number(record: &Record, key: &str) -> Option<u32> {
    field(record, key)?.parse().ok()
}

fn architecture(records: &[Vec<(String, String)>]) -> Option<String> {
    let mut architectures = BTreeSet::new();
    for record in records {
        let value = if let Some(isa) =
            field(record, "isa").filter(|isa| isa.starts_with("rv32") || isa.starts_with("rv64"))
        {
            Some(
                if isa.starts_with("rv64") {
                    "riscv64"
                } else {
                    "riscv32"
                }
                .to_owned(),
            )
        } else if let Some(version) = field(record, "CPU architecture") {
            // ARM's version alone does not establish a 32/64-bit execution mode.
            Some(format!("ARM (architecture {version})"))
        } else if field(record, "vendor_id").is_some_and(|vendor| {
            matches!(
                vendor,
                "GenuineIntel" | "AuthenticAMD" | "HygonGenuine" | "CentaurHauls"
            )
        }) {
            Some(
                match field(record, "flags") {
                    Some(flags) if flags.split_whitespace().any(|flag| flag == "lm") => "x86_64",
                    Some(_) => "x86",
                    None => "x86 (bitness unavailable)",
                }
                .to_owned(),
            )
        } else {
            None
        };
        if let Some(value) = value {
            architectures.insert(value);
        }
    }
    (architectures.len() == 1).then(|| architectures.into_iter().next().unwrap())
}

pub fn process(snapshot: &RawCpuSnapshot) -> CpuInfo {
    let records = snapshot
        .cpuinfo
        .as_ref()
        .map_or(&[][..], |info| info.records.as_slice());
    let values = |keys: &[&str]| -> Vec<String> {
        records
            .iter()
            .filter_map(|record| {
                keys.iter()
                    .find_map(|key| field(record, key))
                    .map(str::to_owned)
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    };
    let record_ids: BTreeSet<u32> = records
        .iter()
        .filter_map(|r| number(r, "processor"))
        .collect();
    let ids: BTreeSet<u32> = snapshot
        .cpu_lists
        .get("present")
        .and_then(|list| list.ids.as_ref())
        .filter(|ids| !ids.is_empty())
        .map(|ids| ids.iter().copied().collect())
        .or_else(|| {
            (!snapshot.logical_cpus.is_empty())
                .then(|| snapshot.logical_cpus.iter().map(|cpu| cpu.id).collect())
        })
        .or_else(|| (!record_ids.is_empty()).then(|| record_ids.clone()))
        .or_else(|| {
            snapshot
                .proc_stat
                .as_ref()
                .filter(|stat| !stat.cpus.is_empty())
                .map(|stat| stat.cpus.keys().copied().collect())
        })
        .unwrap_or_default();

    // Every discovered logical CPU must have a valid package/core pair. Never
    // assume a single socket or count missing/sentinel topology as a real core.
    let topology: BTreeMap<u32, (u32, u32)> = snapshot
        .logical_cpus
        .iter()
        .filter_map(|cpu| {
            let attrs = cpu.topology.as_ref()?;
            Some((
                cpu.id,
                (
                    attrs.get("physical_package_id")?.trim().parse().ok()?,
                    attrs.get("core_id")?.trim().parse().ok()?,
                ),
            ))
        })
        .collect();
    let pairs = ids
        .iter()
        .map(|id| topology.get(id).copied())
        .collect::<Option<BTreeSet<_>>>()
        .or_else(|| {
            if ids != record_ids {
                return None;
            }
            let topology: BTreeMap<u32, (u32, u32)> = records
                .iter()
                .filter_map(|record| {
                    Some((
                        number(record, "processor")?,
                        (number(record, "physical id")?, number(record, "core id")?),
                    ))
                })
                .collect();
            ids.iter().map(|id| topology.get(id).copied()).collect()
        });
    CpuInfo {
        vendors: values(&["vendor_id", "CPU implementer"]),
        models: values(&["model name", "cpu", "Processor"]),
        architecture: architecture(records),
        logical_processors: (!ids.is_empty()).then_some(ids.len()),
        physical_cores: pairs
            .filter(|pairs| !pairs.is_empty())
            .map(|pairs| pairs.len()),
        details: super::cpu_details::properties(snapshot, &ids),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::cpu::RawCpuInfo;

    fn snapshot(text: &str) -> RawCpuSnapshot {
        // All input comes from this fixture, never the host's hardware.
        RawCpuSnapshot {
            timestamp: std::time::SystemTime::UNIX_EPOCH,
            cpuinfo: Some(RawCpuInfo {
                text: text.into(),
                records: text
                    .split("\n\n")
                    .map(|record| {
                        record
                            .lines()
                            .map(|line| {
                                let (key, value) = line.split_once(':').unwrap();
                                (key.trim().into(), value.trim().into())
                            })
                            .collect()
                    })
                    .collect(),
            }),
            proc_stat: None,
            cpu_lists: BTreeMap::new(),
            logical_cpus: Vec::new(),
            hardware_monitors: None,
            thermal_zones: None,
            cooling_devices: None,
            powercap_zones: None,
            vulnerabilities: None,
            capabilities: BTreeMap::new(),
        }
    }

    #[test]
    fn counts_packages_and_smt_without_double_counting_cores() {
        let mut raw = snapshot(
            "processor: 0\nvendor_id: AuthenticAMD\nmodel name: Example\nflags: fpu lm\nphysical id: 0\ncore id: 0\n\nprocessor: 2\nphysical id: 0\ncore id: 0\n\nprocessor: 7\nphysical id: 1\ncore id: 0",
        );
        let info = process(&raw);
        assert_eq!(info.logical_processors, Some(3));
        assert_eq!(info.physical_cores, Some(2));
        assert_eq!(info.architecture.as_deref(), Some("x86_64"));
        assert_eq!(info.models, ["Example"]);
        raw.cpuinfo.as_mut().unwrap().records[2].retain(|(key, _)| key != "physical id");
        assert_eq!(process(&raw).physical_cores, None);
    }

    #[test]
    fn missing_fields_and_non_x86_architectures() {
        let unknown = process(&snapshot("processor: 4"));
        assert_eq!(unknown.logical_processors, Some(1));
        assert_eq!(unknown.physical_cores, None);
        assert_eq!(unknown.architecture, None);
        assert!(unknown.models.is_empty());
        assert_eq!(
            process(&snapshot(
                "processor: 0\nCPU implementer: 0x41\nCPU architecture: 8"
            ))
            .architecture
            .as_deref(),
            Some("ARM (architecture 8)")
        );
        assert_eq!(
            process(&snapshot("processor: 0\nisa: rv64imafdc"))
                .architecture
                .as_deref(),
            Some("riscv64")
        );
        assert_eq!(process(&snapshot("")).logical_processors, None);
    }

    fn detail(info: &CpuInfo, id: &str) -> Option<String> {
        info.details
            .iter()
            .find(|p| p.id == id)
            .unwrap()
            .value
            .clone()
    }

    fn logical(id: u32) -> crate::raw::cpu::RawLogicalCpu {
        crate::raw::cpu::RawLogicalCpu {
            id,
            attributes: None,
            online: Some(true),
            topology: None,
            caches: None,
            frequency: None,
            idle: None,
            thermal_throttle: None,
        }
    }

    #[test]
    fn cache_totals_deduplicate_smt_and_shared_l3_with_id_and_list_fallback() {
        use crate::raw::cpu::RawIndexedAttributes;
        let mut raw = snapshot("processor: 0\nvendor_id: AuthenticAMD\nflags: lm svm");
        raw.logical_cpus = (0..16)
            .map(|id| {
                let mut cpu = logical(id);
                let core = id % 8;
                cpu.caches = Some(
                    [
                        (1, "Data", "48K"),
                        (1, "Instruction", "32K"),
                        (2, "Unified", "1024K"),
                        (3, "Unified", "32768K"),
                    ]
                    .into_iter()
                    .enumerate()
                    .map(|(index, (level, kind, size))| RawIndexedAttributes {
                        index: index as u32,
                        attributes: Some(BTreeMap::from([
                            ("level".into(), level.to_string()),
                            ("type".into(), kind.into()),
                            ("size".into(), size.into()),
                            (
                                "id".into(),
                                if level == 3 {
                                    "0".into()
                                } else {
                                    core.to_string()
                                },
                            ),
                            (
                                "shared_cpu_list".into(),
                                if level == 3 {
                                    "0-15".into()
                                } else {
                                    format!("{core},{}", core + 8)
                                },
                            ),
                        ])),
                    })
                    .collect(),
                );
                cpu.frequency = Some(BTreeMap::from([
                    ("cpuinfo_min_freq".into(), "605310".into()),
                    ("cpuinfo_max_freq".into(), "5582301".into()),
                ]));
                cpu
            })
            .collect();
        let info = process(&raw);
        assert_eq!(detail(&info, "virtualization").as_deref(), Some("AMD-V"));
        assert_eq!(
            detail(&info, "cpuinfo_min_freq").as_deref(),
            Some("605.310 МГц")
        );
        assert_eq!(
            detail(&info, "cpuinfo_max_freq").as_deref(),
            Some("5582.301 МГц")
        );
        for (id, expected) in [
            ("cache_l1d", "384 KiB (экземпляров: 8)"),
            ("cache_l1i", "256 KiB (экземпляров: 8)"),
            ("cache_l2", "8 MiB (экземпляров: 8)"),
            ("cache_l3", "32 MiB (экземпляров: 1)"),
        ] {
            assert_eq!(detail(&info, id).as_deref(), Some(expected));
        }
        raw.logical_cpus[0].caches.as_mut().unwrap()[3]
            .attributes
            .as_mut()
            .unwrap()
            .remove("id");
        assert_eq!(
            detail(&process(&raw), "cache_l3").as_deref(),
            Some("32 MiB (экземпляров: 1)")
        );
        raw.logical_cpus[0].caches.as_mut().unwrap()[3]
            .attributes
            .as_mut()
            .unwrap()
            .insert("size".into(), "broken".into());
        assert_eq!(detail(&process(&raw), "cache_l3"), None);
        raw.logical_cpus[0].caches = None;
        assert!(
            detail(&process(&raw), "cache_l3")
                .unwrap()
                .contains("неполные данные")
        );
    }

    #[test]
    fn optional_cpu_details_do_not_invent_limits_or_virtualization() {
        let mut raw = snapshot("processor: 0\nvendor_id: GenuineIntel\nflags: lm vmx");
        let mut cpu = logical(0);
        cpu.frequency = Some(BTreeMap::from([
            ("cpuinfo_min_freq".into(), "800000".into()),
            ("cpuinfo_max_freq".into(), "3200000".into()),
        ]));
        raw.logical_cpus = vec![cpu, logical(1)];
        let info = process(&raw);
        assert_eq!(detail(&info, "virtualization").as_deref(), Some("VT-x"));
        assert_eq!(
            detail(&info, "cpuinfo_max_freq").as_deref(),
            Some("3200.000 МГц (данные 1/2 CPU)")
        );
        assert_eq!(detail(&info, "ppt"), None);
        assert_eq!(detail(&info, "tdp"), None);
        assert_eq!(detail(&info, "cache_l3"), None);
        raw.logical_cpus[0]
            .frequency
            .as_mut()
            .unwrap()
            .insert("cpuinfo_min_freq".into(), "9999999".into());
        assert_eq!(detail(&process(&raw), "cpuinfo_max_freq"), None);
        assert_eq!(
            detail(&process(&snapshot("processor: 0")), "virtualization"),
            None
        );
        assert_eq!(
            detail(
                &process(&snapshot("processor: 0\nflags: lm")),
                "virtualization"
            )
            .as_deref(),
            Some("не объявлена системой")
        );
    }

    #[test]
    fn power_limits_keep_documented_tdp_socket_caps_and_rapl_separate() {
        use crate::raw::cpu::RawSysfsDevice;
        let sensor = |name: &str, driver: &str, key: &str, value: &str| RawSysfsDevice {
            name: name.into(),
            canonical_path: None,
            attributes: Some(BTreeMap::from([
                ("name".into(), driver.into()),
                (key.into(), value.into()),
            ])),
        };
        let mut raw = snapshot("processor: 0");
        raw.hardware_monitors = Some(vec![
            sensor("gpu", "amdgpu", "power1_crit", "300000000"),
            sensor("cpu", "fam15h_power", "power1_crit", "95000000"),
            sensor("socket", "amd_hsmp_hwmon", "power1_cap", "180000000"),
        ]);
        let mut zone = sensor(
            "intel-rapl:0",
            "package-0",
            "constraint_0_power_limit_uw",
            "65000000",
        );
        zone.attributes
            .as_mut()
            .unwrap()
            .insert("constraint_0_name".into(), "long_term".into());
        raw.powercap_zones = Some(vec![
            zone,
            sensor(
                "intel-rapl:0:0",
                "core",
                "constraint_0_power_limit_uw",
                "10000000",
            ),
        ]);
        let info = process(&raw);
        assert_eq!(
            detail(&info, "tdp").as_deref(),
            Some("cpu: 95.0 Вт (fam15h_power / power1_crit)")
        );
        assert_eq!(detail(&info, "ppt"), None);
        assert_eq!(
            detail(&info, "socket_cap_socket").as_deref(),
            Some("180.0 Вт")
        );
        assert_eq!(
            detail(&info, "rapl_limit_intel-rapl:0_0").as_deref(),
            Some("65.0 Вт")
        );
        assert!(
            info.details
                .iter()
                .all(|p| !p.id.contains("intel-rapl:0:0"))
        );
        assert!(
            info.details
                .iter()
                .all(|p| !p.value.as_deref().is_some_and(|v| v.contains("300.0")))
        );
    }

    #[test]
    fn sysfs_topology_includes_offline_cpus_and_rejects_unknown_ids() {
        use crate::raw::cpu::{RawCpuList, RawLogicalCpu};

        let mut raw = snapshot(
            "processor: 0\nvendor_id: GenuineIntel\nmodel name: Example Intel\nflags: lm\nphysical id: 0\ncore id: 0",
        );
        raw.cpu_lists.insert(
            "present".into(),
            RawCpuList {
                text: "0,2".into(),
                ids: Some(vec![0, 2]),
            },
        );
        raw.logical_cpus = [0, 2]
            .into_iter()
            .map(|id| RawLogicalCpu {
                id,
                attributes: None,
                online: Some(id == 0),
                topology: Some(BTreeMap::from([
                    ("physical_package_id".into(), "0\n".into()),
                    ("core_id".into(), format!("{id}\n")),
                ])),
                caches: None,
                frequency: None,
                idle: None,
                thermal_throttle: None,
            })
            .collect();
        let info = process(&raw);
        assert_eq!(info.logical_processors, Some(2));
        assert_eq!(info.physical_cores, Some(2));
        assert_eq!(info.vendors, ["GenuineIntel"]);
        raw.logical_cpus[1]
            .topology
            .as_mut()
            .unwrap()
            .insert("core_id".into(), "-1\n".into());
        // cpuinfo only covers the online CPU; it cannot fill missing topology.
        assert_eq!(process(&raw).physical_cores, None);
    }
}
