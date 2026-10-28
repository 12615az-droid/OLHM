//! Normalize block, filesystem and SMART readings into the shared device model.
use crate::{error::InterfaceStatus, model::*, raw::storage::*};
use std::collections::BTreeMap;
mod filesystems;
mod health;
#[cfg(test)]
mod tests;

fn failed(status: &InterfaceStatus) -> MetricValue {
    match status {
        InterfaceStatus::PermissionDenied(_) => MetricValue::PermissionDenied,
        InterfaceStatus::ParseError(_) => MetricValue::InvalidData,
        InterfaceStatus::ReadError(_) => MetricValue::ReadError,
        _ => MetricValue::NotAvailable,
    }
}

fn reason(value: &MetricValue) -> String {
    match value {
        MetricValue::PermissionDenied => "нет доступа",
        MetricValue::InvalidData => "некорректные данные",
        MetricValue::ReadError => "ошибка чтения",
        MetricValue::NotSupported => "не поддерживается",
        _ => "недоступно",
    }
    .into()
}

fn property(id: &str, label: &str, value: Option<String>) -> Property {
    Property {
        id: id.into(),
        label: label.into(),
        value,
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
fn number(value: &str) -> Result<u64, MetricValue> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|n| n.is_ascii_digit()) {
        return Err(MetricValue::InvalidData);
    }
    value.parse().map_err(|_| MetricValue::InvalidData)
}
fn counter(raw: &RawBlock, key: &str) -> Result<u64, MetricValue> {
    number(
        raw.attrs
            .get(key)
            .ok_or(MetricValue::NotAvailable)?
            .as_ref()
            .map_err(failed)?,
    )
}
fn attr(raw: &RawBlock, key: &str) -> Option<String> {
    raw.attrs
        .get(key)?
        .as_ref()
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}
fn bytes(value: Result<u64, MetricValue>) -> MetricValue {
    value
        .map(|n| MetricValue::Available(n as f64))
        .unwrap_or_else(|error| error)
}
fn capacity(value: u64) -> String {
    format!("{:.2} GiB ({value} байт)", value as f64 / 1024_f64.powi(3))
}
fn id(raw: &RawBlock) -> String {
    format!("storage/block/{}", raw.identity)
}

pub fn device() -> Device {
    Device { id: "storage".into(), parent_id: None, kind: DeviceKind::Storage, name: "Накопители и файловые системы".into(), properties: vec![
        property("accounting", "Учёт объёмов", Some("диски, разделы и ФС показаны отдельно; их объёмы и I/O не суммируются".into())),
        property("fs_scope", "Область файловых систем", Some("точки монтирования текущего процесса; одна ФС может охватывать несколько устройств".into())),
        property("health_policy", "SMART", Some("read-only; опрос не чаще 60 с; нет данных не означает исправность".into())),
    ] }
}

fn io(raw: &RawBlock, previous: Option<&RawBlock>) -> (MetricValue, MetricValue) {
    let parse = |raw: &RawBlock| -> Result<(u64, u64), MetricValue> {
        let fields = raw
            .stat
            .as_ref()
            .map_err(failed)?
            .split_whitespace()
            .map(number)
            .collect::<Result<Vec<_>, _>>()?;
        if fields.len() < 7 {
            return Err(MetricValue::InvalidData);
        }
        Ok((fields[2], fields[6]))
    };
    let measure = || -> Result<(f64, f64), MetricValue> {
        let current = parse(raw)?;
        let old = previous
            .filter(|old| old.identity == raw.identity)
            .ok_or(MetricValue::Waiting)?;
        let previous = parse(old).map_err(|_| MetricValue::Waiting)?;
        let interval = raw
            .stat_at
            .checked_duration_since(old.stat_at)
            .filter(|time| !time.is_zero())
            .ok_or(MetricValue::InvalidData)?
            .as_secs_f64();
        let read = current
            .0
            .checked_sub(previous.0)
            .ok_or(MetricValue::CounterReset)?;
        let written = current
            .1
            .checked_sub(previous.1)
            .ok_or(MetricValue::CounterReset)?;
        // Linux diskstats sectors are 512 bytes, regardless of logical block size.
        Ok((
            read as f64 * 512.0 / interval,
            written as f64 * 512.0 / interval,
        ))
    };
    match measure() {
        Ok((read, write)) => (MetricValue::Available(read), MetricValue::Available(write)),
        Err(reason) => (reason.clone(), reason),
    }
}

