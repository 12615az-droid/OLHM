//! Hardware window consuming the same processed snapshots as the CLI.
use super::{format::metric, gui_worker::Worker};
use crate::model::{DeviceKind, DeviceReading, Metric, MetricValue, MonitorSnapshot, Unit};
use eframe::egui::{self, Color32, RichText};
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(74, 205, 187);

pub fn run(target: String, interval: Duration) -> Result<(), Box<dyn std::error::Error>> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(concat!(
                "OLHM ",
                env!("CARGO_PKG_VERSION"),
                " — CPU / RAM / Storage / GPU"
            ))
            .with_inner_size([1040.0, 780.0])
            .with_min_inner_size([520.0, 420.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "olhm",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            cc.egui_ctx.style_mut(|style| {
                style.spacing.item_spacing = egui::vec2(12.0, 10.0);
                style.visuals.selection.bg_fill = ACCENT.gamma_multiply(0.35);
                style.visuals.panel_fill = Color32::from_rgb(20, 25, 33);
            });
            Ok(Box::new(CpuApp::new(cc.egui_ctx.clone(), target, interval)))
        }),
    )
    .map_err(|error| std::io::Error::other(format!("Не удалось открыть GUI: {error}")).into())
}

struct CpuApp {
    target: String,
    interval: Duration,
    worker: Option<Worker>,
    snapshot: Option<MonitorSnapshot>,
    received: Option<Instant>,
    error: Option<String>,
}

impl CpuApp {
    fn new(ctx: egui::Context, target: String, interval: Duration) -> Self {
        let mut app = Self {
            target,
            interval,
            worker: None,
            snapshot: None,
            received: None,
            error: None,
        };
        app.restart(ctx);
        app
    }

    fn restart(&mut self, ctx: egui::Context) {
        self.worker = None;
        self.snapshot = None;
        self.received = None;
        let target = self.target.clone();
        match Worker::start(
            self.interval,
            move || ctx.request_repaint(),
            move || crate::monitor::open(&target),
        ) {
            Ok(worker) => {
                self.worker = Some(worker);
                self.error = None;
            }
            Err(error) => self.error = Some(format!("Не удалось запустить мониторинг: {error}")),
        }
    }

    fn receive(&mut self) {
        let Some(worker) = &self.worker else {
            return;
        };
        if let Some(message) = worker.take() {
            match message {
                Ok(snapshot) => {
                    self.snapshot = Some(snapshot);
                    self.received = Some(Instant::now());
                }
                Err(error) => {
                    self.snapshot = None;
                    self.error = Some(error);
                }
            }
        } else if worker.is_finished() && self.error.is_none() {
            self.snapshot = None;
            self.error = Some("Рабочий поток мониторинга остановлен".into());
        }
    }
}

