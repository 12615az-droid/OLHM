//! Optional CPU characteristics. No model-name databases or privileged probing.
use crate::{
    model::Property,
    raw::cpu::{RawAttributes, RawCpuSnapshot},
};
use std::collections::{BTreeMap, BTreeSet};

fn property(id: impl Into<String>, label: impl Into<String>, value: Option<String>) -> Property {
    Property {
        id: id.into(),
        label: label.into(),
        value,
    }
}

fn positive(attrs: &RawAttributes, name: &str) -> Option<u64> {
    attrs
        .get(name)?
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
}

pub(super) fn properties(raw: &RawCpuSnapshot, ids: &BTreeSet<u32>) -> Vec<Property> {
    let mut result = vec![virtualization(raw)];
    for (key, label, minimum) in [
        ("cpuinfo_min_freq", "CPU min MHz (cpufreq)", true),
        ("cpuinfo_max_freq", "CPU max MHz (cpufreq)", false),
    ] {
        let values: BTreeMap<_, _> = raw
            .logical_cpus
            .iter()
            .filter(|cpu| ids.contains(&cpu.id))
            .filter_map(|cpu| {
                let attrs = cpu.frequency.as_ref()?;
                let low = positive(attrs, "cpuinfo_min_freq");
                let high = positive(attrs, "cpuinfo_max_freq");
                if matches!((low, high), (Some(a), Some(b)) if a > b) {
                    return None;
                }
                Some((cpu.id, positive(attrs, key)?))
            })
            .collect();
        let value = if minimum {
            values.values().min()
        } else {
            values.values().max()
        };
        result.push(property(
            key,
            label,
            value.map(|khz| {
                let mut text = format!("{}.{:03} МГц", khz / 1000, khz % 1000);
                if values.len() < ids.len() {
                    text.push_str(&format!(" (данные {}/{} CPU)", values.len(), ids.len()));
                }
                text
            }),
        ));
    }
    result.extend(caches(raw, ids));
    result.extend(power_limits(raw));
    result
}

