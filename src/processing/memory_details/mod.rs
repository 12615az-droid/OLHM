//! Optional module inventories and live memory-specific readings.
//! Firmware configuration and SPD capabilities are never labelled live timings.
use crate::{error::InterfaceStatus, model::*, raw::memory::*};
use std::path::PathBuf;
mod smbios;
mod spd;
mod swap;
#[cfg(test)]
mod tests;

pub struct Module {
    pub device: Device,
    spd_device: Option<PathBuf>,
    serial: Option<String>,
    part: Option<String>,
}

pub struct Inventory {
    pub modules: Vec<Module>,
    pub properties: Vec<Property>,
}

fn property(id: &str, label: &str, value: Option<String>) -> Property {
    Property {
        id: id.into(),
        label: label.into(),
        value,
    }
}

fn failed(status: &InterfaceStatus) -> MetricValue {
    match status {
        InterfaceStatus::PermissionDenied(_) => MetricValue::PermissionDenied,
        InterfaceStatus::ReadError(_) => MetricValue::ReadError,
        InterfaceStatus::ParseError(_) => MetricValue::InvalidData,
        _ => MetricValue::NotAvailable,
    }
}

fn reason(value: &MetricValue) -> String {
    match value {
        MetricValue::PermissionDenied => "нет доступа".into(),
        MetricValue::InvalidData => "некорректные данные".into(),
        MetricValue::ReadError => "ошибка чтения".into(),
        MetricValue::NotSupported => "не поддерживается".into(),
        _ => "недоступно".into(),
    }
}

fn capacity(bytes: u64) -> String {
    format!("{:.2} GiB ({bytes} байт)", bytes as f64 / 1024_f64.powi(3))
}

fn word(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

pub fn inventory(raw: &RawMemoryInventory) -> Inventory {
    let mut modules = Vec::new();
    let mut count = 0;
    let mut error = None;
    match &raw.dmi {
        Ok(entries) if !entries.is_empty() => {
            for entry in entries {
                match smbios::parse(entry) {
                    Ok(Some(module)) => {
                        modules.push(module);
                        count += 1;
                    }
                    Ok(None) => {}
                    Err(reason) => error = Some(reason),
                }
            }
        }
        Ok(_) => error = Some(MetricValue::NotAvailable),
        Err(status) => error = Some(failed(status)),
    }
    let mut properties = vec![property(
        "modules",
        "Установлено модулей/устройств памяти (SMBIOS)",
        Some(
            error
                .as_ref()
                .map(reason)
                .unwrap_or_else(|| count.to_string()),
        ),
    )];
    match &raw.spd {
        Ok(entries) => {
            properties.push(property(
                "spd_devices",
                "SPD-устройств, видимых ядру (список может быть неполным)",
                Some(entries.len().to_string()),
            ));
            for entry in entries {
                match spd::parse(entry) {
                    Ok(module) => {
                        // Merge only a unique, non-placeholder serial + part match.
                        let matches: Vec<_> = modules
                            .iter()
                            .enumerate()
                            .filter(|(_, old)| {
                                old.spd_device.is_none()
                                    && old
                                        .serial
                                        .as_ref()
                                        .zip(module.serial.as_ref())
                                        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
                                    && old
                                        .part
                                        .as_ref()
                                        .zip(module.part.as_ref())
                                        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
                            })
                            .map(|(index, _)| index)
                            .collect();
                        if matches.len() == 1 {
                            let old = &mut modules[matches[0]];
                            old.device.properties.extend(module.device.properties);
                            old.spd_device = module.spd_device;
                        } else {
                            modules.push(module);
                        }
                    }
                    Err(error) => properties.push(property(
                        &format!("spd_error_{}", entry.path.display()),
                        &format!("SPD [{}]", entry.path.display()),
                        Some(reason(&error)),
                    )),
                }
            }
        }
        Err(status) => properties.push(property(
            "spd_devices",
            "Сведения SPD",
            Some(reason(&failed(status))),
        )),
    }
    properties.push(property("active_timings", "Текущие тайминги контроллера", Some("недоступно через стандартные интерфейсы Linux; SPD описывает паспортные JEDEC-параметры".into())));
    Inventory {
        modules,
        properties,
    }
}

fn temperature(raw: &RawMemoryTemperature) -> MetricValue {
    for (value, optional) in [(&raw.fault, true), (&raw.enabled, false)] {
        match value {
            Ok(value) if value.trim() == if optional { "0" } else { "1" } => {}
            Ok(value) if !optional && value.trim() == "0" => return MetricValue::NotAvailable,
            Ok(_) => return MetricValue::InvalidData,
            Err(InterfaceStatus::NotAvailable) => {}
            Err(status) => return failed(status),
        }
    }
    match &raw.input {
        Ok(value) => match value.trim().parse::<i64>() {
            Ok(value) if (-273_150..=250_000).contains(&value) => {
                MetricValue::Available(value as f64 / 1000.0)
            }
            _ => MetricValue::InvalidData,
        },
        Err(status) => failed(status),
    }
}

pub fn readings(
    inventory: &Inventory,
    raw: &RawMemoryDetails,
) -> (Vec<DeviceReading>, Vec<Property>) {
    let (mut readings, status) = swap::readings(raw);
    for module in &inventory.modules {
        let sensor = raw
            .temperatures
            .iter()
            .find(|sensor| module.spd_device.as_ref() == Some(&sensor.device));
        readings.push(DeviceReading {
            device: module.device.clone(),
            metrics: vec![Metric {
                id: "temperature".into(),
                label: "Температура модуля (датчик привязан по sysfs)".into(),
                unit: Unit::Celsius,
                value: sensor.map(temperature).unwrap_or(MetricValue::NotAvailable),
            }],
        });
    }
    for sensor in &raw.temperatures {
        if inventory
            .modules
            .iter()
            .any(|module| module.spd_device.as_ref() == Some(&sensor.device))
        {
            continue;
        }
        readings.push(DeviceReading {
            device: Device {
                id: format!("ram/temperature/{}", sensor.path.display()),
                parent_id: Some("ram".into()),
                kind: DeviceKind::MemoryTemperature,
                name: format!(
                    "Датчик памяти [{}; {}]",
                    sensor.driver,
                    sensor.device.display()
                ),
                properties: vec![property("association", "Привязка к слоту SMBIOS", None)],
            },
            metrics: vec![Metric {
                id: "temperature".into(),
                label: "Температура памяти".into(),
                unit: Unit::Celsius,
                value: temperature(sensor),
            }],
        });
    }
    let mut properties = vec![property("swap_details", "Детализация swap", Some(status))];
    if raw.temperatures.is_empty() {
        properties.push(property(
            "memory_temperature",
            "Температуры модулей",
            Some("нет подтверждённого датчика jc42/spd5118".into()),
        ));
    }
    (readings, properties)
}
