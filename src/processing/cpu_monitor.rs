use crate::{
    error::InterfaceStatus,
    raw::{cpu::RawCpuCounters, cpu_monitor::*},
};
use std::{collections::BTreeMap, time::Duration};

pub use crate::model::MetricValue as CpuValue;

fn failed(status: &InterfaceStatus) -> CpuValue {
    match status {
        InterfaceStatus::PermissionDenied(_) => CpuValue::PermissionDenied,
        InterfaceStatus::ParseError(_) => CpuValue::InvalidData,
        InterfaceStatus::ReadError(_) => CpuValue::ReadError,
        _ => CpuValue::NotAvailable,
    }
}

fn frequency(raw: &Result<String, InterfaceStatus>) -> CpuValue {
    match raw {
        Ok(value) => match value.trim().parse::<u64>() {
            Ok(khz) if khz > 0 => CpuValue::Available(khz as f64 / 1000.0),
            _ => CpuValue::InvalidData,
        },
        Err(status) => failed(status),
    }
}

/// Linux guest/guest_nice are included in user/nice. Only the first eight
/// columns enter the total; idle and iowait count as non-busy time.
fn usage(previous: Option<&RawCpuCounters>, current: Option<&RawCpuCounters>) -> CpuValue {
    let Some(current) = current else {
        return CpuValue::NotAvailable;
    };
    let Some(previous) = previous else {
        return CpuValue::Waiting;
    };
    let count = current.counters.len().min(8);
    if count < 4 || count != previous.counters.len().min(8) {
        return CpuValue::InvalidData;
    }
    let mut total = 0_u128;
    let mut idle = 0_u128;
    for index in 0..count {
        let Some(delta) = current.counters[index].checked_sub(previous.counters[index]) else {
            // iowait can decrease too: invalidate this interval and rebaseline.
            return CpuValue::CounterReset;
        };
        total += u128::from(delta);
        if index == 3 || index == 4 {
            idle += u128::from(delta);
        }
    }
    if total == 0 {
        return CpuValue::NoProgress;
    }
    CpuValue::Available((total - idle) as f64 / total as f64 * 100.0)
}

#[derive(Debug)]
pub struct CpuThreadMetrics {
    pub usage: CpuValue,
    pub frequency_mhz: CpuValue,
}

#[derive(Debug)]
pub struct CpuTemperature {
    pub id: String,
    pub source: String,
    pub label: String,
    pub celsius: CpuValue,
    pub critical_celsius: CpuValue,
}

#[derive(Debug)]
pub struct CpuMetrics {
    pub interval: Option<Duration>,
    pub usage: CpuValue,
    pub throttling: CpuValue,
    pub cpus: BTreeMap<u32, CpuThreadMetrics>,
    pub temperatures: Vec<CpuTemperature>,
    pub power: Vec<CpuPower>,
}

#[derive(Debug)]
pub struct CpuPower {
    pub source: String,
    pub domain: String,
    pub watts: CpuValue,
    pub averaged: bool,
}

fn unsigned(raw: &Result<String, InterfaceStatus>) -> Result<u64, CpuValue> {
    match raw {
        Ok(text) => text.trim().parse().map_err(|_| CpuValue::InvalidData),
        Err(status) => Err(failed(status)),
    }
}

pub fn supports_temperature(driver: &str) -> bool {
    matches!(driver, "coretemp" | "k10temp")
}

pub fn supports_power(zone: &str, domain: &str) -> bool {
    let numeric = |id: &str| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
    // Only documented RAPL CPU-package zones are attributed to the CPU.
    // Subdomains (cores/uncore/DRAM) and platform (psys) must not be added.
    zone.strip_prefix("intel-rapl:")
        .or_else(|| zone.strip_prefix("intel-rapl-mmio:"))
        .is_some_and(numeric)
        && domain.strip_prefix("package-").is_some_and(numeric)
}

fn energy(raw: &RawCpuPower) -> Result<u64, CpuValue> {
    let value = unsigned(&raw.energy_uj)?;
    match &raw.max_energy_range_uj {
        Err(InterfaceStatus::NotAvailable) => {}
        range => {
            let range = unsigned(range)?;
            if range == 0 || value > range {
                return Err(CpuValue::InvalidData);
            }
        }
    }
    Ok(value)
}