impl CpuApp {
    fn show(&mut self, ctx: &egui::Context) {
        self.receive();
        // Only a low-frequency status timer; incoming samples wake the window.
        ctx.request_repaint_after(Duration::from_secs(1));
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("OLHM").size(25.0).strong().color(ACCENT));
                for (target, label) in [
                    ("cpu", "CPU"),
                    ("ram", "RAM"),
                    ("storage", "Диски"),
                    ("gpu", "GPU"),
                ] {
                    if ui.selectable_label(self.target == target, label).clicked()
                        && self.target != target
                    {
                        self.target = target.into();
                        self.restart(ctx.clone());
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.weak(format!("v{}", env!("CARGO_PKG_VERSION")));
                    if ui.button("Закрыть").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
            ui.add_space(8.0);
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("Опрос: {:.1} с", self.interval.as_secs_f64()));
                if let Some(snapshot) = &self.snapshot
                    && let Some(interval) = snapshot.interval
                {
                    ui.weak(format!("Интервал замеров: {:.3} с", interval.as_secs_f64()));
                }
                if let Some(received) = self.received {
                    ui.weak(format!(
                        "Последний кадр: {:.0} с назад",
                        received.elapsed().as_secs_f64()
                    ));
                }
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            if let Some(error) = &self.error {
                ui.heading("Мониторинг недоступен");
                ui.colored_label(Color32::LIGHT_RED, error);
                if ui.button("Повторить подключение").clicked() {
                    self.restart(ctx.clone());
                }
                return;
            }
            let Some(snapshot) = &self.snapshot else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(match self.target.as_str() {
                        "ram" => "Чтение оперативной памяти…",
                        "storage" => "Обнаружение накопителей и файловых систем…",
                        "gpu" => "Обнаружение видеокарт и доступных датчиков…",
                        _ => "Обнаружение процессора и датчиков…",
                    });
                });
                return;
            };
            if self.received.is_some_and(|at| {
                at.elapsed() > self.interval.saturating_mul(3).max(Duration::from_secs(3))
            }) {
                ui.colored_label(
                    Color32::YELLOW,
                    "Новые данные задерживаются. Ниже — последний полученный кадр.",
                );
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for reading in snapshot
                        .devices
                        .iter()
                        .filter(|r| r.device.parent_id.is_none())
                    {
                        if matches!(
                            reading.device.kind,
                            DeviceKind::Memory | DeviceKind::Storage | DeviceKind::Graphics
                        ) {
                            memory_overview(ui, reading);
                        } else {
                            overview(ui, reading);
                        }
                    }
                    let threads: Vec<_> = snapshot
                        .devices
                        .iter()
                        .filter(|r| r.device.kind == DeviceKind::LogicalCpu)
                        .collect();
                    if !threads.is_empty() {
                        ui.add_space(12.0);
                        ui.heading("Логические процессоры");
                        egui::Grid::new("cpu-threads")
                            .num_columns(3)
                            .striped(true)
                            .spacing([24.0, 10.0])
                            .show(ui, |ui| {
                                ui.strong("Процессор");
                                ui.strong("Загрузка");
                                ui.strong("Частота");
                                ui.end_row();
                                for reading in threads {
                                    ui.label(&reading.device.name);
                                    for id in ["usage", "frequency"] {
                                        if let Some(value) =
                                            reading.metrics.iter().find(|m| m.id == id)
                                        {
                                            metric_ui(ui, value, 150.0);
                                        } else {
                                            ui.weak("недоступно");
                                        }
                                    }
                                    ui.end_row();
                                }
                            });
                    }
                    for (kind, heading) in [
                        (DeviceKind::PowerDomain, "Потребление CPU"),
                        (DeviceKind::TemperatureSensor, "Температуры CPU"),
                        (DeviceKind::MemoryModule, "Модули памяти / SPD"),
                        (
                            DeviceKind::MemoryTemperature,
                            "Датчики памяти без привязки к модулю",
                        ),
                        (DeviceKind::Swap, "Swap"),
                        (DeviceKind::Gpu, "Видеокарты"),
                        (
                            DeviceKind::BlockDevice,
                            "Накопители и целые блочные устройства",
                        ),
                        (DeviceKind::Partition, "Разделы"),
                        (
                            DeviceKind::FileSystem,
                            "Файловые системы и точки монтирования",
                        ),
                    ] {
                        let readings: Vec<_> = snapshot
                            .devices
                            .iter()
                            .filter(|r| r.device.kind == kind)
                            .collect();
                        if readings.is_empty() {
                            continue;
                        }
                        ui.add_space(16.0);
                        ui.heading(heading);
                        for reading in readings {
                            if kind == DeviceKind::Gpu {
                                gpu_card(ui, reading);
                                continue;
                            }
                            if matches!(
                                kind,
                                DeviceKind::BlockDevice
                                    | DeviceKind::Partition
                                    | DeviceKind::FileSystem
                            ) {
                                if kind == DeviceKind::Partition {
                                    egui::CollapsingHeader::new(&reading.device.name)
                                        .id_salt(&reading.device.id)
                                        .show(ui, |ui| storage_card(ui, reading));
                                } else {
                                    storage_card(ui, reading);
                                }
                                continue;
                            }
                            egui::Frame::group(ui.style()).show(ui, |ui| {
                                ui.set_width((ui.available_width() - 2.0).max(0.0));
                                ui.label(&reading.device.name);
                                if matches!(
                                    kind,
                                    DeviceKind::Swap
                                        | DeviceKind::MemoryModule
                                        | DeviceKind::MemoryTemperature
                                ) {
                                    for property in &reading.device.properties {
                                        ui.horizontal_wrapped(|ui| {
                                            ui.weak(format!("{}:", property.label));
                                            ui.label(
                                                property.value.as_deref().unwrap_or("недоступно"),
                                            );
                                        });
                                    }
                                }
                                for value in &reading.metrics {
                                    if matches!(
                                        kind,
                                        DeviceKind::Swap
                                            | DeviceKind::MemoryModule
                                            | DeviceKind::MemoryTemperature
                                    ) {
                                        ui.label(&value.label);
                                    }
                                    metric_ui(ui, value, 200.0);
                                }
                            });
                        }
                    }
                });
        });
    }
}

