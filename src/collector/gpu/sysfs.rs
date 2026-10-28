//! Cached sensor discovery with read-only live values. No control interfaces are written.
use super::*;
use GpuUnit::*;

struct Source {
    id: String,
    path: PathBuf,
    unit: GpuUnit,
    active_dpm: bool,
    fault: Option<PathBuf>,
    enabled: Option<PathBuf>,
    owner: Option<PathBuf>,
}
#[derive(Default)]
pub(super) struct Sources {
    live: Vec<Source>,
    pins: Vec<(PathBuf, String)>,
    static_values: BTreeMap<String, RawGpuValue>,
}
fn add(s: &mut Sources, id: &str, path: PathBuf, unit: GpuUnit, owner: Option<&Path>, dpm: bool) {
    s.live.push(Source {
        id: id.into(),
        path,
        unit,
        active_dpm: dpm,
        fault: None,
        enabled: None,
        owner: owner.map(Path::to_owned),
    });
}
fn optional(s: &mut Sources, id: &str, path: PathBuf, unit: GpuUnit, owner: Option<&Path>) {
    if path.exists() {
        add(s, id, path, unit, owner, false);
    }
}
fn sensor(s: &mut Sources, id: &str, path: PathBuf, unit: GpuUnit, hwmon: &Path, base: &str) {
    let fault = hwmon.join(format!("{base}_fault"));
    let enabled = hwmon.join(format!("{base}_enable"));
    s.live.push(Source {
        id: id.into(),
        path,
        unit,
        active_dpm: false,
        fault: fault.exists().then_some(fault),
        enabled: (!base.starts_with("fan") && enabled.exists()).then_some(enabled),
        owner: Some(hwmon.into()),
    });
}

