//! Validate Linux memory counters and adapt them to the shared device model.
use crate::{
    error::InterfaceStatus,
    model::{Device, DeviceKind, DeviceReading, Metric, MetricValue, Property, Unit},
    raw::memory::RawMemorySample,
};
use std::collections::BTreeMap;

type Counter = Result<u64, MetricValue>;

pub struct MemoryMetrics {
    pub total: MetricValue,
    pub available: MetricValue,
    pub used: MetricValue,
    pub usage: MetricValue,
    pub swap_total: MetricValue,
    pub swap_used: MetricValue,
}

fn parse_kib(text: &str) -> Counter {
    let mut parts = text.split_whitespace();
    let value = parts.next().ok_or(MetricValue::InvalidData)?;
    if !value.bytes().all(|byte| byte.is_ascii_digit())
        || parts.next() != Some("kB")
        || parts.next().is_some()
    {
        return Err(MetricValue::InvalidData);
    }
    value
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(1024))
        .ok_or(MetricValue::InvalidData)
}

fn fields(raw: &RawMemorySample) -> Result<BTreeMap<&str, Counter>, MetricValue> {
    let text = raw.meminfo.as_ref().map_err(|status| match status {
        InterfaceStatus::PermissionDenied(_) => MetricValue::PermissionDenied,
        InterfaceStatus::ReadError(_) => MetricValue::ReadError,
        InterfaceStatus::ParseError(_) => MetricValue::InvalidData,
        _ => MetricValue::NotAvailable,
    })?;
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if !matches!(name, "MemTotal" | "MemAvailable" | "SwapTotal" | "SwapFree") {
            continue;
        }
        // A duplicate must not silently replace an ambiguous counter.
        let value = if fields.contains_key(name) {
            Err(MetricValue::InvalidData)
        } else {
            parse_kib(value)
        };
        fields.insert(name, value);
    }
    Ok(fields)
}

fn as_value(value: Counter) -> MetricValue {
    match value {
        Ok(bytes) => MetricValue::Available(bytes as f64),
        Err(reason) => reason,
    }
}

pub fn process(raw: &RawMemorySample) -> MemoryMetrics {
    let fields = fields(raw);
    let get = |name| -> Counter {
        fields
            .as_ref()
            .map_err(Clone::clone)?
            .get(name)
            .cloned()
            .unwrap_or(Err(MetricValue::NotAvailable))
    };
    let total = get("MemTotal").and_then(|n| {
        if n == 0 {
            Err(MetricValue::InvalidData)
        } else {
            Ok(n)
        }
    });
    let mut available = get("MemAvailable");
    if matches!((&total, &available), (Ok(total), Ok(available)) if available > total) {
        available = Err(MetricValue::InvalidData);
    }
    let used = total.clone().and_then(|total| {
        available
            .clone()
            .and_then(|available| total.checked_sub(available).ok_or(MetricValue::InvalidData))
    });
    let usage = match (&used, &total) {
        (Ok(used), Ok(total)) => MetricValue::Available(*used as f64 / *total as f64 * 100.0),
        (Err(reason), _) => reason.clone(),
        (_, Err(reason)) => reason.clone(),
    };
    let swap_total = get("SwapTotal");
    let swap_used = swap_total.clone().and_then(|total| {
        get("SwapFree").and_then(|free| total.checked_sub(free).ok_or(MetricValue::InvalidData))
    });
    MemoryMetrics {
        total: as_value(total),
        available: as_value(available),
        used: as_value(used),
        usage,
        swap_total: as_value(swap_total),
        swap_used: as_value(swap_used),
    }
}

pub fn device(initial: &MemoryMetrics) -> Device {
    Device {
        id: "ram".into(),
        parent_id: None,
        kind: DeviceKind::Memory,
        name: "Оперативная память (RAM)".into(),
        properties: vec![
            Property {
                id: "capacity_at_start".into(),
                label: "Объём RAM, доступный Linux (при запуске)".into(),
                value: match initial.total {
                    MetricValue::Available(bytes) => Some(format!("{bytes:.0} байт")),
                    _ => None,
                },
            },
            Property {
                id: "usage_method".into(),
                label: "Расчёт использования".into(),
                value: Some("MemTotal − MemAvailable; системная память Linux".into()),
            },
        ],
    }
}