impl eframe::App for CpuApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.show(ctx);
    }
}

fn gpu_card(ui: &mut egui::Ui, reading: &DeviceReading) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width((ui.available_width() - 2.0).max(0.0));
        ui.heading(&reading.device.name);
        for property in reading.device.properties.iter().filter(|p| {
            matches!(
                p.id.as_str(),
                "vendor" | "driver" | "driver_version" | "memory_kind"
            )
        }) {
            ui.horizontal_wrapped(|ui| {
                ui.weak(format!("{}:", property.label));
                ui.label(property.value.as_deref().unwrap_or("недоступно"));
            });
        }
        egui::CollapsingHeader::new("Характеристики и источники GPU")
            .id_salt((&reading.device.id, "details"))
            .show(ui, |ui| {
                for property in &reading.device.properties {
                    ui.label(format!(
                        "{}: {}",
                        property.label,
                        property.value.as_deref().unwrap_or("недоступно")
                    ));
                }
            });
        for value in reading.metrics.iter().filter(|m| {
            !matches!(
                m.value,
                MetricValue::NotSupported | MetricValue::NotAvailable
            )
        }) {
            ui.label(&value.label);
            if matches!(value.id.as_str(), "thermal_throttle" | "power_throttle")
                && value.value == MetricValue::Available(1.0)
            {
                ui.colored_label(Color32::LIGHT_RED, metric(value));
            } else {
                metric_ui(ui, value, ui.available_width().min(400.0));
            }
        }
        egui::CollapsingHeader::new("Недоступные и неподдерживаемые показатели")
            .id_salt((&reading.device.id, "missing"))
            .show(ui, |ui| {
                for value in reading.metrics.iter().filter(|m| {
                    matches!(
                        m.value,
                        MetricValue::NotSupported | MetricValue::NotAvailable
                    )
                }) {
                    ui.label(format!("{}: {}", value.label, metric(value)));
                }
            });
    });
}

fn storage_card(ui: &mut egui::Ui, reading: &DeviceReading) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width((ui.available_width() - 2.0).max(0.0));
        ui.strong(&reading.device.name);
        let essential =
            |id: &str| matches!(id, "model" | "fs_type" | "mountpoints" | "smart_assessment");
        for property in reading
            .device
            .properties
            .iter()
            .filter(|p| essential(&p.id))
        {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("{}:", property.label));
                ui.label(property.value.as_deref().unwrap_or("недоступно"));
            });
        }
        egui::CollapsingHeader::new("Характеристики и привязки")
            .id_salt(&reading.device.id)
            .show(ui, |ui| {
                for property in reading
                    .device
                    .properties
                    .iter()
                    .filter(|p| !essential(&p.id))
                {
                    ui.horizontal_wrapped(|ui| {
                        ui.weak(format!("{}:", property.label));
                        ui.label(property.value.as_deref().unwrap_or("недоступно"));
                    });
                }
            });
        for value in &reading.metrics {
            ui.label(&value.label);
            metric_ui(ui, value, ui.available_width().min(400.0));
        }
    });
}

fn memory_overview(ui: &mut egui::Ui, reading: &DeviceReading) {
    ui.heading(&reading.device.name);
    for property in &reading.device.properties {
        ui.horizontal_wrapped(|ui| {
            ui.weak(format!("{}:", property.label));
            ui.label(property.value.as_deref().unwrap_or("недоступно"));
        });
    }
    ui.add_space(12.0);
    for value in &reading.metrics {
        ui.label(RichText::new(&value.label).strong());
        metric_ui(ui, value, ui.available_width().min(560.0));
    }
}

