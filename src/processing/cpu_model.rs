//! CPU adapter to the common processed model. No terminal or filesystem access.
use super::{cpu::CpuInfo, cpu_monitor::CpuMetrics};
use crate::model::*;

pub fn device(info: &CpuInfo) -> Device {
    let list = |values: &[String]| (!values.is_empty()).then(|| values.join(", "));
    Device {
        id: "cpu".into(),
        parent_id: None,
        kind: DeviceKind::Cpu,
        name: "CPU".into(),
        properties: [
            ("vendor", "Производитель", list(&info.vendors)),
            ("model", "Модель", list(&info.models)),
            ("architecture", "Архитектура", info.architecture.clone()),
            (
                "logical_processors",
                "Логических процессоров",
                info.logical_processors.map(|n| n.to_string()),
            ),
            (
                "physical_cores",
                "Физических ядер",
                info.physical_cores.map(|n| n.to_string()),
            ),
        ]
        .into_iter()
        .map(|(id, label, value)| Property {
            id: id.into(),
            label: label.into(),
            value,
        })
        .chain(info.details.iter().cloned())
        .collect(),
    }
}

fn metric(id: &str, label: &str, unit: Unit, value: MetricValue) -> Metric {
    Metric {
        id: id.into(),
        label: label.into(),
        unit,
        value,
    }
}

fn child(id: String, kind: DeviceKind, name: String, metrics: Vec<Metric>) -> DeviceReading {
    DeviceReading {
        device: Device {
            id,
            parent_id: Some("cpu".into()),
            kind,
            name,
            properties: Vec::new(),
        },
        metrics,
    }
}

pub fn snapshot(root: &Device, data: CpuMetrics) -> MonitorSnapshot {
    let mut metrics = vec![
        metric("usage", "Общая загрузка", Unit::Percent, data.usage),
        metric(
            "throttling",
            "Тепловой троттлинг: новые события/учтённое время за интервал",
            Unit::Boolean,
            data.throttling,
        ),
        metric(
            "critical_alarm",
            "Критическая температурная тревога (может быть сохранённой)",
            Unit::Boolean,
            data.critical_alarm,
        ),
    ];
    if data.power.is_empty() {
        metrics.push(metric(
            "power",
            "Потребление CPU",
            Unit::Watt,
            MetricValue::NotSupported,
        ));
    }
    if data.temperatures.is_empty() {
        metrics.push(metric(
            "temperature",
            "Температура CPU",
            Unit::Celsius,
            MetricValue::NotSupported,
        ));
    }
    let mut devices = vec![DeviceReading {
        device: root.clone(),
        metrics,
    }];
    for power in data.power {
        let mode = if power.averaged {
            "; среднее за интервал"
        } else {
            ""
        };
        devices.push(child(
            format!("cpu/power/{}", power.source),
            DeviceKind::PowerDomain,
            format!("Потребление CPU [{}; {}{mode}]", power.source, power.domain),
            vec![metric("power", "Мощность", Unit::Watt, power.watts)],
        ));
    }
    for (id, cpu) in data.cpus {
        devices.push(child(
            format!("cpu/thread/{id}"),
            DeviceKind::LogicalCpu,
            format!("CPU {id}"),
            vec![
                metric("usage", "Загрузка", Unit::Percent, cpu.usage),
                metric("frequency", "Частота", Unit::Megahertz, cpu.frequency_mhz),
            ],
        ));
    }
    for temperature in data.temperatures {
        devices.push(child(
            format!("cpu/temperature/{}", temperature.id),
            DeviceKind::TemperatureSensor,
            format!(
                "Температура CPU [{}; {}]",
                temperature.source, temperature.label
            ),
            vec![
                metric(
                    "temperature",
                    "Температура",
                    Unit::Celsius,
                    temperature.celsius,
                ),
                metric(
                    "critical_temperature",
                    "Критическая температура",
                    Unit::Celsius,
                    temperature.critical_celsius,
                ),
            ],
        ));
    }
    MonitorSnapshot {
        interval: data.interval,
        devices,
    }
}