fn power(raw: &RawCpuPower, old: Option<&RawCpuPower>, boot_changed: bool) -> CpuValue {
    if !matches!(raw.power_uw, Err(InterfaceStatus::NotAvailable)) {
        return match unsigned(&raw.power_uw) {
            Ok(microwatts) => CpuValue::Available(microwatts as f64 / 1_000_000.0),
            Err(reason) => reason,
        };
    }
    let current = match energy(raw) {
        Ok(value) => value,
        Err(reason) => return reason,
    };
    let Some(old) = old else {
        return CpuValue::Waiting;
    };
    if boot_changed {
        return CpuValue::CounterReset;
    }
    if old.max_energy_range_uj != raw.max_energy_range_uj {
        return CpuValue::Waiting;
    }
    let previous = match energy(old) {
        Ok(value) => value,
        Err(_) => return CpuValue::Waiting,
    };
    let Some(elapsed) = raw
        .at
        .checked_duration_since(old.at)
        .filter(|time| !time.is_zero())
    else {
        return CpuValue::InvalidData;
    };
    // A reset and a wrap cannot be distinguished reliably from two readings.
    // Rebaseline instead of turning a reset into a fabricated power spike.
    let Some(delta) = current.checked_sub(previous) else {
        return CpuValue::CounterReset;
    };
    CpuValue::Available(delta as f64 / 1_000_000.0 / elapsed.as_secs_f64())
}

fn temperature(raw: &RawCpuTemperature) -> CpuValue {
    match &raw.fault {
        Ok(fault) if fault.trim() == "0" => {}
        Ok(_) => return CpuValue::InvalidData,
        Err(InterfaceStatus::NotAvailable) => {}
        Err(status) => return failed(status),
    }
    match &raw.input {
        Ok(value) => match value.trim().parse::<i64>() {
            Ok(milli) if (-273_150..=250_000).contains(&milli) => {
                CpuValue::Available(milli as f64 / 1000.0)
            }
            _ => CpuValue::InvalidData,
        },
        Err(status) => failed(status),
    }
}

fn millidegrees(raw: &Result<String, InterfaceStatus>) -> CpuValue {
    match raw {
        Ok(value) => match value.trim().parse::<i64>() {
            Ok(milli) if (-273_150..=250_000).contains(&milli) => {
                CpuValue::Available(milli as f64 / 1000.0)
            }
            _ => CpuValue::InvalidData,
        },
        Err(status) => failed(status),
    }
}

fn throttling(sample: &RawCpuSample, previous: Option<&RawCpuSample>) -> CpuValue {
    let mut has_source = false;
    let mut comparable = false;
    let mut fallback = None;
    for temperature in sample
        .temperatures
        .iter()
        .filter(|raw| supports_temperature(&raw.driver))
    {
        match unsigned(&temperature.critical_alarm) {
            Ok(1) => return CpuValue::Available(1.0),
            Ok(0) => {
                has_source = true;
                comparable = true;
            }
            Ok(_) => fallback = Some(CpuValue::InvalidData),
            Err(CpuValue::NotAvailable) => {}
            Err(reason) => fallback = Some(reason),
        }
    }
    for (&cpu_id, cpu) in &sample.cpus {
        for (name, current) in &cpu.throttle_counts {
            has_source = true;
            let current = match unsigned(current) {
                Ok(value) => value,
                Err(reason) => {
                    fallback = Some(reason);
                    continue;
                }
            };
            let Some(old) = previous
                .and_then(|sample| sample.cpus.get(&cpu_id))
                .and_then(|cpu| cpu.throttle_counts.get(name))
            else {
                continue;
            };
            let old = match unsigned(old) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if current > old {
                return CpuValue::Available(1.0);
            }
            if current < old {
                fallback = Some(CpuValue::CounterReset);
                continue;
            }
            comparable = true;
        }
    }
    if let Some(reason) = fallback {
        reason
    } else if comparable {
        CpuValue::Available(0.0)
    } else if has_source {
        CpuValue::Waiting
    } else {
        CpuValue::NotSupported
    }
}

#[derive(Default)]
pub struct CpuMonitor {
    previous: Option<RawCpuSample>,
}