fn overview(ui: &mut egui::Ui, reading: &DeviceReading) {
    let model = reading
        .device
        .properties
        .iter()
        .find(|p| p.id == "model")
        .and_then(|p| p.value.as_deref())
        .unwrap_or("Модель CPU недоступна");
    ui.add_space(10.0);
    ui.heading(model);
    ui.horizontal_wrapped(|ui| {
        for property in reading.device.properties.iter().filter(|p| {
            matches!(
                p.id.as_str(),
                "vendor" | "architecture" | "logical_processors" | "physical_cores"
            )
        }) {
            ui.label(format!(
                "{}: {}",
                property.label,
                property.value.as_deref().unwrap_or("недоступно")
            ));
        }
    });
    egui::CollapsingHeader::new("Характеристики CPU: кеши, частоты, виртуализация и лимиты")
        .default_open(true)
        .show(ui, |ui| {
            for property in reading.device.properties.iter().filter(|p| {
                !matches!(
                    p.id.as_str(),
                    "model" | "vendor" | "architecture" | "logical_processors" | "physical_cores"
                )
            }) {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{}:", property.label));
                    ui.label(property.value.as_deref().unwrap_or("недоступно"));
                });
            }
        });
    ui.add_space(12.0);
    for value in &reading.metrics {
        ui.label(RichText::new(&value.label).strong());
        metric_ui(ui, value, ui.available_width().min(560.0));
    }
}