pub(super) fn discover(c: &mut CachedGpu) {
    let mut s = Sources::default();
    for (id, file, unit) in [
        ("pcie_speed", "current_link_speed", PcieSpeed),
        ("pcie_width", "current_link_width", PcieWidth),
    ] {
        add(&mut s, id, c.path.join(file), unit, None, false);
    }
    for (id, file, unit) in [
        ("pcie_max_speed", "max_link_speed", PcieSpeed),
        ("pcie_max_width", "max_link_width", PcieWidth),
    ] {
        let path = c.path.join(file);
        s.static_values.insert(
            id.into(),
            RawGpuValue {
                value: text(&path),
                unit,
                source: path.display().to_string(),
            },
        );
    }
    let driver = c.raw.metadata.get("driver").cloned().unwrap_or_default();
    if matches!(driver.as_str(), "amdgpu" | "radeon") {
        for (id, file, unit) in [
            ("load", "gpu_busy_percent", Percent),
            ("memory_load", "mem_busy_percent", Percent),
            ("memory_total", "mem_info_vram_total", Byte),
            ("memory_used", "mem_info_vram_used", Byte),
            ("shared_total", "mem_info_gtt_total", Byte),
            ("shared_used", "mem_info_gtt_used", Byte),
        ] {
            add(&mut s, id, c.path.join(file), unit, None, false);
        }
        for (id, file) in [
            ("core_clock", "pp_dpm_sclk"),
            ("memory_clock", "pp_dpm_mclk"),
        ] {
            add(&mut s, id, c.path.join(file), Megahertz, None, true);
        }
    }
    for h in entries(&c.path.join("hwmon")).unwrap_or_default() {
        if fs::canonicalize(h.join("device")).ok().as_ref() != Some(&c.path) {
            continue;
        }
        let name = match text(&h.join("name")) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if !matches!(
            name.as_str(),
            "amdgpu" | "radeon" | "nouveau" | "i915" | "xe"
        ) || name != driver
        {
            continue;
        }
        s.pins.push((h.clone(), name));
        let mut powers = Vec::new();
        for file in entries(&h).unwrap_or_default() {
            let n = file.file_name().unwrap_or_default().to_string_lossy();
            if n.starts_with("temp") && n.ends_with("_input") {
                let base = n.trim_end_matches("_input");
                let label = text(&h.join(format!("{base}_label")))
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                let id = match label.as_str() {
                    "edge" | "gpu" | "gpu core" => Some("temperature"),
                    "junction" | "hotspot" | "hot spot" => Some("hotspot"),
                    "mem" | "memory" | "vram" => Some("memory_temperature"),
                    _ if base == "temp1" => Some("temperature"),
                    _ => None,
                };
                if let Some(id) = id {
                    sensor(&mut s, id, file.clone(), MilliCelsius, &h, base);
                    let path = h.join(format!("{base}_crit"));
                    if path.exists() {
                        s.static_values.insert(
                            format!("{id}_critical"),
                            RawGpuValue {
                                value: text(&path),
                                unit: MilliCelsius,
                                source: path.display().to_string(),
                            },
                        );
                    }
                }
            }
            if n.starts_with("fan") && n.ends_with("_input") {
                let base = n.trim_end_matches("_input");
                if numbered(base, "fan") {
                    sensor(&mut s, &format!("{base}_rpm"), file.clone(), Rpm, &h, base);
                }
            }
            if numbered(&n, "pwm") {
                add(
                    &mut s,
                    &format!("fan{}_pwm", n.trim_start_matches("pwm")),
                    file.clone(),
                    Pwm,
                    Some(&h),
                    false,
                );
            }
            if n.starts_with("power") && (n.ends_with("_average") || n.ends_with("_input")) {
                let base = n.rsplit_once('_').unwrap().0;
                if numbered(base, "power") {
                    let label = text(&h.join(format!("{base}_label")))
                        .unwrap_or_default()
                        .to_ascii_lowercase();
                    powers.push((
                        match label.as_str() {
                            "card" | "board" => 0,
                            "ppt" | "pkg" => 1,
                            _ => 2,
                        },
                        base.to_owned(),
                    ));
                }
            }
        }
        powers.sort();
        powers.dedup();
        if let Some((_, base)) = powers.first() {
            let input = h.join(format!("{base}_input"));
            let average = h.join(format!("{base}_average"));
            let instantaneous = input.exists();
            add(
                &mut s,
                "power",
                if instantaneous { input } else { average },
                MicroWatt,
                Some(&h),
                false,
            );
            optional(
                &mut s,
                "power_limit",
                h.join(format!("{base}_cap")),
                MicroWatt,
                Some(&h),
            );
            let label = text(&h.join(format!("{base}_label")))
                .unwrap_or_default()
                .to_ascii_lowercase();
            let scope = match (driver.as_str(), label.as_str()) {
                (_, "card" | "board") => "board",
                ("amdgpu" | "radeon", _) => "soc",
                (_, "pkg") => "gpu_package",
                (_, "gpu") => "chip",
                _ => "unknown",
            };
            for key in ["power_scope", "power_limit_scope"] {
                c.raw.metadata.insert(key.into(), scope.into());
            }
            for key in ["power_domain", "power_limit_domain"] {
                c.raw
                    .metadata
                    .insert(key.into(), format!("{}/{base}", h.display()));
            }
            c.raw.metadata.insert(
                "power_sampling".into(),
                if instantaneous {
                    "instantaneous"
                } else {
                    "average"
                }
                .into(),
            );
        } else if h.join("energy1_input").exists() {
            add(
                &mut s,
                "energy",
                h.join("energy1_input"),
                MicroJoule,
                Some(&h),
                false,
            );
            optional(
                &mut s,
                "power_limit",
                h.join("power1_max"),
                MicroWatt,
                Some(&h),
            );
            for key in ["power_scope", "power_limit_scope"] {
                c.raw.metadata.insert(key.into(), "gpu_package".into());
            }
            for key in ["power_domain", "power_limit_domain"] {
                c.raw.metadata.insert(key.into(), h.display().to_string());
            }
        }
        optional(&mut s, "core_clock", h.join("freq1_input"), Hertz, Some(&h));
        optional(
            &mut s,
            "memory_clock",
            h.join("freq2_input"),
            Hertz,
            Some(&h),
        );
    }
    if matches!(driver.as_str(), "i915" | "xe") {
        let mut gts = Vec::new();
        if let Some(card) = &c.card {
            gts.extend(
                entries(&card.join("gt"))
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| numbered(n, "gt"))
                    }),
            );
            if gts.is_empty() {
                optional(
                    &mut s,
                    "core_clock",
                    card.join("gt_act_freq_mhz"),
                    Megahertz,
                    None,
                );
            }
        }
        for tile in entries(&c.path)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| numbered(n, "tile"))
            })
        {
            gts.extend(entries(&tile).unwrap_or_default().into_iter().filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| numbered(n, "gt"))
            }));
        }
        gts.sort();
        gts.dedup();
        c.raw
            .metadata
            .insert("intel_gt_count".into(), gts.len().to_string());
        for (index, gt) in gts.iter().enumerate() {
            let id = if gts.len() == 1 {
                "core_clock".into()
            } else {
                format!("gt{index}_clock")
            };
            optional(
                &mut s,
                &id,
                gt.join(if driver == "xe" {
                    "freq0/act_freq"
                } else {
                    "rps_act_freq_mhz"
                }),
                Megahertz,
                None,
            );
            for reason in [
                "thermal",
                "soc_thermal",
                "mem_thermal",
                "vr_thermal",
                "soc_avg_thermal",
                "pl1",
                "pl2",
                "pl4",
                "psys_pl1",
                "psys_pl2",
                "pwrbrk",
            ] {
                let path = if driver == "xe" {
                    gt.join(format!("freq0/throttle/reason_{reason}"))
                } else {
                    gt.join(format!("throttle_reason_{reason}"))
                };
                optional(
                    &mut s,
                    &format!("gt{index}_throttle_{reason}"),
                    path,
                    Boolean,
                    None,
                );
            }
        }
    }
    c.sources = s;
}