impl CpuMonitor {
    pub fn update(&mut self, sample: RawCpuSample) -> CpuMetrics {
        let previous = self.previous.as_ref();
        let interval = previous
            .and_then(|p| sample.at.checked_duration_since(p.at))
            .filter(|d| !d.is_zero());
        let old_stat = previous.and_then(|p| p.stat.as_ref().ok());
        let boot_changed = old_stat
            .zip(sample.stat.as_ref().ok())
            .is_some_and(|(a, b)| {
                let boot = |s: &crate::raw::cpu::RawProcStat| {
                    s.other
                        .iter()
                        .find(|(key, _)| key == "btime")
                        .map(|(_, value)| value.clone())
                };
                boot(a) != boot(b)
            });
        let measure = |old, current| {
            if boot_changed {
                CpuValue::CounterReset
            } else if previous.is_some() && interval.is_none() {
                CpuValue::InvalidData
            } else {
                usage(old, current)
            }
        };
        let overall = match &sample.stat {
            Err(status) => failed(status),
            Ok(stat) => {
                let topology_changed =
                    old_stat.is_some_and(|old| !old.cpus.keys().eq(stat.cpus.keys()));
                if topology_changed {
                    CpuValue::Waiting
                } else {
                    measure(
                        old_stat.and_then(|s| s.aggregate.as_ref()),
                        stat.aggregate.as_ref(),
                    )
                }
            }
        };
        let cpus = sample
            .cpus
            .iter()
            .map(|(&id, cpu)| {
                let current = sample.stat.as_ref().ok().and_then(|s| s.cpus.get(&id));
                let was_offline = previous
                    .and_then(|p| p.cpus.get(&id))
                    .is_some_and(|p| p.online == Some(false));
                let old = if was_offline {
                    None
                } else {
                    old_stat.and_then(|s| s.cpus.get(&id))
                };
                let metrics = if cpu.online == Some(false) {
                    CpuThreadMetrics {
                        usage: CpuValue::Offline,
                        frequency_mhz: CpuValue::Offline,
                    }
                } else {
                    CpuThreadMetrics {
                        usage: match &sample.stat {
                            Err(status) => failed(status),
                            Ok(_) => measure(old, current),
                        },
                        frequency_mhz: frequency(&cpu.frequency_khz),
                    }
                };
                (id, metrics)
            })
            .collect();
        let temperatures = sample
            .temperatures
            .iter()
            .filter(|raw| supports_temperature(&raw.driver))
            .map(|raw| {
                let mut label = raw
                    .label
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| raw.channel.clone());
                if raw.driver == "k10temp" && (label == "Tctl" || raw.channel == "temp1") {
                    label = "Tctl (управляющее значение)".into();
                }
                CpuTemperature {
                    id: raw.path.to_string_lossy().into_owned(),
                    source: format!("{} / {}", raw.driver, raw.device),
                    label,
                    celsius: temperature(raw),
                    critical_celsius: millidegrees(&raw.critical),
                }
            })
            .collect();
        let power = sample
            .power
            .iter()
            .filter(|raw| supports_power(&raw.zone, &raw.domain))
            .map(|raw| {
                let old = previous.and_then(|sample| {
                    sample.power.iter().find(|old| {
                        old.path == raw.path && old.zone == raw.zone && old.domain == raw.domain
                    })
                });
                CpuPower {
                    source: raw.zone.clone(),
                    domain: raw.domain.clone(),
                    watts: power(raw, old, boot_changed),
                    averaged: matches!(raw.power_uw, Err(InterfaceStatus::NotAvailable)),
                }
            })
            .collect();
        let metrics = CpuMetrics {
            interval,
            usage: overall,
            throttling: throttling(&sample, previous),
            cpus,
            temperatures,
            power,
        };
        self.previous = Some(sample);
        metrics
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raw::cpu::RawProcStat;
    use std::time::Instant;

    fn counters(values: &[u64]) -> RawCpuCounters {
        RawCpuCounters {
            counters: values.into(),
        }
    }

    fn sample(at: Instant, user: u64, idle: u64) -> RawCpuSample {
        let values = [user, 0, 0, idle, 0, 0, 0, 0, 0, 0];
        RawCpuSample {
            at,
            stat: Ok(RawProcStat {
                text: String::new(),
                aggregate: Some(counters(&values)),
                cpus: BTreeMap::from([(7, counters(&values))]),
                other: vec![("btime".into(), "1".into())],
            }),
            cpus: BTreeMap::from([(
                7,
                RawCpuSampleEntry {
                    online: Some(true),
                    frequency_khz: Ok("4200000\n".into()),
                    throttle_counts: BTreeMap::new(),
                },
            )]),
            temperatures: Vec::new(),
            power: Vec::new(),
        }
    }

