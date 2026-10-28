//! Shared display units and states for CLI and GUI.
use crate::model::{Metric, MetricValue, Unit};

pub(super) fn metric(metric: &Metric) -> String {
    let unit = match metric.unit {
        Unit::Byte => "B",
        Unit::BytePerSecond => "B/s",
        Unit::Count => "шт.",
        Unit::GigatransfersPerSecond => "GT/s",
        Unit::Boolean => "",
        Unit::Percent => "%",
        Unit::Megahertz => "МГц",
        Unit::Celsius => "°C",
        Unit::Watt => "Вт",
        Unit::Rpm => "об/мин",
    };
    match metric.value {
        MetricValue::Available(value)
            if matches!(metric.unit, Unit::Byte | Unit::BytePerSecond) =>
        {
            if !value.is_finite() || value < 0.0 {
                return "некорректные данные".into();
            }
            for (suffix, factor) in [
                ("TiB", 1024_f64.powi(4)),
                ("GiB", 1024_f64.powi(3)),
                ("MiB", 1024_f64.powi(2)),
                ("KiB", 1024.0),
            ] {
                if value >= factor {
                    return format!(
                        "{:.2} {suffix}{}",
                        value / factor,
                        if metric.unit == Unit::BytePerSecond {
                            "/s"
                        } else {
                            ""
                        }
                    );
                }
            }
            format!("{value:.0} {unit}")
        }
        MetricValue::Available(value)
            if matches!(metric.unit, Unit::Count | Unit::Rpm)
                && value.is_finite()
                && value >= 0.0 =>
        {
            format!("{value:.0} {unit}")
        }
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
