//! Versioned, bounded gpu_metrics parsing. Unknown layouts are never guessed.
use crate::raw::gpu::*;

fn word(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}
fn insert(raw: &mut RawGpu, id: &str, number: u16, unit: GpuUnit) {
    if number == u16::MAX {
        return;
    } // Linux initialises unsupported fields to all ones.
    if raw.values.get(id).is_none_or(|v| {
        matches!(
            v.value,
            Err(GpuError::NotAvailable | GpuError::NotSupported)
        )
    }) {
        raw.values.insert(
            id.into(),
            RawGpuValue {
                value: Ok(number.to_string()),
                unit,
                source: "amdgpu gpu_metrics (versioned)".into(),
            },
        );
    }
}

pub(super) fn augment(raw: &mut RawGpu) {
    let bytes = match &raw.amd_metrics {
        Ok(bytes) => bytes.clone(),
        Err(_) => return,
    };
    let Some(length) = word(&bytes, 0).map(usize::from) else {
        return;
    };
    if length != bytes.len() || length < 4 {
        raw.metadata
            .insert("gpu_metrics".into(), "некорректная длина".into());
        return;
    }
    let version = (bytes[2], bytes[3]);
    raw.metadata
        .insert("gpu_metrics".into(), format!("{}.{}", version.0, version.1));
    // v1.4/v1.5 export VCN instances separately, not individual encode/decode percentages.
    if matches!(version, (1, 4) | (1, 5)) {
        if bytes.len() < 32 {
            return;
        }
        for (id, offset, unit) in [
            ("hotspot", 4, GpuUnit::Celsius),
            ("memory_temperature", 6, GpuUnit::Celsius),
            ("load", 12, GpuUnit::Percent),
            ("memory_load", 14, GpuUnit::Percent),
        ] {
            insert(raw, id, word(&bytes, offset).unwrap(), unit);
        }
        for index in 0..4 {
            insert(
                raw,
                &format!("media_vcn{index}"),
                word(&bytes, 16 + index * 2).unwrap(),
                GpuUnit::Percent,
            );
        }
        return;
    }
    // gpu_metrics v1.1-v1.3 share this prefix; v1.0 and v1.4+ have different layouts.
    let minimum = match version {
        (1, 1) => 96,
        (1, 2) => 104,
        (1, 3) => 120,
        _ => return,
    };
    if bytes.len() < minimum {
        raw.metadata
            .insert("gpu_metrics".into(), "усечённая структура".into());
        return;
    }
    for (id, offset, unit) in [
        ("temperature", 4, GpuUnit::Celsius),
        ("hotspot", 6, GpuUnit::Celsius),
        ("memory_temperature", 8, GpuUnit::Celsius),
        ("load", 16, GpuUnit::Percent),
        ("memory_load", 18, GpuUnit::Percent),
        ("media", 20, GpuUnit::Percent),
        ("core_clock", 54, GpuUnit::Megahertz),
        ("memory_clock", 58, GpuUnit::Megahertz),
        ("fan1_rpm", 72, GpuUnit::Rpm),
    ] {
        insert(raw, id, word(&bytes, offset).unwrap(), unit);
    }
    if version == (1, 3) {
        // Only ASIC-independent flags have stable meanings across AMD generations.
        let bits = u64::from_le_bytes(bytes[112..120].try_into().unwrap());
        if bits != u64::MAX {
            for (id, mask) in [
                ("power_throttle", 0xffu64),
                ("thermal_throttle", 0xffffu64 << 32),
            ] {
                insert(raw, id, u16::from(bits & mask != 0), GpuUnit::Boolean);
            }
        }
    }
}