    #[test]
    fn usage_excludes_guest_duplicates_and_detects_invalid_intervals() {
        let old = counters(&[100, 20, 30, 400, 50, 10, 10, 5, 70, 10, 0]);
        let current = counters(&[120, 25, 35, 450, 60, 15, 15, 5, 90, 15, 999]);
        assert_eq!(usage(Some(&old), Some(&current)), CpuValue::Available(40.0));
        assert_eq!(usage(Some(&old), Some(&old)), CpuValue::NoProgress);
        assert_eq!(usage(None, Some(&old)), CpuValue::Waiting);
        assert_eq!(usage(Some(&old), None), CpuValue::NotAvailable);
        assert_eq!(usage(Some(&current), Some(&old)), CpuValue::CounterReset);
        let iowait_decreased = counters(&[120, 25, 35, 450, 49, 15, 15, 5]);
        assert_eq!(
            usage(Some(&old), Some(&iowait_decreased)),
            CpuValue::CounterReset
        );
        // Large counters must not overflow their sums; guest and future fields
        // do not participate, even when they are much larger than the deltas.
        let old = counters(&[u64::MAX - 100; 10]);
        let current = counters(&[u64::MAX; 10]);
        assert_eq!(usage(Some(&old), Some(&current)), CpuValue::Available(75.0));
        assert_eq!(
            usage(Some(&counters(&[0; 4])), Some(&counters(&[1, 0, 0, 3]))),
            CpuValue::Available(25.0)
        );
    }

    #[test]
    fn first_sample_errors_resets_and_recovery_never_invent_load() {
        let at = Instant::now();
        let mut monitor = CpuMonitor::default();
        assert_eq!(monitor.update(sample(at, 10, 90)).usage, CpuValue::Waiting);
        let metrics = monitor.update(sample(at + Duration::from_secs(1), 35, 165));
        assert_eq!(metrics.usage, CpuValue::Available(25.0));
        assert_eq!(metrics.cpus[&7].usage, CpuValue::Available(25.0));
        assert_eq!(metrics.cpus[&7].frequency_mhz, CpuValue::Available(4200.0));
        assert_eq!(metrics.interval, Some(Duration::from_secs(1)));
        assert_eq!(
            monitor
                .update(sample(at + Duration::from_secs(2), 0, 0))
                .usage,
            CpuValue::CounterReset
        );
        assert_eq!(
            monitor
                .update(sample(at + Duration::from_secs(3), 50, 50))
                .usage,
            CpuValue::Available(50.0)
        );
        let mut error = sample(at + Duration::from_secs(4), 0, 0);
        error.stat = Err(InterfaceStatus::PermissionDenied("fixture".into()));
        assert_eq!(monitor.update(error).usage, CpuValue::PermissionDenied);
        assert_eq!(
            monitor
                .update(sample(at + Duration::from_secs(5), 100, 100))
                .usage,
            CpuValue::Waiting
        );
        assert_eq!(
            monitor
                .update(sample(at + Duration::from_secs(6), 125, 175))
                .usage,
            CpuValue::Available(25.0)
        );
        assert_eq!(
            monitor
                .update(sample(at + Duration::from_secs(6), 150, 250))
                .usage,
            CpuValue::InvalidData
        );
    }

    #[test]
    fn hotplug_and_reboot_require_a_new_baseline() {
        let at = Instant::now();
        let mut monitor = CpuMonitor::default();
        monitor.update(sample(at, 10, 90));
        let mut offline = sample(at + Duration::from_secs(1), 10, 90);
        offline.cpus.get_mut(&7).unwrap().online = Some(false);
        offline.stat.as_mut().unwrap().cpus.clear();
        let metrics = monitor.update(offline);
        assert_eq!(metrics.usage, CpuValue::Waiting);
        assert_eq!(metrics.cpus[&7].usage, CpuValue::Offline);
        let metrics = monitor.update(sample(at + Duration::from_secs(2), 20, 180));
        assert_eq!(metrics.usage, CpuValue::Waiting);
        assert_eq!(metrics.cpus[&7].usage, CpuValue::Waiting);
        let mut reboot = sample(at + Duration::from_secs(3), 30, 270);
        reboot.stat.as_mut().unwrap().other[0].1 = "2".into();
        assert_eq!(monitor.update(reboot).usage, CpuValue::CounterReset);
    }

