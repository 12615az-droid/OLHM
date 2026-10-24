//! Generic text renderer: no collectors, CPU processing or filesystem access.
use super::format::metric;
use crate::model::{Device, MonitorSnapshot};
use std::io::{self, Write};

pub fn write_info(output: &mut impl Write, device: &Device) -> io::Result<()> {
    writeln!(output, "OLHM — {}", device.name)?;
    for property in &device.properties {
        writeln!(
            output,
            "{}: {}",
            property.label,
            property.value.as_deref().unwrap_or("недоступно")
        )?;
    }
    Ok(())
}

pub fn write_monitor(
    output: &mut impl Write,
    snapshot: &MonitorSnapshot,
    refresh: bool,
) -> io::Result<()> {
    if refresh {
        write!(output, "\x1b[H\x1b[2J")?;
    }
    write_snapshot(output, snapshot)?;
    writeln!(output, "Остановка: Ctrl+C\n")
}

pub fn write_snapshot(output: &mut impl Write, snapshot: &MonitorSnapshot) -> io::Result<()> {
    if let Some(interval) = snapshot.interval {
        writeln!(output, "Интервал замеров: {:.3} с", interval.as_secs_f64())?;
    }
    for reading in &snapshot.devices {
        if reading.device.parent_id.is_none() {
            write_info(output, &reading.device)?;
            for value in &reading.metrics {
                writeln!(output, "{}: {}", value.label, metric(value))?;
            }
        } else {
            write!(output, "{}: ", reading.device.name)?;
            for (index, value) in reading.metrics.iter().enumerate() {
                if index != 0 {
                    write!(output, " | ")?;
                }
                write!(output, "{}: {}", value.label, metric(value))?;
            }
            writeln!(output)?;
            for property in &reading.device.properties {
                writeln!(
                    output,
                    "  {}: {}",
                    property.label,
                    property.value.as_deref().unwrap_or("недоступно")
                )?;
            }
        }
    }
    writeln!(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DeviceKind, DeviceReading, Metric, MetricValue, Property, Unit};

    fn fixture() -> MonitorSnapshot {
        MonitorSnapshot {
            interval: Some(std::time::Duration::from_secs(1)),
            devices: vec![DeviceReading {
                device: Device {
                    id: "test-device".into(),
                    parent_id: None,
                    kind: DeviceKind::Cpu,
                    name: "Тестовое устройство".into(),
                    properties: vec![Property {
                        id: "model".into(),
                        label: "Модель".into(),
                        value: None,
                    }],
                },
                metrics: vec![Metric {
                    id: "power".into(),
                    label: "Потребление".into(),
                    unit: Unit::Watt,
                    value: MetricValue::Available(12.5),
                }],
            }],
        }
    }

    #[test]
    fn any_device_renders_without_cpu_specific_logic() {
        let mut output = Vec::new();
        write_snapshot(&mut output, &fixture()).unwrap();
        let text = String::from_utf8(output).unwrap();
        assert!(text.contains("OLHM — Тестовое устройство"));
        assert!(text.contains("Модель: недоступно"));
        assert!(text.contains("Потребление: 12.5 Вт"));
        assert!(!text.contains('\x1b'));
        assert!(!text.contains("Ctrl+C"));
    }

    #[test]
    fn errors_are_distinct_from_missing_data_and_zero() {
        let mut snapshot = fixture();
        let value = &mut snapshot.devices[0].metrics[0];
        value.value = MetricValue::PermissionDenied;
        assert_eq!(metric(value), "нет доступа");
        value.value = MetricValue::ReadError;
        assert_eq!(metric(value), "ошибка чтения");
        value.value = MetricValue::NotAvailable;
        assert_eq!(metric(value), "недоступно");
        value.value = MetricValue::Available(0.0);
        assert_eq!(metric(value), "0.0 Вт");
    }
}