fn virtualization(raw: &RawCpuSnapshot) -> Property {
    let mut observed = false;
    let mut names = BTreeSet::new();
    if let Some(info) = &raw.cpuinfo {
        for record in &info.records {
            for (_, flags) in record.iter().filter(|(key, _)| key == "flags") {
                observed = true;
                for flag in flags.split_whitespace() {
                    match flag {
                        "svm" => {
                            names.insert("AMD-V");
                        }
                        "vmx" => {
                            names.insert("VT-x");
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    property(
        "virtualization",
        "Виртуализация (флаги, видимые Linux)",
        if !names.is_empty() {
            Some(names.into_iter().collect::<Vec<_>>().join(", "))
        } else {
            observed.then(|| "не объявлена системой".into())
        },
    )
}

#[derive(Default)]
struct Cache {
    observations: Vec<CacheObservation>,
}

struct CacheObservation {
    id: Option<u32>,
    shared: Option<Vec<u32>>,
    bytes: Option<u64>,
}

fn cpu_list(text: &str) -> Option<Vec<u32>> {
    let mut ids = BTreeSet::new();
    let parse_id = |text: &str| {
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse::<u32>().ok())
            .flatten()
    };
    for part in text.trim().split(',') {
        let (start, end) = match part.split_once('-') {
            Some((start, end)) => (parse_id(start)?, parse_id(end)?),
            None => {
                let id = parse_id(part)?;
                (id, id)
            }
        };
        if end < start || u64::from(end) - u64::from(start) + 1 + ids.len() as u64 > 1_048_576 {
            return None;
        }
        ids.extend(start..=end);
    }
    (!ids.is_empty() && ids.len() <= 1_048_576).then(|| ids.into_iter().collect())
}

fn cache_bytes(text: &str) -> Option<u64> {
    let text = text.trim();
    let (digits, multiplier) = if let Some(number) = text.strip_suffix('K') {
        (number, 1024)
    } else if let Some(number) = text.strip_suffix('M') {
        (number, 1024 * 1024)
    } else {
        (text, 1)
    };
    digits
        .parse::<u64>()
        .ok()?
        .checked_mul(multiplier)
        .filter(|bytes| *bytes > 0)
}

fn size(bytes: u64) -> String {
    for (unit, multiplier) in [("MiB", 1024 * 1024), ("KiB", 1024)] {
        if bytes.is_multiple_of(multiplier) {
            return format!("{} {unit}", bytes / multiplier);
        }
    }
    format!("{bytes} B")
}

fn caches(raw: &RawCpuSnapshot, ids: &BTreeSet<u32>) -> Vec<Property> {
    let mut groups: BTreeMap<(u32, String), Cache> = BTreeMap::new();
    let mut enumerated = BTreeSet::new();
    let mut incomplete = false;
    for cpu in raw.logical_cpus.iter().filter(|cpu| ids.contains(&cpu.id)) {
        let Some(caches) = &cpu.caches else {
            incomplete = true;
            continue;
        };
        if caches.is_empty() {
            incomplete = true;
        }
        enumerated.insert(cpu.id);
        for cache in caches {
            let Some(attrs) = &cache.attributes else {
                incomplete = true;
                continue;
            };
            let level = attrs
                .get("level")
                .and_then(|text| text.trim().parse::<u32>().ok())
                .filter(|level| *level > 0);
            let kind = attrs
                .get("type")
                .map(|text| text.trim())
                .filter(|kind| matches!(*kind, "Data" | "Instruction" | "Unified"));
            let (Some(level), Some(kind)) = (level, kind) else {
                incomplete = true;
                continue;
            };
            groups
                .entry((level, kind.into()))
                .or_default()
                .observations
                .push(CacheObservation {
                    id: attrs.get("id").and_then(|id| id.trim().parse().ok()),
                    shared: attrs
                        .get("shared_cpu_list")
                        .and_then(|text| cpu_list(text))
                        .filter(|shared| shared.contains(&cpu.id)),
                    bytes: attrs.get("size").and_then(|text| cache_bytes(text)),
                });
        }
    }
    incomplete |= enumerated != *ids;
    if groups.is_empty() {
        return [
            (1, "Data"),
            (1, "Instruction"),
            (2, "Unified"),
            (3, "Unified"),
        ]
        .into_iter()
        .map(|(level, kind)| cache_property(level, kind, None))
        .collect();
    }
    groups
        .into_iter()
        .map(|((level, kind), cache)| {
            // Pick one identity scheme for the entire group. Never mix IDs and
            // shared lists: that can count the same physical cache twice.
            let by_id = cache.observations.iter().all(|entry| entry.id.is_some());
            let mut unique = BTreeMap::new();
            let mut valid = true;
            for entry in cache.observations {
                let key = if by_id {
                    entry.id.map(|id| vec![id])
                } else {
                    entry.shared
                };
                let (Some(key), Some(bytes)) = (key, entry.bytes) else {
                    valid = false;
                    continue;
                };
                if unique.insert(key, bytes).is_some_and(|old| old != bytes) {
                    valid = false;
                }
            }
            let sum = unique
                .values()
                .try_fold(0u64, |sum, bytes| sum.checked_add(*bytes));
            let value = if valid && !unique.is_empty() {
                sum.map(|bytes| {
                    format!(
                        "{} (экземпляров: {}){}",
                        size(bytes),
                        unique.len(),
                        if incomplete {
                            "; неполные данные"
                        } else {
                            ""
                        }
                    )
                })
            } else {
                None
            };
            cache_property(level, &kind, value)
        })
        .collect()
}

fn cache_property(level: u32, kind: &str, value: Option<String>) -> Property {
    let suffix = match kind {
        "Data" => "d",
        "Instruction" => "i",
        _ => "",
    };
    property(
        format!("cache_l{level}{suffix}"),
        format!("Кеш L{level}{suffix} (суммарно)"),
        value,
    )
}

fn power_limits(raw: &RawCpuSnapshot) -> Vec<Property> {
    // No generic Linux attribute means CPU TDP or Ryzen PPT. Driver-specific
    // values are admitted only with documented semantics and CPU association.
    let mut result = vec![property("ppt", "PPT", None)];
    let mut tdp = Vec::new();
    for sensor in raw.hardware_monitors.iter().flatten() {
        let Some(attrs) = &sensor.attributes else {
            continue;
        };
        let driver = attrs.get("name").map(|name| name.trim());
        if driver == Some("fam15h_power") {
            if let Some(microwatts) = positive(attrs, "power1_crit") {
                tdp.push(format!(
                    "{}: {:.1} Вт (fam15h_power / power1_crit)",
                    sensor.name,
                    microwatts as f64 / 1_000_000.0
                ));
            }
        } else if driver == Some("amd_hsmp_hwmon") {
            // Kept as a socket cap, not relabelled as desktop Ryzen PPT/TDP.
            if let Some(microwatts) = positive(attrs, "power1_cap") {
                result.push(property(
                    format!("socket_cap_{}", sensor.name),
                    format!("Лимит мощности сокета [{}; HSMP; при запуске]", sensor.name),
                    Some(format!("{:.1} Вт", microwatts as f64 / 1_000_000.0)),
                ));
            }
        }
    }
    result.push(property(
        "tdp",
        "TDP",
        (!tdp.is_empty()).then(|| tdp.join("; ")),
    ));
    for zone in raw.powercap_zones.iter().flatten() {
        let Some(attrs) = &zone.attributes else {
            continue;
        };
        let Some(domain) = attrs.get("name").map(|name| name.trim()) else {
            continue;
        };
        if !super::cpu_monitor::supports_power(&zone.name, domain) {
            continue;
        }
        for (key, text) in attrs {
            let Some(index) = key
                .strip_prefix("constraint_")
                .and_then(|text| text.strip_suffix("_power_limit_uw"))
                .filter(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
            else {
                continue;
            };
            let Some(microwatts) = text.trim().parse::<u64>().ok().filter(|n| *n > 0) else {
                continue;
            };
            let name = attrs
                .get(&format!("constraint_{index}_name"))
                .map(|text| text.trim())
                .filter(|name| !name.is_empty())
                .unwrap_or(index);
            result.push(property(
                format!("rapl_limit_{}_{}", zone.name, index),
                format!("Лимит RAPL [{}; {domain}; {name}; при запуске]", zone.name),
                Some(format!("{:.1} Вт", microwatts as f64 / 1_000_000.0)),
            ));
        }
    }
    result
}