    fn sensor(
        driver: &str,
        label: &str,
        input: Result<String, InterfaceStatus>,
    ) -> RawCpuTemperature {
        RawCpuTemperature {
            path: format!("/fixture/{driver}/{label}/temp1_input").into(),
            device: "hwmon3".into(),
            driver: driver.into(),
            channel: "temp1".into(),
            label: Some(label.into()),
            input,
            fault: Err(InterfaceStatus::NotAvailable),
            critical: Err(InterfaceStatus::NotAvailable),
            critical_alarm: Err(InterfaceStatus::NotAvailable),
        }
    }

    #[test]
    fn temperature_identification_faults_and_units() {
        let mut raw = sample(Instant::now(), 0, 0);
        raw.temperatures = vec![
            sensor("nvme", "CPU", Ok("99000".into())),
            sensor("coretemp", "Package id 0", Ok("42500".into())),
            sensor("k10temp", "Tctl", Ok("52000".into())),
            sensor(
                "coretemp",
                "Core 0",
                Err(InterfaceStatus::PermissionDenied("fixture".into())),
            ),
        ];
        let metrics = CpuMonitor::default().update(raw);
        assert_eq!(metrics.temperatures.len(), 3);
        assert_eq!(metrics.temperatures[0].celsius, CpuValue::Available(42.5));
        assert_eq!(
            metrics.temperatures[0].critical_celsius,
            CpuValue::NotAvailable
        );
        assert!(metrics.temperatures[1].label.contains("управляющее"));
        assert_eq!(metrics.temperatures[2].celsius, CpuValue::PermissionDenied);
        let mut bad = sensor("coretemp", "Package id 0", Ok("999999".into()));
        assert_eq!(temperature(&bad), CpuValue::InvalidData);
        bad.input = Ok("0".into());
        assert_eq!(temperature(&bad), CpuValue::Available(0.0));
        bad.fault = Ok("1".into());
        assert_eq!(temperature(&bad), CpuValue::InvalidData);
        assert_eq!(frequency(&Ok("0".into())), CpuValue::InvalidData);
        assert_eq!(frequency(&Ok("NaN".into())), CpuValue::InvalidData);
        assert_eq!(
            frequency(&Err(InterfaceStatus::NotAvailable)),
            CpuValue::NotAvailable
        );
    }

    #[test]
    fn throttling_uses_alarm_or_counter_delta_without_treating_history_as_active() {
        let at = Instant::now();
        let mut monitor = CpuMonitor::default();
        let mut first = sample(at, 0, 100);
        first
            .cpus
            .get_mut(&7)
            .unwrap()
            .throttle_counts
            .insert("core_throttle_count".into(), Ok("12\n".into()));
        let mut temperature = sensor("coretemp", "Package id 0", Ok("80000".into()));
        temperature.critical = Ok("100000\n".into());
        temperature.critical_alarm = Err(InterfaceStatus::NotAvailable);
        first.temperatures.push(temperature);
        let metrics = monitor.update(first);
        assert_eq!(metrics.throttling, CpuValue::Waiting);
        assert_eq!(
            metrics.temperatures[0].critical_celsius,
            CpuValue::Available(100.0)
        );

        let mut unchanged = sample(at + Duration::from_secs(1), 10, 190);
        unchanged
            .cpus
            .get_mut(&7)
            .unwrap()
            .throttle_counts
            .insert("core_throttle_count".into(), Ok("12\n".into()));
        assert_eq!(
            monitor.update(unchanged).throttling,
            CpuValue::Available(0.0)
        );

        let mut increased = sample(at + Duration::from_secs(2), 20, 280);
        increased
            .cpus
            .get_mut(&7)
            .unwrap()
            .throttle_counts
            .insert("core_throttle_count".into(), Ok("13\n".into()));
        assert_eq!(
            monitor.update(increased).throttling,
            CpuValue::Available(1.0)
        );

        let mut alarm = sample(at + Duration::from_secs(3), 30, 370);
        let mut temperature = sensor("coretemp", "Package id 0", Ok("101000".into()));
        temperature.critical_alarm = Ok("1\n".into());
        alarm.temperatures.push(temperature);
        assert_eq!(monitor.update(alarm).throttling, CpuValue::Available(1.0));

        assert_eq!(
            CpuMonitor::default().update(sample(at, 0, 0)).throttling,
            CpuValue::NotSupported
        );
    }

