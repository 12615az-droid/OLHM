use super::*;
use std::collections::BTreeSet;

fn counter(text: &str) -> Result<u64, MetricValue> {
    if text.is_empty() || !text.bytes().all(|n| n.is_ascii_digit()) {
        return Err(MetricValue::InvalidData);
    }
    text.parse().map_err(|_| MetricValue::InvalidData)
}

fn metric(id: &str, label: &str, value: Result<u64, MetricValue>) -> Metric {
    Metric {
        id: id.into(),
        label: label.into(),
        unit: Unit::Byte,
        value: match value {
            Ok(n) => MetricValue::Available(n as f64),
            Err(reason) => reason,
        },
    }
}

fn parse(raw: &RawMemoryDetails) -> Result<Vec<DeviceReading>, MetricValue> {
    let text = raw.swaps.as_ref().map_err(failed)?;
    let mut lines = text.lines();
    if lines
        .next()
        .map(|s| s.split_whitespace().collect::<Vec<_>>())
        != Some(vec!["Filename", "Type", "Size", "Used", "Priority"])
    {
        return Err(MetricValue::InvalidData);
    }
    let mut seen = BTreeSet::new();
    let mut readings = Vec::new();
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(MetricValue::InvalidData);
        }
        let filename =
            crate::collector::memory::unescape_path(fields[0]).ok_or(MetricValue::InvalidData)?;
        if !seen.insert(filename.clone()) {
            return Err(MetricValue::InvalidData);
        }
        let total = counter(fields[2])?
            .checked_mul(1024)
            .ok_or(MetricValue::InvalidData)?;
        let used = counter(fields[3])?
            .checked_mul(1024)
            .ok_or(MetricValue::InvalidData)?;
        if used > total {
            return Err(MetricValue::InvalidData);
        }
        let priority = fields[4]
            .parse::<i32>()
            .map_err(|_| MetricValue::InvalidData)?;
        let zram = raw.zram.get(&filename);
        let kind = if zram.is_some() {
            "zram (сжатая RAM)"
        } else if fields[1] == "file" {
            "swap-файл"
        } else {
            "блочное устройство (zram не подтверждён)"
        };
        let mut properties = vec![
            property("path", "Устройство / файл", Some(filename.clone())),
            property("type", "Тип swap", Some(kind.into())),
            property("priority", "Приоритет swap", Some(priority.to_string())),
        ];
        let mut metrics = vec![
            metric("total", "Доступный объём swap", Ok(total)),
            metric("used", "Занято swap", Ok(used)),
        ];
        if let Some(attrs) = zram {
            let get = |name: &str| {
                attrs
                    .get(name)
                    .ok_or(MetricValue::NotAvailable)?
                    .as_ref()
                    .map_err(failed)
            };
            let stats = get("mm_stat").and_then(|text| {
                text.split_whitespace()
                    .take(3)
                    .map(counter)
                    .collect::<Result<Vec<_>, _>>()
            });
            let stat = |index: usize| {
                stats
                    .as_ref()
                    .map_err(Clone::clone)?
                    .get(index)
                    .copied()
                    .ok_or(MetricValue::InvalidData)
            };
            metrics.extend([
                metric(
                    "disksize",
                    "Виртуальная ёмкость zram (не занятая RAM)",
                    get("disksize").and_then(|n| counter(n.trim())),
                ),
                metric("original", "Несжатые данные zram", stat(0)),
                metric("compressed", "Сжатые данные zram", stat(1)),
                metric("ram_used", "Фактически выделено RAM под zram", stat(2)),
            ]);
            let algorithm = get("comp_algorithm").ok().and_then(|text| {
                let mut active = text
                    .split_whitespace()
                    .filter_map(|s| s.strip_prefix('[')?.strip_suffix(']'));
                let name = active.next()?;
                active.next().is_none().then(|| name.to_owned())
            });
            properties.push(property("algorithm", "Алгоритм сжатия zram", algorithm));
        }
        readings.push(DeviceReading {
            device: Device {
                id: format!("ram/swap/{filename}"),
                parent_id: Some("ram".into()),
                kind: DeviceKind::Swap,
                name: format!("Swap [{filename}; {kind}]"),
                properties,
            },
            metrics,
        });
    }
    Ok(readings)
}

pub(super) fn readings(raw: &RawMemoryDetails) -> (Vec<DeviceReading>, String) {
    match parse(raw) {
        Ok(readings) => {
            let status = if readings.is_empty() {
                "активных swap-источников нет".into()
            } else {
                format!(
                    "{} активных источников; сводка meminfo включает все типы",
                    readings.len()
                )
            };
            (readings, status)
        }
        Err(error) => (Vec::new(), reason(&error)),
    }
}
