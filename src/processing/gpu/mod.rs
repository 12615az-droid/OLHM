//! Shared GPU model: validation, unit conversion and conservative derived values.
use crate::{model::*, raw::gpu::*};
mod amd;
#[cfg(test)]
mod tests;

fn failure(e: &GpuError) -> MetricValue {
    match e {
        GpuError::NotSupported => MetricValue::NotSupported,
        GpuError::NotAvailable => MetricValue::NotAvailable,
        GpuError::PermissionDenied => MetricValue::PermissionDenied,
        GpuError::ReadError(_) => MetricValue::ReadError,
        GpuError::InvalidData => MetricValue::InvalidData,
    }
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

pub fn device() -> Device {
    Device {id:"gpu".into(),parent_id:None,kind:DeviceKind::Graphics,name:"Видеокарты (GPU)".into(),properties:vec![
        property("scope","Достоверность",Some("только доступные показатели драйвера; отсутствие датчика не означает нулевое значение".into())),
        property("memory_policy","Память",Some("VRAM, UMA-пул и GTT/разделяемая RAM не суммируются; BAR не считается объёмом VRAM".into())),
    ]}
}

fn value(raw: &RawGpu, id: &str) -> MetricValue {
    let Some(raw) = raw.values.get(id) else {
        return MetricValue::NotSupported;
    };
    let s = match &raw.value {
        Ok(s) => s,
        Err(e) => return failure(e),
    };
    let numeric = if raw.unit == GpuUnit::PcieSpeed {
        let parts: Vec<_> = s.split_whitespace().collect();
        if !matches!(parts.as_slice(), [_, "GT/s"] | [_, "GT/s", "PCIe"]) {
            return MetricValue::NotAvailable;
        }
        parts[0]
    } else {
        s.trim()
    };
    let Ok(mut n) = numeric.parse::<f64>() else {
        return MetricValue::InvalidData;
    };
    if !n.is_finite() {
        return MetricValue::InvalidData;
    }
    n *= match raw.unit {
        GpuUnit::MilliCelsius | GpuUnit::MilliWatt => 0.001,
        GpuUnit::MicroWatt => 0.000001,
        GpuUnit::Hertz => 0.000001,
        GpuUnit::Pwm => 100.0 / 255.0,
        _ => 1.0,
    };
    let valid = match raw.unit {
        GpuUnit::PcieSpeed => n > 0.0 && n <= 256.0,
        GpuUnit::PcieWidth => n.fract() == 0.0 && matches!(n as u32, 1 | 2 | 4 | 8 | 12 | 16 | 32),
        GpuUnit::Celsius | GpuUnit::MilliCelsius => (-100.0..=250.0).contains(&n),
        GpuUnit::Percent if id.ends_with("_percent") => (0.0..=1000.0).contains(&n),
        GpuUnit::Percent | GpuUnit::Pwm => (0.0..=100.0).contains(&n),
        GpuUnit::Boolean => n == 0.0 || n == 1.0,
        GpuUnit::Megahertz | GpuUnit::Hertz => (0.0..=100000.0).contains(&n),
        GpuUnit::Watt | GpuUnit::MilliWatt | GpuUnit::MicroWatt => (0.0..=100000.0).contains(&n),
        GpuUnit::Rpm => (0.0..=100000.0).contains(&n),
        GpuUnit::Byte | GpuUnit::MicroJoule | GpuUnit::Bits => n >= 0.0,
    };
    if valid {
        MetricValue::Available(n)
    } else {
        MetricValue::InvalidData
    }
}
fn number(value: &MetricValue) -> Result<f64, MetricValue> {
    match value {
        MetricValue::Available(n) => Ok(*n),
        e => Err(e.clone()),
    }
}
fn derived(result: Result<f64, MetricValue>) -> MetricValue {
    result.map(MetricValue::Available).unwrap_or_else(|e| e)
}

fn engine(
    current: &Result<RawEngineCounter, GpuError>,
    old: Option<&Result<RawEngineCounter, GpuError>>,
) -> MetricValue {
    derived((|| {
        let current = current.as_ref().map_err(failure)?;
        let old = old
            .and_then(|v| v.as_ref().ok())
            .ok_or(MetricValue::Waiting)?;
        let busy = current
            .busy
            .checked_sub(old.busy)
            .ok_or(MetricValue::CounterReset)?;
        let time = current
            .total
            .checked_sub(old.total)
            .ok_or(MetricValue::CounterReset)?;
        if time == 0 {
            return Err(MetricValue::NoProgress);
        }
        if busy > time {
            return Err(MetricValue::InvalidData);
        }
        Ok(busy as f64 / time as f64 * 100.0)
    })())
}

fn reading(
    mut raw: RawGpu,
    previous: Option<&RawGpu>,
    interval: Option<std::time::Duration>,
) -> DeviceReading {
    amd::augment(&mut raw);
    let meta = |id: &str| raw.metadata.get(id).cloned();
    let vendor = match meta("vendor_id").as_deref() {
        Some("0x1002" | "0x1022") => "AMD",
        Some("0x10de") => "NVIDIA",
        Some("0x8086") => "Intel",
        _ => "неизвестен",
    };
    let memory_kind = meta("memory_kind").unwrap_or_else(|| "unknown".into());
    let memory_label = match memory_kind.as_str() {
        "dedicated" => "VRAM",
        "shared" => "UMA-пул RAM (драйвер)",
        "framebuffer" => "Frame buffer (NVML)",
        _ => "Пул VRAM/UMA (тип не подтверждён)",
    };
    let mut properties = vec![
        property("model", "Модель GPU", meta("model")),
        property("vendor", "Производитель", Some(vendor.into())),
        property("pci", "PCI / аппаратный путь", meta("pci")),
        property("device_id", "PCI Device ID", meta("device_id")),
        property("driver", "Драйвер", meta("driver")),
        property("driver_version", "Версия драйвера", meta("driver_version")),
        property(
            "memory_kind",
            "Тип памяти",
            Some(
                match memory_kind.as_str() {
                    "dedicated" => "локальная VRAM, подтверждено DRM",
                    "shared" => "интегрированная графика; UMA использует RAM",
                    "framebuffer" => "frame buffer NVML; не BAR и не объём всей RAM",
                    _ => {
                        "не подтверждён; размер пула драйвера не доказывает наличие физической VRAM"
                    }
                }
                .into(),
            ),
        ),
    ];
    for (id, label) in [
        ("model_source", "Источник названия"),
        ("nvml", "NVML"),
        ("drm_status", "DRM query"),
        ("gpu_metrics", "AMD gpu_metrics"),
        ("power_sampling", "Окно мощности"),
    ] {
        if raw.metadata.contains_key(id) {
            properties.push(property(id, label, meta(id)));
        }
    }
    let mut metrics = Vec::new();
    for (id, label, unit) in [
        (
            "pcie_speed",
            "PCIe: текущая скорость линии",
            Unit::GigatransfersPerSecond,
        ),
        ("pcie_width", "PCIe: текущее число линий (x)", Unit::Count),
        (
            "pcie_max_speed",
            "PCIe: максимальная скорость устройства",
            Unit::GigatransfersPerSecond,
        ),
        (
            "pcie_max_width",
            "PCIe: максимальное число линий устройства (x)",
            Unit::Count,
        ),
        ("load", "GPU Load", Unit::Percent),
        (
            "memory_load",
            "Memory Controller Load (активность)",
            Unit::Percent,
        ),
        ("encode", "Video Encode", Unit::Percent),
        ("decode", "Video Decode", Unit::Percent),
        ("temperature", "GPU Temperature", Unit::Celsius),
        ("hotspot", "GPU Hot Spot", Unit::Celsius),
        (
            "memory_temperature",
            "Memory Junction / Memory Temperature",
            Unit::Celsius,
        ),
        ("core_clock", "Текущая частота ядра GPU", Unit::Megahertz),
        (
            "memory_clock",
            if memory_kind == "shared" {
                "Текущая частота памяти/UMC общей RAM (APU)"
            } else {
                "Текущая частота памяти GPU"
            },
            Unit::Megahertz,
        ),
        (
            "temperature_limit",
            "Temperature Limit (NVML GPU max operating)",
            Unit::Celsius,
        ),
    ] {
        metrics.push(metric(id, label, unit, value(&raw, id)));
    }
    for (id, label) in [
        (
            "temperature_critical",
            "GPU: критический порог (не активный троттлинг)",
        ),
        ("hotspot_critical", "Hot Spot: критический порог"),
        ("memory_temperature_critical", "Память: критический порог"),
    ] {
        if raw.values.contains_key(id) {
            metrics.push(metric(id, label, Unit::Celsius, value(&raw, id)));
        }
    }
    if raw.values.contains_key("media") {
        metrics.push(metric(
            "media",
            "Video encode/decode (совместная активность VCN/UVD)",
            Unit::Percent,
            value(&raw, "media"),
        ));
    }
    let total = value(&raw, "memory_total");
    let mut used = value(&raw, "memory_used");
    if matches!((&total,&used),(MetricValue::Available(total),MetricValue::Available(used)) if used>total)
    {
        used = MetricValue::InvalidData;
    }
    let mut free = if raw.values.contains_key("memory_free") {
        value(&raw, "memory_free")
    } else {
        derived((|| Ok(number(&total)? - number(&used)?))())
    };
    if matches!((&total,&used,&free),(MetricValue::Available(t),MetricValue::Available(u),MetricValue::Available(f)) if *t<=0.0 || f< &0.0 || u+f>t+1.0)
    {
        free = MetricValue::InvalidData;
    }
    let usage = derived((|| {
        let total = number(&total)?;
        if total <= 0.0 {
            return Err(MetricValue::InvalidData);
        }
        Ok(number(&used)? / total * 100.0)
    })());
    if let MetricValue::Available(total) = total {
        properties.push(property(
            "memory_capacity",
            "Объём памяти при опросе",
            Some(format!("{total:.0} байт ({memory_label})")),
        ));
    }
    for (id, label, unit, n) in [
        (
            "memory_total",
            format!("{memory_label}: всего"),
            Unit::Byte,
            total,
        ),
        (
            "memory_used",
            format!("{memory_label}: использовано"),
            Unit::Byte,
            used,
        ),
        (
            "memory_free",
            format!("{memory_label}: свободно (оценка драйвера)"),
            Unit::Byte,
            free,
        ),
        (
            "memory_usage",
            format!("{memory_label}: использование"),
            Unit::Percent,
            usage,
        ),
    ] {
        metrics.push(metric(id, &label, unit, n));
    }
    for (id, label) in [
        (
            "shared_total",
            "GTT / разделяемая RAM: бюджет драйвера (не отдельная VRAM)",
        ),
        ("shared_used", "GTT / разделяемая RAM: использовано"),
    ] {
        if raw.values.contains_key(id) {
            metrics.push(metric(id, label, Unit::Byte, value(&raw, id)));
        }
    }
    let scope = meta("power_scope").unwrap_or_else(|| "unknown".into());
    let power_label = match scope.as_str() {
        "board" => "Потребление видеокарты (GPU и связанная схема)",
        "chip" => "Потребление графического чипа",
        "soc" => "Потребление SoC/PPT (на APU включает CPU)",
        "gpu_package" => "Потребление GPU package",
        _ => "Мощность датчика GPU (область не подтверждена)",
    };
    let mut power = value(&raw, "power");
    if !raw.values.contains_key("power") && raw.values.contains_key("energy") {
        power = derived((|| {
            let current = raw.values["energy"]
                .value
                .as_ref()
                .map_err(failure)?
                .parse::<u64>()
                .map_err(|_| MetricValue::InvalidData)?;
            let old = previous
                .and_then(|p| p.values.get("energy"))
                .ok_or(MetricValue::Waiting)?;
            let old = old
                .value
                .as_ref()
                .map_err(|_| MetricValue::Waiting)?
                .parse::<u64>()
                .map_err(|_| MetricValue::Waiting)?;
            let measured_interval = raw
                .energy_at
                .zip(previous.and_then(|p| p.energy_at))
                .and_then(|(now, old)| now.checked_duration_since(old))
                .or(interval);
            let dt = measured_interval
                .filter(|d| !d.is_zero())
                .ok_or(MetricValue::Waiting)?
                .as_secs_f64();
            Ok(current.checked_sub(old).ok_or(MetricValue::CounterReset)? as f64 / 1e6 / dt)
        })());
    }
    let cap = value(&raw, "power_limit");
    let ratio = derived((|| {
        if scope == "unknown"
            || meta("power_limit_scope") != Some(scope.clone())
            || meta("power_domain").is_none()
            || meta("power_domain") != meta("power_limit_domain")
        {
            return Err(MetricValue::NotSupported);
        }
        let cap = number(&cap)?;
        if cap <= 0.0 {
            return Err(MetricValue::InvalidData);
        }
        Ok(number(&power)? / cap * 100.0)
    })());
    metrics.push(metric("power", power_label, Unit::Watt, power));
    metrics.push(metric(
        "power_limit",
        &format!("Power Limit ({scope})"),
        Unit::Watt,
        cap,
    ));
    metrics.push(metric(
        "power_usage",
        "Использование Power Limit (та же область мощности)",
        Unit::Percent,
        ratio,
    ));
    for id in ["thermal_throttle", "power_throttle"] {
        let mut state = value(&raw, id);
        if raw.values.contains_key("nvml_active_reasons") {
            let read_mask = |id: &str| -> Result<u64, MetricValue> {
                raw.values
                    .get(id)
                    .ok_or(MetricValue::NotSupported)?
                    .value
                    .as_ref()
                    .map_err(failure)?
                    .parse()
                    .map_err(|_| MetricValue::InvalidData)
            };
            state = match read_mask("nvml_active_reasons") {
                Err(error) => error,
                Ok(active) => {
                    let mask = if id == "thermal_throttle" { 0x60 } else { 0x84 };
                    if active & mask != 0 {
                        MetricValue::Available(1.0)
                    } else if active & 8 != 0 {
                        MetricValue::NotAvailable
                    } else {
                        match read_mask("nvml_supported_reasons") {
                            Ok(supported) if supported & mask == mask => {
                                MetricValue::Available(0.0)
                            }
                            Ok(_) => MetricValue::NotSupported,
                            Err(error) => error,
                        }
                    }
                }
            };
        }
        if matches!(state, MetricValue::NotSupported) {
            let matches_reason = |key: &str| {
                key.starts_with("gt")
                    && key.split_once("_throttle_").is_some_and(|(_, reason)| {
                        if id == "thermal_throttle" {
                            reason.ends_with("thermal")
                        } else {
                            matches!(
                                reason,
                                "pl1" | "pl2" | "pl4" | "psys_pl1" | "psys_pl2" | "pwrbrk"
                            )
                        }
                    })
            };
            let candidates: Vec<_> = raw
                .values
                .keys()
                .filter(|s| matches_reason(s))
                .map(|id| value(&raw, id))
                .collect();
            if candidates.contains(&MetricValue::Available(1.0)) {
                state = MetricValue::Available(1.0)
            } else if !candidates.is_empty()
                && meta("intel_gt_count")
                    .and_then(|n| n.parse::<usize>().ok())
                    .is_some_and(|count| {
                        (0..count).all(|gt| {
                            raw.values.keys().any(|key| {
                                key.starts_with(&format!("gt{gt}_")) && matches_reason(key)
                            })
                        })
                    })
                && candidates.iter().all(|s| *s == MetricValue::Available(0.0))
            {
                state = MetricValue::Available(0.0)
            } else if let Some(error) = candidates
                .into_iter()
                .find(|s| !matches!(s, MetricValue::Available(_)))
            {
                state = error
            }
        }
        metrics.push(metric(
            id,
            if id == "thermal_throttle" {
                "Thermal Throttling (подтверждённая причина драйвера)"
            } else {
                "Power Throttling (подтверждённая причина драйвера)"
            },
            Unit::Boolean,
            state,
        ));
    }
    let mut sources = Vec::new();
    for (id, raw_value) in &raw.values {
        if id.starts_with("fan") {
            let (unit, label) = if id.ends_with("_rpm") {
                (
                    Unit::Rpm,
                    format!("{}: скорость вентилятора", id.trim_end_matches("_rpm")),
                )
            } else if id.ends_with("_pwm") {
                (
                    Unit::Percent,
                    format!(
                        "{}: PWM duty (не измерение RPM %)",
                        id.trim_end_matches("_pwm")
                    ),
                )
            } else {
                (
                    Unit::Percent,
                    format!(
                        "{}: относительная скорость NVML (заданная)",
                        id.trim_end_matches("_percent")
                    ),
                )
            };
            metrics.push(metric(id, &label, unit, value(&raw, id)));
        } else if id.starts_with("media_vcn") {
            metrics.push(metric(
                id,
                &format!(
                    "AMD VCN {}: encode/decode (совместно)",
                    id.trim_start_matches("media_vcn")
                ),
                Unit::Percent,
                value(&raw, id),
            ));
        } else if id.starts_with("gt") && id.ends_with("_clock") {
            metrics.push(metric(
                id,
                &format!("{id}: текущая частота GT"),
                Unit::Megahertz,
                value(&raw, id),
            ));
        }
        sources.push(format!("{id}: {}", raw_value.source));
    }
    for (name, counter) in &raw.engines {
        metrics.push(metric(
            &format!("engine/{name}"),
            &format!("Intel engine {name} (отдельная активность, не общий GPU Load)"),
            Unit::Percent,
            engine(counter, previous.and_then(|p| p.engines.get(name))),
        ));
    }
    if !raw.engines.is_empty() {
        sources.push("Intel engine PMU: Δbusy / Δtotal; нужны разрешения perf_event".into());
    }
    properties.push(property(
        "sources",
        "Источники показателей",
        Some(sources.join("; ")),
    ));
    DeviceReading {
        device: Device {
            id: format!("gpu/{}", raw.identity),
            parent_id: Some("gpu".into()),
            kind: DeviceKind::Gpu,
            name: meta("model")
                .unwrap_or_else(|| format!("GPU {vendor} [{}]", meta("pci").unwrap_or_default())),
            properties,
        },
        metrics,
    }
}

#[derive(Default)]
pub struct GpuMonitor {
    previous: Option<RawGpuSample>,
}
impl GpuMonitor {
    pub fn update(&mut self, raw: RawGpuSample) -> MonitorSnapshot {
        let interval = self
            .previous
            .as_ref()
            .and_then(|p| raw.at.checked_duration_since(p.at));
        let mut root = device();
        root.properties.push(property(
            "discovery",
            "Обнаружение GPU",
            Some(match &raw.devices {
                Ok(gpus) => format!("{} устройств", gpus.len()),
                Err(e) => format!("{e:?}"),
            }),
        ));
        let mut devices = vec![DeviceReading {
            device: root,
            metrics: Vec::new(),
        }];
        if let Ok(gpus) = &raw.devices {
            for gpu in gpus {
                let previous = self
                    .previous
                    .as_ref()
                    .and_then(|p| p.devices.as_ref().ok())
                    .and_then(|v| v.iter().find(|p| p.identity == gpu.identity));
                devices.push(reading(gpu.clone(), previous, interval));
            }
        }
        self.previous = Some(raw);
        MonitorSnapshot { interval, devices }
    }
}