    fn power_reading(at: Instant, value: Result<String, InterfaceStatus>) -> RawCpuPower {
        RawCpuPower {
            path: "/fixture/intel-rapl:0".into(),
            zone: "intel-rapl:0".into(),
            domain: "package-0".into(),
            at,
            power_uw: Err(InterfaceStatus::NotAvailable),
            energy_uj: value,
            max_energy_range_uj: Ok("1000000000".into()),
        }
    }

    #[test]
    fn power_uses_energy_and_its_own_measurement_interval() {
        let at = Instant::now();
        let mut monitor = CpuMonitor::default();
        let mut first = sample(at, 0, 0);
        first.power.push(power_reading(at, Ok("10000000".into())));
        assert_eq!(monitor.update(first).power[0].watts, CpuValue::Waiting);
        let mut second = sample(at + Duration::from_secs(1), 10, 90);
        // Energy readings are two seconds apart, despite a one-second CPU
        // sampling interval: 25 joules / 2 seconds = 12.5 W.
        second.power.push(power_reading(
            at + Duration::from_secs(2),
            Ok("35000000".into()),
        ));
        let metrics = monitor.update(second);
        assert_eq!(metrics.power[0].watts, CpuValue::Available(12.5));
        assert!(metrics.power[0].averaged);
        let mut third = sample(at + Duration::from_secs(2), 20, 180);
        third.power.push(power_reading(
            at + Duration::from_secs(3),
            Ok("35000000".into()),
        ));
        assert_eq!(
            monitor.update(third).power[0].watts,
            CpuValue::Available(0.0)
        );
    }

    #[test]
    fn power_failures_resets_wraps_and_recovery_are_explicit() {
        let at = Instant::now();
        let initial = power_reading(at, Ok("900000000".into()));
        let mut current = power_reading(at + Duration::from_secs(1), Ok("10000000".into()));
        assert_eq!(
            power(&current, Some(&initial), false),
            CpuValue::CounterReset
        );
        // The next interval after a decrease can be used again.
        let next = power_reading(at + Duration::from_secs(2), Ok("20000000".into()));
        assert_eq!(
            power(&next, Some(&current), false),
            CpuValue::Available(10.0)
        );
        assert_eq!(power(&next, Some(&current), true), CpuValue::CounterReset);
        current.at = next.at;
        assert_eq!(power(&next, Some(&current), false), CpuValue::InvalidData);
        current.energy_uj = Err(InterfaceStatus::PermissionDenied("fixture".into()));
        assert_eq!(
            power(&current, Some(&initial), false),
            CpuValue::PermissionDenied
        );
        assert_eq!(power(&next, Some(&current), false), CpuValue::Waiting);
        current.energy_uj = Err(InterfaceStatus::ReadError("fixture".into()));
        assert_eq!(power(&current, Some(&initial), false), CpuValue::ReadError);
        current.energy_uj = Err(InterfaceStatus::NotAvailable);
        assert_eq!(
            power(&current, Some(&initial), false),
            CpuValue::NotAvailable
        );
        for invalid in ["-1", "NaN", "1000000001", "18446744073709551616"] {
            current.energy_uj = Ok(invalid.into());
            assert_eq!(
                power(&current, Some(&initial), false),
                CpuValue::InvalidData
            );
        }
    }

    #[test]
    fn power_filters_subdomains_and_keeps_multiple_packages_separate() {
        let at = Instant::now();
        let mut raw = sample(at, 0, 0);
        let mut package = power_reading(at, Err(InterfaceStatus::NotAvailable));
        package.power_uw = Ok("62500000".into());
        raw.power.push(package);
        let mut second = power_reading(at, Ok("0".into()));
        second.path = "/fixture/intel-rapl:1".into();
        second.zone = "intel-rapl:1".into();
        second.domain = "package-1".into();
        raw.power.push(second);
        for (zone, name) in [
            ("intel-rapl:0:0", "core"),
            ("intel-rapl:0:1", "dram"),
            ("intel-rapl:2", "psys"),
            ("other:0", "package-0"),
        ] {
            let mut other = power_reading(at, Ok("100".into()));
            other.zone = zone.into();
            other.domain = name.into();
            raw.power.push(other);
        }
        let metrics = CpuMonitor::default().update(raw);
        assert_eq!(metrics.power.len(), 2);
        assert_eq!(metrics.power[0].watts, CpuValue::Available(62.5));
        assert!(!metrics.power[0].averaged);
        assert_eq!(metrics.power[1].domain, "package-1");
        assert_eq!(metrics.power[1].watts, CpuValue::Waiting);
    }
}
