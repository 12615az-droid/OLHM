use super::*;

pub(super) fn values(raw: &Result<RawFsStat, InterfaceStatus>) -> [MetricValue; 5] {
    let calculate = || -> Result<[f64; 5], MetricValue> {
        let stats = raw.as_ref().map_err(failed)?;
        if stats.fragment_size == 0
            || stats.blocks == 0
            || stats.free > stats.blocks
            || stats.available > stats.free
        {
            return Err(MetricValue::InvalidData);
        }
        let total = stats
            .blocks
            .checked_mul(stats.fragment_size)
            .ok_or(MetricValue::InvalidData)?;
        let free = stats
            .free
            .checked_mul(stats.fragment_size)
            .ok_or(MetricValue::InvalidData)?;
        let available = stats
            .available
            .checked_mul(stats.fragment_size)
            .ok_or(MetricValue::InvalidData)?;
        let used = total - free;
        let denominator = used
            .checked_add(available)
            .ok_or(MetricValue::InvalidData)?;
        let percent = if denominator == 0 {
            return Err(MetricValue::InvalidData);
        } else {
            used as f64 / denominator as f64 * 100.0
        };
        Ok([
            total as f64,
            used as f64,
            available as f64,
            free as f64,
            percent,
        ])
    };
    match calculate() {
        Ok(values) => values.map(MetricValue::Available),
        Err(error) => std::array::from_fn(|_| error.clone()),
    }
}

pub(super) fn readings(mounts: &[RawMount], blocks: &[RawBlock]) -> Vec<DeviceReading> {
    let mut groups: BTreeMap<_, Vec<&RawMount>> = BTreeMap::new();
    for mount in mounts {
        groups
            .entry((
                &mount.major_minor,
                &mount.root,
                &mount.source,
                &mount.fs_type,
            ))
            .or_default()
            .push(mount);
    }
    groups
        .into_iter()
        .map(|(key, mounts)| {
            let mount = mounts[0];
            let stats = mounts
                .iter()
                .find(|mount| mount.stats.is_ok())
                .unwrap_or(&mount)
                .stats
                .clone();
            let parent = blocks
                .iter()
                .find(|block| attr(block, "dev").as_deref() == Some(&mount.major_minor))
                .or_else(|| {
                    blocks.iter().find(|block| {
                        mount.source == format!("/dev/{}", block.name)
                            || block.metadata.get("path") == Some(&mount.source)
                    })
                });
            let properties = vec![
                property(
                    "source",
                    "Источник mountinfo (не перечень всех дисков ФС)",
                    Some(mount.source.clone()),
                ),
                property("fs_type", "Файловая система", Some(mount.fs_type.clone())),
                property("root", "Корень / подтом", Some(mount.root.clone())),
                property(
                    "mountpoints",
                    "Точки монтирования",
                    Some(
                        mounts
                            .iter()
                            .map(|m| m.target.as_str())
                            .collect::<Vec<_>>()
                            .join("; "),
                    ),
                ),
                property(
                    "mount_modes",
                    "Режимы монтирования",
                    Some(
                        mounts
                            .iter()
                            .map(|m| format!("{}: {}", m.target, m.options))
                            .collect::<Vec<_>>()
                            .join("; "),
                    ),
                ),
                property(
                    "usage_method",
                    "Процент заполнения",
                    Some("занято / (занято + доступно пользователю), как df".into()),
                ),
            ];
            let [total, used, available, free, percent] = values(&stats);
            DeviceReading {
                device: Device {
                    id: format!("storage/fs/{key:?}"),
                    parent_id: Some(parent.map(id).unwrap_or_else(|| "storage".into())),
                    kind: DeviceKind::FileSystem,
                    name: format!("ФС {} [{}; {}]", mount.source, mount.fs_type, mount.target),
                    properties,
                },
                metrics: vec![
                    metric("total", "Размер ФС", Unit::Byte, total),
                    metric("used", "Занято", Unit::Byte, used),
                    metric("available", "Доступно пользователю", Unit::Byte, available),
                    metric("free", "Свободно всего (включая резерв)", Unit::Byte, free),
                    metric("usage", "Заполненность", Unit::Percent, percent),
                ],
            }
        })
        .collect()
}