fn block(raw: &RawBlock, previous: Option<&RawBlock>, blocks: &[RawBlock]) -> DeviceReading {
    let parent = raw
        .parent
        .as_ref()
        .and_then(|name| blocks.iter().find(|block| &block.name == name));
    let size = counter(raw, "size")
        .and_then(|sectors| sectors.checked_mul(512).ok_or(MetricValue::InvalidData));
    let mut properties = vec![
        property("path", "Устройство", Some(format!("/dev/{}", raw.name))),
        property(
            "size",
            "Ёмкость устройства",
            size.clone().ok().map(capacity),
        ),
        property("dev", "Major:minor", attr(raw, "dev")),
        property(
            "parent",
            "Родительский накопитель",
            raw.parent.as_ref().map(|name| format!("/dev/{name}")),
        ),
        property(
            "model",
            "Модель",
            raw.metadata
                .get("model")
                .cloned()
                .or_else(|| attr(raw, "device/model")),
        ),
        property(
            "vendor",
            "Vendor (из lsblk/sysfs)",
            raw.metadata
                .get("vendor")
                .cloned()
                .or_else(|| attr(raw, "device/vendor")),
        ),
        property(
            "serial",
            "Серийный номер",
            raw.metadata
                .get("serial")
                .cloned()
                .or_else(|| attr(raw, "device/serial")),
        ),
        property(
            "firmware",
            "Прошивка",
            attr(raw, "device/firmware_rev")
                .or_else(|| raw.metadata.get("rev").cloned())
                .or_else(|| attr(raw, "device/rev")),
        ),
        property(
            "state",
            "Состояние драйвера (не SMART)",
            attr(raw, "device/state"),
        ),
        property(
            "read_only",
            "Только чтение",
            attr(raw, "ro").and_then(|n| match n.as_str() {
                "0" => Some("нет".into()),
                "1" => Some("да".into()),
                _ => None,
            }),
        ),
    ];
    if raw.partition {
        properties.push(property(
            "start",
            "Начало раздела",
            counter(raw, "start")
                .ok()
                .and_then(|n| n.checked_mul(512))
                .map(|n| format!("{n} байт")),
        ));
        properties.push(property(
            "partition_number",
            "Номер раздела",
            attr(raw, "partition"),
        ));
    } else {
        let nvme = raw.path.components().any(|s| s.as_os_str() == "nvme");
        let kind = if nvme {
            Some("NVMe".into())
        } else {
            attr(raw, "queue/rotational").and_then(|n| match n.as_str() {
                "1" => Some("вращающийся носитель (HDD)".into()),
                "0" => Some("невращающийся носитель (SSD/flash либо виртуальный)".into()),
                _ => None,
            })
        };
        properties.push(property("media", "Тип носителя / интерфейс", kind));
        properties.push(property(
            "transport",
            "Транспорт",
            raw.metadata
                .get("tran")
                .cloned()
                .or_else(|| attr(raw, "device/transport")),
        ));
    }
    for (key, label) in [
        ("type", "Тип блочного устройства (lsblk)"),
        ("pttype", "Таблица разделов"),
        ("parttype", "Тип раздела (GUID/код)"),
        ("partuuid", "PARTUUID"),
        ("fstype", "Файловая система / сигнатура"),
        ("label", "Метка ФС"),
        ("uuid", "UUID ФС"),
    ] {
        properties.push(property(key, label, raw.metadata.get(key).cloned()));
    }
    if !raw.partition {
        let slaves_path = attr(raw, "slaves");
        if slaves_path.is_some() {
            properties.push(property("slaves", "Базовые устройства", slaves_path));
        }
    }
    let (read, write) = io(raw, previous);
    let mut metrics = vec![
        metric("capacity", "Размер", Unit::Byte, bytes(size)),
        metric("read", "Чтение", Unit::BytePerSecond, read),
        metric("write", "Запись", Unit::BytePerSecond, write),
    ];
    if !raw.partition {
        let (health, health_properties, smart_temp) = health::readings(raw);
        properties.extend(health_properties);
        let temperature = match &raw.temperature {
            Ok(text) => match text.trim().parse::<i64>() {
                Ok(n) if (-273_150..=250_000).contains(&n) => {
                    MetricValue::Available(n as f64 / 1000.0)
                }
                _ => MetricValue::InvalidData,
            },
            Err(InterfaceStatus::NotAvailable) => smart_temp,
            Err(error) => failed(error),
        };
        properties.push(property(
            "temperature_source",
            "Источник температуры",
            if matches!(&raw.temperature, Err(InterfaceStatus::NotAvailable))
                && matches!(temperature, MetricValue::Available(_))
            {
                Some("smartctl; кеш не более частоты опроса SMART".into())
            } else {
                raw.temperature_source.clone()
            },
        ));
        metrics.push(metric(
            "temperature",
            "Температура накопителя / контроллера",
            Unit::Celsius,
            temperature,
        ));
        metrics.extend(health);
    }
    DeviceReading {
        device: Device {
            id: id(raw),
            parent_id: Some(parent.map(id).unwrap_or_else(|| "storage".into())),
            kind: if raw.partition {
                DeviceKind::Partition
            } else {
                DeviceKind::BlockDevice
            },
            name: format!(
                "{} /dev/{}",
                if raw.partition {
                    "Раздел"
                } else {
                    "Накопитель"
                },
                raw.name
            ),
            properties,
        },
        metrics,
    }
}