fn metric_ui(ui: &mut egui::Ui, value: &Metric, width: f32) {
    if matches!(value.id.as_str(), "smart_failed" | "nvme_warning")
        && value.value == MetricValue::Available(1.0)
    {
        ui.colored_label(Color32::LIGHT_RED, metric(value));
        return;
    }
    if value.unit == Unit::Percent
        && let MetricValue::Available(number) = value.value
        && number.is_finite()
    {
        ui.add(
            egui::ProgressBar::new((number / 100.0).clamp(0.0, 1.0) as f32)
                .desired_width(width)
                .fill(ACCENT)
                .text(metric(value)),
        );
        return;
    }
    let text = metric(value);
    match value.value {
        MetricValue::Available(number) if number.is_finite() => {
            ui.label(RichText::new(text).size(17.0).color(ACCENT));
        }
        MetricValue::PermissionDenied
        | MetricValue::ReadError
        | MetricValue::InvalidData
        | MetricValue::Available(_) => {
            ui.colored_label(Color32::LIGHT_YELLOW, text);
        }
        _ => {
            ui.weak(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Device, Property};

    fn text_from_shapes(shapes: &[egui::epaint::ClippedShape]) -> String {
        fn collect(shape: &egui::Shape, output: &mut String) {
            match shape {
                egui::Shape::Text(text) => {
                    output.push_str(text.galley.text());
                    output.push('\n');
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, output);
                    }
                }
                _ => {}
            }
        }
        let mut output = String::new();
        for shape in shapes {
            collect(&shape.shape, &mut output);
        }
        output
    }

    #[test]
    fn window_renders_real_zero_waiting_permission_errors_and_missing_sensors() {
        let ctx = egui::Context::default();
        let mut root = DeviceReading {
            device: Device {
                id: "cpu".into(),
                parent_id: None,
                kind: DeviceKind::Cpu,
                name: "CPU".into(),
                properties: vec![Property {
                    id: "model".into(),
                    label: "Модель".into(),
                    value: Some("Fixture Intel CPU".into()),
                }],
            },
            metrics: vec![
                Metric {
                    id: "usage".into(),
                    label: "Общая загрузка".into(),
                    unit: Unit::Percent,
                    value: MetricValue::Waiting,
                },
                Metric {
                    id: "power".into(),
                    label: "Потребление CPU".into(),
                    unit: Unit::Watt,
                    value: MetricValue::PermissionDenied,
                },
                Metric {
                    id: "temperature".into(),
                    label: "Температура CPU".into(),
                    unit: Unit::Celsius,
                    value: MetricValue::NotSupported,
                },
            ],
        };
        let mut app = CpuApp {
            target: "cpu".into(),
            interval: Duration::from_secs(1),
            worker: None,
            snapshot: None,
            received: None,
            error: None,
        };
        let mut render = |reading: DeviceReading| {
            app.snapshot = Some(MonitorSnapshot {
                interval: Some(Duration::from_secs(1)),
                devices: vec![reading],
            });
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1040.0, 780.0),
                )),
                ..Default::default()
            };
            let output = ctx.run(input, |ctx| app.show(ctx));
            text_from_shapes(&output.shapes)
        };
        let first = render(root.clone());
        assert!(first.contains("Fixture Intel CPU"));
        assert!(first.contains("ожидание второго замера"));
        assert!(first.contains("нет доступа"));
        assert!(first.contains("не поддерживается"));
        assert!(!first.contains("0.0 Вт"));
        root.metrics[0].value = MetricValue::Available(0.0);
        root.metrics[1].value = MetricValue::Available(12.5);
        root.metrics[2].value = MetricValue::ReadError;
        let next = render(root);
        assert!(next.contains("0.0 %"));
        assert!(next.contains("12.5 Вт"));
        assert!(next.contains("ошибка чтения"));
        assert!(!next.contains("нет доступа"));
    }

    #[test]
    fn gpu_card_labels_temperatures_and_does_not_show_unavailable_sensors_as_zero() {
        let ctx = egui::Context::default();
        let reading = DeviceReading {
            device: Device {
                id: "gpu/test".into(),
                parent_id: Some("gpu".into()),
                kind: DeviceKind::Gpu,
                name: "Fixture GPU".into(),
                properties: Vec::new(),
            },
            metrics: vec![
                Metric {
                    id: "temperature".into(),
                    label: "GPU Temperature".into(),
                    unit: Unit::Celsius,
                    value: MetricValue::Available(45.0),
                },
                Metric {
                    id: "power".into(),
                    label: "Потребление SoC".into(),
                    unit: Unit::Watt,
                    value: MetricValue::PermissionDenied,
                },
                Metric {
                    id: "memory_temperature".into(),
                    label: "Memory Junction".into(),
                    unit: Unit::Celsius,
                    value: MetricValue::NotSupported,
                },
            ],
        };
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| gpu_card(ui, &reading));
        });
        let text = text_from_shapes(&output.shapes);
        for expected in ["Fixture GPU", "GPU Temperature", "45.0 °C", "нет доступа"] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(!text.contains("0.0 °C"));
        assert!(!text.contains("Модель CPU недоступна"));
    }

    #[test]
    fn storage_cards_render_capacity_mounts_and_health_errors() {
        let ctx = egui::Context::default();
        let reading = DeviceReading {
            device: Device {
                id: "storage:test".into(),
                parent_id: None,
                kind: DeviceKind::BlockDevice,
                name: "Fixture NVMe".into(),
                properties: vec![Property {
                    id: "mountpoints".into(),
                    label: "Точки монтирования".into(),
                    value: Some("/data".into()),
                }],
            },
            metrics: vec![
                Metric {
                    id: "smart_failed".into(),
                    label: "SMART сообщает о неисправности".into(),
                    unit: Unit::Boolean,
                    value: MetricValue::PermissionDenied,
                },
                Metric {
                    id: "size".into(),
                    label: "Ёмкость".into(),
                    unit: Unit::Byte,
                    value: MetricValue::Available(1073741824.0),
                },
            ],
        };
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| storage_card(ui, &reading));
        });
        let text = text_from_shapes(&output.shapes);
        for expected in ["Fixture NVMe", "/data", "нет доступа", "1.00 GiB"] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(!text.contains("Модель CPU недоступна"));
    }

    #[test]
    fn ram_window_renders_capacity_usage_and_swap_with_labels() {
        let ctx = egui::Context::default();
        let raw = crate::raw::memory::RawMemorySample {
            at: Instant::now(),
            meminfo: Ok(
                "MemTotal: 1048576 kB\nMemAvailable: 524288 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n"
                    .into(),
            ),
        };
        let data = crate::processing::memory::process(&raw);
        let root = crate::processing::memory::device(&data);
        let mut app = CpuApp {
            target: "ram".into(),
            interval: Duration::from_secs(1),
            worker: None,
            snapshot: Some(MonitorSnapshot {
                interval: None,
                devices: crate::processing::memory::readings(&root, data),
            }),
            received: None,
            error: None,
        };
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1040.0, 780.0),
            )),
            ..Default::default()
        };
        let output = ctx.run(input, |ctx| app.show(ctx));
        let text = text_from_shapes(&output.shapes);
        for expected in [
            "Оперативная память (RAM)",
            "Всего RAM (доступно Linux)",
            "1.00 GiB",
            "Использовано RAM",
            "Доступно RAM",
            "512.00 MiB",
            "50.0 %",
            "Всего swap",
            "Использовано swap",
            "0 B",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
        assert!(!text.contains("Модель CPU недоступна"));
    }
}
