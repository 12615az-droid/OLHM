//! Shared display units and states for CLI and GUI.
use crate::model::{Metric, MetricValue, Unit};

pub(super) fn metric(metric: &Metric) -> String {
    let unit = match metric.unit {
        Unit::Boolean => "",
        Unit::Percent => "%",
        Unit::Megahertz => "МГц",
        Unit::Celsius => "°C",
        Unit::Watt => "Вт",
    };
    match metric.value {
        MetricValue::Available(value) if metric.unit == Unit::Boolean && value == 0.0 => {
            "нет".into()
        }
        MetricValue::Available(value) if metric.unit == Unit::Boolean && value == 1.0 => {
            "да".into()
        }
        MetricValue::Available(value) if value.is_finite() => format!("{value:.1} {unit}"),
        MetricValue::Available(_) | MetricValue::InvalidData => "некорректные данные".into(),
        MetricValue::Waiting => "ожидание второго замера".into(),
        MetricValue::NotAvailable => "недоступно".into(),
        MetricValue::NotSupported => "не поддерживается или нет подтверждённого источника".into(),
        MetricValue::PermissionDenied => "нет доступа".into(),
        MetricValue::ReadError => "ошибка чтения".into(),
        MetricValue::CounterReset => "счётчик уменьшился; повторный замер".into(),
        MetricValue::NoProgress => "нет изменения счётчиков".into(),
        MetricValue::Offline => "отключён".into(),
    }
}