#[derive(Default)]
pub struct StorageMonitor {
    previous: Option<RawStorageSample>,
}

impl StorageMonitor {
    pub fn update(&mut self, raw: RawStorageSample) -> MonitorSnapshot {
        let interval = self
            .previous
            .as_ref()
            .and_then(|old| raw.at.checked_duration_since(old.at));
        let blocks = raw.blocks.as_ref().map(Vec::as_slice).unwrap_or_default();
        let mut root = device();
        root.properties.push(property(
            "discovery",
            "Обнаружение устройств",
            Some(match &raw.blocks {
                Ok(blocks) => format!("{} блочных устройств/разделов", blocks.len()),
                Err(error) => reason(&failed(error)),
            }),
        ));
        root.properties.push(property(
            "metadata",
            "Метаданные разделов и несмонтированных ФС (lsblk)",
            Some(
                raw.metadata_status
                    .as_ref()
                    .map(|_| "доступны".into())
                    .unwrap_or_else(|error| reason(&failed(error))),
            ),
        ));
        root.properties.push(property(
            "mounts",
            "Точки монтирования",
            Some(match &raw.mounts {
                Ok(mounts) => format!(
                    "{} записей; одинаковые привязки сгруппированы",
                    mounts.len()
                ),
                Err(error) => reason(&failed(error)),
            }),
        ));
        let mut devices = vec![DeviceReading {
            device: root,
            metrics: Vec::new(),
        }];
        for current in blocks {
            let previous = self
                .previous
                .as_ref()
                .and_then(|old| old.blocks.as_ref().ok())
                .and_then(|blocks| blocks.iter().find(|old| old.identity == current.identity));
            devices.push(block(current, previous, blocks));
        }
        if let Ok(mounts) = &raw.mounts {
            devices.extend(filesystems::readings(mounts, blocks));
        }
        self.previous = Some(raw);
        MonitorSnapshot { interval, devices }
    }
}