pub fn readings(root: &Device, data: MemoryMetrics) -> Vec<DeviceReading> {
    let bytes = |id: &str, label: &str, value| Metric {
        id: id.into(),
        label: label.into(),
        unit: Unit::Byte,
        value,
    };
    vec![
        DeviceReading {
            device: root.clone(),
            metrics: vec![
                bytes("total", "Всего RAM (доступно Linux)", data.total),
                bytes("used", "Использовано RAM", data.used),
                bytes("available", "Доступно RAM", data.available),
                Metric {
                    id: "usage".into(),
                    label: "Использование RAM".into(),
                    unit: Unit::Percent,
                    value: data.usage,
                },
            ],
        },
        DeviceReading {
            device: Device {
                id: "ram/swap".into(),
                parent_id: Some("ram".into()),
                kind: DeviceKind::Swap,
                name: "Swap (суммарно: все источники, включая zram)".into(),
                properties: Vec::new(),
            },
            metrics: vec![
                bytes("total", "Всего swap", data.swap_total),
                bytes("used", "Использовано swap", data.swap_used),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn process_text(text: &str) -> MemoryMetrics {
        process(&RawMemorySample {
            at: Instant::now(),
            meminfo: Ok(text.into()),
        })
    }

    #[test]
    fn uses_memavailable_and_binary_units_with_independent_swap() {
        let data = process_text(
            "MemTotal: 16777216 kB\nMemFree: 1048576 kB\nMemAvailable: 6291456 kB\nSwapTotal: 2097152 kB\nSwapFree: 1572864 kB\nHugePages_Total: 0\n",
        );
        let gib = 1024_f64.powi(3);
        assert_eq!(data.total, MetricValue::Available(16.0 * gib));
        assert_eq!(data.available, MetricValue::Available(6.0 * gib));
        assert_eq!(data.used, MetricValue::Available(10.0 * gib));
        assert_eq!(data.usage, MetricValue::Available(62.5));
        assert_eq!(data.swap_total, MetricValue::Available(2.0 * gib));
        assert_eq!(data.swap_used, MetricValue::Available(0.5 * gib));
    }

    #[test]
    fn missing_available_is_not_replaced_by_free_and_missing_swap_is_not_zero() {
        let data = process_text("MemTotal: 16384 kB\nMemFree: 8192 kB\n");
        assert_eq!(data.total, MetricValue::Available(16384.0 * 1024.0));
        assert_eq!(data.available, MetricValue::NotAvailable);
        assert_eq!(data.used, MetricValue::NotAvailable);
        assert_eq!(data.usage, MetricValue::NotAvailable);
        assert_eq!(data.swap_total, MetricValue::NotAvailable);
        assert_eq!(data.swap_used, MetricValue::NotAvailable);
    }

    #[test]
    fn no_swap_and_fully_available_or_full_ram_are_real_zero_values() {
        for (available, usage) in [("1000", 0.0), ("0", 100.0)] {
            let data = process_text(&format!(
                "MemTotal: 1000 kB\nMemAvailable: {available} kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n"
            ));
            assert_eq!(data.usage, MetricValue::Available(usage));
            assert_eq!(data.swap_total, MetricValue::Available(0.0));
            assert_eq!(data.swap_used, MetricValue::Available(0.0));
        }
    }

    #[test]
    fn invalid_units_duplicates_overflows_and_relations_do_not_invent_usage() {
        for total in [
            "-1 kB",
            "NaN kB",
            "16 MB",
            "16",
            "16 kB extra",
            "18446744073709551615 kB",
            "0 kB",
        ] {
            let data = process_text(&format!(
                "MemTotal: {total}\nMemAvailable: 1 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n"
            ));
            assert_eq!(data.total, MetricValue::InvalidData, "{total}");
            assert_eq!(data.used, MetricValue::InvalidData);
            assert_eq!(data.usage, MetricValue::InvalidData);
            assert_eq!(data.swap_used, MetricValue::Available(0.0));
        }
        let duplicate = process_text("MemTotal: 10 kB\nMemTotal: 10 kB\nMemAvailable: 1 kB\n");
        assert_eq!(duplicate.total, MetricValue::InvalidData);
        let inconsistent =
            process_text("MemTotal: 10 kB\nMemAvailable: 11 kB\nSwapTotal: 2 kB\nSwapFree: 3 kB\n");
        assert_eq!(inconsistent.total, MetricValue::Available(10240.0));
        assert_eq!(inconsistent.available, MetricValue::InvalidData);
        assert_eq!(inconsistent.used, MetricValue::InvalidData);
        assert_eq!(inconsistent.swap_used, MetricValue::InvalidData);
        let bad_swap = process_text("MemTotal: 10 kB\nMemAvailable: 5 kB\nSwapTotal: invalid\n");
        assert_eq!(bad_swap.usage, MetricValue::Available(50.0));
        assert_eq!(bad_swap.swap_total, MetricValue::InvalidData);
    }

    #[test]
    fn whole_interface_errors_remain_explicit() {
        for (status, expected) in [
            (InterfaceStatus::NotAvailable, MetricValue::NotAvailable),
            (
                InterfaceStatus::PermissionDenied("fixture".into()),
                MetricValue::PermissionDenied,
            ),
            (
                InterfaceStatus::ReadError("fixture".into()),
                MetricValue::ReadError,
            ),
        ] {
            let data = process(&RawMemorySample {
                at: Instant::now(),
                meminfo: Err(status),
            });
            assert_eq!(data.total, expected);
            assert_eq!(data.used, expected);
            assert_eq!(data.available, expected);
            assert_eq!(data.usage, expected);
            assert_eq!(data.swap_used, expected);
        }
    }
}
