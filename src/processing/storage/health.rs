//! Report the drive's own SMART assessment; do not infer "healthy" from no data.
use super::*;
use serde_json::Value;

fn temp(value: Option<&Value>) -> MetricValue {
    match value.and_then(Value::as_f64) {
        Some(n) if n.is_finite() && (-273.15..=250.0).contains(&n) => MetricValue::Available(n),
        Some(_) => MetricValue::InvalidData,
        None => MetricValue::NotAvailable,
    }
}

pub(super) fn readings(raw: &RawBlock) -> (Vec<Metric>, Vec<Property>, MetricValue) {
    let mut metrics = Vec::new();
    let mut properties = Vec::new();
    let mut fallback_temp = MetricValue::NotAvailable;
    let mut failed_flag = MetricValue::NotSupported;
    let assessment;
    match &raw.smart {
        Err(status) => {
            failed_flag = failed(status);
            assessment = format!(
                "{}; smartctl необязателен, нужны разрешения чтения устройства",
                reason(&failed_flag)
            );
        }
        Ok(data) => {
            let messages = data["smartctl"]["messages"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item["string"].as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            let lower = messages.to_ascii_lowercase();
            let exit = data["smartctl"]["exit_status"].as_u64().unwrap_or(0);
            if lower.contains("standby") && data["smart_status"]["passed"].as_bool().is_none() {
                failed_flag = MetricValue::NotAvailable;
                assessment = "опрос пропущен: накопитель в standby; диск не разбужен".into();
            } else if exit == 5
                && lower.contains("power mode")
                && data["smart_status"]["passed"].as_bool().is_none()
            {
                failed_flag = MetricValue::NotSupported;
                assessment = "опрос пропущен: проверка режима питания ATA не поддерживается".into();
            } else if exit & 3 != 0 {
                failed_flag = if lower.contains("permission denied")
                    || lower.contains("operation not permitted")
                {
                    MetricValue::PermissionDenied
                } else if lower.contains("no such") {
                    MetricValue::NotAvailable
                } else {
                    MetricValue::ReadError
                };
                assessment = if messages.is_empty() {
                    reason(&failed_flag)
                } else {
                    messages
                };
            } else {
                assessment = match data["smart_status"]["passed"].as_bool() {
                    Some(passed) => {
                        failed_flag = MetricValue::Available(if passed { 0.0 } else { 1.0 });
                        if passed {
                            "PASSED — пороговая оценка накопителя; не гарантия исправности".into()
                        } else {
                            "FAILED — накопитель сообщает о неисправности".into()
                        }
                    }
                    None => "SMART-оценка недоступна или не поддерживается".into(),
                };
                fallback_temp = temp(data.get("temperature").and_then(|t| t.get("current")));
                if let Some(log) = data.get("nvme_smart_health_information_log") {
                    properties.push(property(
                        "smart_scope",
                        "Область SMART NVMe",
                        Some("контроллер; может быть общей для нескольких namespaces".into()),
                    ));
                    if let Some(warnings) = log["critical_warning"].as_u64() {
                        properties.push(property(
                            "nvme_warning_bits",
                            "NVMe critical_warning (маска)",
                            Some(format!("0x{warnings:02x}")),
                        ));
                        metrics.push(metric(
                            "nvme_warning",
                            "NVMe критическое предупреждение",
                            Unit::Boolean,
                            MetricValue::Available(if warnings == 0 { 0.0 } else { 1.0 }),
                        ));
                    }
                    for (key, label, unit, maximum) in [
                        (
                            "percentage_used",
                            "Износ NVMe (оценка; может быть >100%)",
                            Unit::Percent,
                            255,
                        ),
                        (
                            "available_spare",
                            "NVMe доступный резерв",
                            Unit::Percent,
                            100,
                        ),
                        (
                            "available_spare_threshold",
                            "NVMe порог резерва",
                            Unit::Percent,
                            100,
                        ),
                        (
                            "media_errors",
                            "NVMe ошибки носителя",
                            Unit::Count,
                            u64::MAX,
                        ),
                        (
                            "unsafe_shutdowns",
                            "NVMe небезопасные выключения",
                            Unit::Count,
                            u64::MAX,
                        ),
                    ] {
                        if let Some(value) = log.get(key) {
                            let value = value
                                .as_u64()
                                .filter(|n| *n <= maximum)
                                .map(|n| MetricValue::Available(n as f64))
                                .unwrap_or(MetricValue::InvalidData);
                            metrics.push(metric(key, label, unit, value));
                        }
                    }
                }
                if let Some(table) = data["ata_smart_attributes"]["table"].as_array() {
                    for entry in table {
                        let label = match (entry["id"].as_u64(), entry["name"].as_str()) {
                            (Some(5), Some("Reallocated_Sector_Ct")) => {
                                "ATA переназначенные секторы"
                            }
                            (Some(197), Some("Current_Pending_Sector")) => {
                                "ATA ожидающие переназначения секторы"
                            }
                            (Some(198), Some("Offline_Uncorrectable")) => {
                                "ATA неисправимые секторы (offline)"
                            }
                            _ => continue,
                        };
                        let value = entry["raw"]["value"]
                            .as_u64()
                            .map(|n| MetricValue::Available(n as f64))
                            .unwrap_or(MetricValue::InvalidData);
                        metrics.push(metric(
                            &format!("ata_{}", entry["id"]),
                            label,
                            Unit::Count,
                            value,
                        ));
                    }
                }
                if exit & 4 != 0 {
                    properties.push(property(
                        "smart_partial",
                        "SMART: часть команд",
                        Some("ошибка чтения; доступные поля показаны отдельно".into()),
                    ));
                }
            }
        }
    }
    metrics.insert(
        0,
        metric(
            "smart_failed",
            "SMART сообщает о неисправности",
            Unit::Boolean,
            failed_flag,
        ),
    );
    properties.push(property(
        "smart_assessment",
        "Состояние здоровья SMART",
        Some(assessment),
    ));
    properties.push(property(
        "smart_age",
        "Возраст SMART (опрос не чаще 60 с)",
        Some(format!(
            "{:.0} с",
            raw.stat_at
                .checked_duration_since(raw.smart_at)
                .unwrap_or_default()
                .as_secs_f64()
        )),
    ));
    (metrics, properties, fallback_temp)
}