pub(super) fn sample(c: &mut CachedGpu) {
    let valid: BTreeSet<_> = c
        .sources
        .pins
        .iter()
        .filter(|(p, name)| {
            fs::canonicalize(p.join("device")).ok().as_ref() == Some(&c.path)
                && text(&p.join("name")).ok().as_ref() == Some(name)
        })
        .map(|(p, _)| p)
        .collect();
    for source in &c.sources.live {
        if source.owner.as_ref().is_some_and(|p| !valid.contains(p)) {
            continue;
        }
        let result = (|| {
            if let Some(fault) = &source.fault
                && text(fault)? != "0"
            {
                return Err(GpuError::InvalidData);
            }
            if let Some(enabled) = &source.enabled
                && text(enabled)? != "1"
            {
                return Err(GpuError::NotAvailable);
            }
            if source.id == "energy" {
                c.raw.energy_at = Some(Instant::now());
            }
            let value = text(&source.path)?;
            if source.active_dpm {
                active_clock(&value)
            } else {
                Ok(value)
            }
        })();
        put(
            &mut c.raw,
            &source.id,
            source.unit,
            result,
            &source.path.display().to_string(),
        );
    }
    for (id, value) in &c.sources.static_values {
        if Path::new(&value.source).parent() == Some(c.path.as_path())
            || valid
                .iter()
                .any(|p| Path::new(&value.source).starts_with(p))
        {
            c.raw.values.insert(id.clone(), value.clone());
        }
    }
    c.raw.amd_metrics = if c.raw.metadata.get("driver").is_some_and(|d| d == "amdgpu") {
        binary(&c.path.join("gpu_metrics"))
    } else {
        Err(GpuError::NotSupported)
    };
}

fn active_clock(s: &str) -> Result<String, GpuError> {
    let active: Vec<_> = s
        .lines()
        .filter(|l| l.split_whitespace().any(|p| p == "*"))
        .collect();
    if active.len() != 1 {
        return Err(GpuError::NotAvailable);
    }
    let n = active[0]
        .split_whitespace()
        .nth(1)
        .ok_or(GpuError::InvalidData)?;
    let n = n
        .strip_suffix("Mhz")
        .or_else(|| n.strip_suffix("MHz"))
        .ok_or(GpuError::InvalidData)?;
    if n.parse::<u64>().is_err() {
        return Err(GpuError::InvalidData);
    }
    Ok(n.into())
}
