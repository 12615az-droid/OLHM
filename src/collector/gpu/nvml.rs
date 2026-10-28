//! Optional NVML read-only adapter. No SDK installation or nvidia-smi subprocess.
use super::*;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
type Device = *mut c_void;
type Ret = c_int;
#[repr(C)]
#[derive(Default)]
struct Memory {
    total: u64,
    free: u64,
    used: u64,
}
#[repr(C)]
#[derive(Default)]
struct Util {
    gpu: u32,
    memory: u32,
}
#[repr(C)]
struct Fan {
    version: u32,
    fan: u32,
    speed: u32,
}
#[repr(C)]
#[derive(Default)]
struct Field {
    id: u32,
    scope: u32,
    timestamp: i64,
    latency: i64,
    kind: u32,
    status: i32,
    value: u64,
}

pub(super) struct Nvml {
    library: *mut c_void,
    driver_version: Option<String>,
}
fn status(code: Ret) -> Result<(), GpuError> {
    match code {
        0 => Ok(()),
        3 => Err(GpuError::NotSupported),
        4 => Err(GpuError::PermissionDenied),
        9 | 15 => Err(GpuError::NotAvailable),
        _ => Err(GpuError::ReadError(format!("NVML error {code}"))),
    }
}
impl Nvml {
    pub(super) fn open() -> Result<Self, GpuError> {
        let mut library = std::ptr::null_mut();
        // Try only vendor/system locations; do not search arbitrary working-directory files.
        for name in [
            "libnvidia-ml.so.1",
            "/run/opengl-driver/lib/libnvidia-ml.so.1",
            "/usr/lib/x86_64-linux-gnu/libnvidia-ml.so.1",
            "/usr/lib64/libnvidia-ml.so.1",
        ] {
            let name = CString::new(name).unwrap();
            // SAFETY: NUL-terminated filename, standard dlopen flags, handle owned below.
            library = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LOCAL | libc::RTLD_NOW) };
            if !library.is_null() {
                break;
            }
        }
        if library.is_null() {
            return Err(GpuError::NotAvailable);
        }
        let mut result = Self {
            library,
            driver_version: None,
        };
        // SAFETY: these are documented NVML ABI signatures; no optional function is called when absent.
        let init = unsafe { result.symbol::<unsafe extern "C" fn() -> Ret>(c"nvmlInit_v2") };
        let initialized = init.and_then(|f| status(unsafe { f() }));
        if let Err(e) = initialized {
            // No nvmlShutdown after failed initialization.
            unsafe { libc::dlclose(result.library) };
            std::mem::forget(result);
            return Err(e);
        }
        result.driver_version = result.string(c"nvmlSystemGetDriverVersion");
        Ok(result)
    }
    unsafe fn symbol<T: Copy>(&self, name: &CStr) -> Result<T, GpuError> {
        // SAFETY: handle is live; requested symbols/signatures are private constants in this adapter.
        let pointer = unsafe { libc::dlsym(self.library, name.as_ptr()) };
        if pointer.is_null() {
            return Err(GpuError::NotSupported);
        }
        assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<*mut c_void>());
        Ok(unsafe { std::mem::transmute_copy(&pointer) })
    }
    fn string(&self, name: &CStr) -> Option<String> {
        let f = unsafe { self.symbol::<unsafe extern "C" fn(*mut c_char, c_uint) -> Ret>(name) }
            .ok()?;
        let mut bytes = [0i8; 256];
        status(unsafe { f(bytes.as_mut_ptr(), bytes.len() as u32) }).ok()?;
        // Bounded conversion: a buggy/old driver cannot cause an unbounded CStr read.
        let bytes: Vec<_> = bytes
            .iter()
            .take_while(|b| **b != 0)
            .map(|b| *b as u8)
            .collect();
        String::from_utf8(bytes).ok()
    }
    fn number(&self, device: Device, name: &CStr) -> Result<String, GpuError> {
        let f = unsafe { self.symbol::<unsafe extern "C" fn(Device, *mut c_uint) -> Ret>(name) }?;
        let mut value = 0;
        status(unsafe { f(device, &mut value) })?;
        Ok(value.to_string())
    }
    fn indexed(&self, device: Device, name: &CStr, index: u32) -> Result<String, GpuError> {
        let f = unsafe {
            self.symbol::<unsafe extern "C" fn(Device, c_uint, *mut c_uint) -> Ret>(name)
        }?;
        let mut value = 0;
        status(unsafe { f(device, index, &mut value) })?;
        Ok(value.to_string())
    }
    fn mask(&self, device: Device, new: &CStr, old: &CStr) -> Result<u64, GpuError> {
        let f = unsafe { self.symbol::<unsafe extern "C" fn(Device, *mut u64) -> Ret>(new) }
            .or_else(|_| unsafe {
                self.symbol::<unsafe extern "C" fn(Device, *mut u64) -> Ret>(old)
            })?;
        let mut value = 0;
        status(unsafe { f(device, &mut value) })?;
        Ok(value)
    }
    pub(super) fn sample(&self, raw: &mut RawGpu) {
        let result = (|| {
            let pci = raw
                .metadata
                .get("pci")
                .filter(|s| valid_bdf(s))
                .ok_or(GpuError::NotSupported)?;
            let pci = CString::new(pci.as_str()).map_err(|_| GpuError::InvalidData)?;
            let f = unsafe {
                self.symbol::<unsafe extern "C" fn(*const c_char, *mut Device) -> Ret>(
                    c"nvmlDeviceGetHandleByPciBusId_v2",
                )
            }?;
            let mut device = std::ptr::null_mut();
            status(unsafe { f(pci.as_ptr(), &mut device) })?;
            if device.is_null() {
                return Err(GpuError::InvalidData);
            }
            Ok(device)
        })();
        let device = match result {
            Ok(device) => device,
            Err(e) => {
                for (id, unit) in [
                    ("load", GpuUnit::Percent),
                    ("memory_load", GpuUnit::Percent),
                    ("power", GpuUnit::MilliWatt),
                    ("temperature", GpuUnit::Celsius),
                ] {
                    if !raw.values.contains_key(id) {
                        put(raw, id, unit, Err(e.clone()), "NVML device lookup by PCI");
                    }
                }
                raw.metadata.insert("nvml".into(), format!("{e:?}"));
                return;
            }
        };
        raw.metadata.insert(
            "nvml".into(),
            "доступна; устройство сопоставлено по PCI BDF".into(),
        );
        if !raw.metadata.contains_key("nvml_name")
            && let Ok(f) = unsafe {
                self.symbol::<unsafe extern "C" fn(Device, *mut c_char, c_uint) -> Ret>(
                    c"nvmlDeviceGetName",
                )
            }
        {
            let mut bytes = [0i8; 256];
            if status(unsafe { f(device, bytes.as_mut_ptr(), 256) }).is_ok() {
                let bytes = bytes
                    .into_iter()
                    .take_while(|b| *b != 0)
                    .map(|b| b as u8)
                    .collect();
                if let Ok(name) = String::from_utf8(bytes) {
                    raw.metadata.insert("model".into(), name.clone());
                    raw.metadata.insert("nvml_name".into(), name);
                    raw.metadata.insert("model_source".into(), "NVML".into());
                }
            }
        }
        if let Some(version) = &self.driver_version {
            raw.metadata
                .insert("driver_version".into(), version.clone());
        }
        for (id, name, unit) in [
            ("power", c"nvmlDeviceGetPowerUsage", GpuUnit::MilliWatt),
            (
                "power_limit",
                c"nvmlDeviceGetPowerManagementLimit",
                GpuUnit::MilliWatt,
            ),
        ] {
            put(
                raw,
                id,
                unit,
                self.number(device, name),
                name.to_str().unwrap(),
            );
        }
        raw.metadata.insert("power_scope".into(), "board".into());
        raw.metadata
            .insert("power_limit_scope".into(), "board".into());
        raw.metadata
            .insert("power_domain".into(), "NVML device".into());
        raw.metadata
            .insert("power_limit_domain".into(), "NVML device".into());
        raw.metadata.insert(
            "power_sampling".into(),
            "NVML: instant/driver average (обычно 1 с на Ampere+)".into(),
        );
        if !raw.metadata.contains_key("memory_kind") {
            raw.metadata
                .insert("memory_kind".into(), "framebuffer".into());
        }
        let memory = (|| {
            let f = unsafe {
                self.symbol::<unsafe extern "C" fn(Device, *mut Memory) -> Ret>(
                    c"nvmlDeviceGetMemoryInfo",
                )
            }?;
            let mut memory = Memory::default();
            status(unsafe { f(device, &mut memory) })?;
            Ok(memory)
        })();
        for (id, value) in [
            ("memory_total", memory.as_ref().map(|m| m.total)),
            ("memory_used", memory.as_ref().map(|m| m.used)),
            ("memory_free", memory.as_ref().map(|m| m.free)),
        ] {
            put(
                raw,
                id,
                GpuUnit::Byte,
                value.map(|v| v.to_string()).map_err(Clone::clone),
                "NVML framebuffer memory",
            );
        }
        let usage = (|| {
            let f = unsafe {
                self.symbol::<unsafe extern "C" fn(Device, *mut Util) -> Ret>(
                    c"nvmlDeviceGetUtilizationRates",
                )
            }?;
            let mut util = Util::default();
            status(unsafe { f(device, &mut util) })?;
            Ok(util)
        })();
        for (id, value) in [
            ("load", usage.as_ref().map(|u| u.gpu)),
            ("memory_load", usage.as_ref().map(|u| u.memory)),
        ] {
            put(
                raw,
                id,
                GpuUnit::Percent,
                value.map(|v| v.to_string()).map_err(Clone::clone),
                "NVML utilization (собственное окно драйвера)",
            );
        }
        for (id, name) in [
            ("encode", c"nvmlDeviceGetEncoderUtilization"),
            ("decode", c"nvmlDeviceGetDecoderUtilization"),
        ] {
            let value = (|| {
                let f = unsafe {
                    self.symbol::<unsafe extern "C" fn(Device, *mut u32, *mut u32) -> Ret>(name)
                }?;
                let mut value = 0;
                let mut period = 0;
                status(unsafe { f(device, &mut value, &mut period) })?;
                Ok(value.to_string())
            })();
            put(raw, id, GpuUnit::Percent, value, name.to_str().unwrap());
        }
        for (id, name, index, unit) in [
            (
                "temperature",
                c"nvmlDeviceGetTemperature",
                0,
                GpuUnit::Celsius,
            ),
            (
                "temperature_limit",
                c"nvmlDeviceGetTemperatureThreshold",
                3,
                GpuUnit::Celsius,
            ),
            (
                "core_clock",
                c"nvmlDeviceGetClockInfo",
                0,
                GpuUnit::Megahertz,
            ),
            (
                "memory_clock",
                c"nvmlDeviceGetClockInfo",
                2,
                GpuUnit::Megahertz,
            ),
        ] {
            put(
                raw,
                id,
                unit,
                self.indexed(device, name, index),
                name.to_str().unwrap(),
            );
        }
        let temperature = (|| {
            let f = unsafe {
                self.symbol::<unsafe extern "C" fn(Device, c_int, *mut Field) -> Ret>(
                    c"nvmlDeviceGetFieldValues",
                )
            }?;
            let mut field = Field {
                id: 82,
                ..Default::default()
            };
            status(unsafe { f(device, 1, &mut field) })?;
            status(field.status)?;
            // The ABI union is read only for the type actually returned by NVML.
            match field.kind {
                1 => Ok((field.value as u32).to_string()),
                2 | 3 => Ok(field.value.to_string()),
                _ => Err(GpuError::InvalidData),
            }
        })();
        put(
            raw,
            "memory_temperature",
            GpuUnit::Celsius,
            temperature,
            "NVML memory temperature field 82",
        );
        let active = self.mask(
            device,
            c"nvmlDeviceGetCurrentClocksEventReasons",
            c"nvmlDeviceGetCurrentClocksThrottleReasons",
        );
        let supported = self.mask(
            device,
            c"nvmlDeviceGetSupportedClocksEventReasons",
            c"nvmlDeviceGetSupportedClocksThrottleReasons",
        );
        for (id, bits) in [
            ("nvml_active_reasons", active),
            ("nvml_supported_reasons", supported),
        ] {
            put(
                raw,
                id,
                GpuUnit::Bits,
                bits.map(|bits| bits.to_string()),
                "NVML current/supported clock event reasons",
            );
        }
        let fans = self
            .number(device, c"nvmlDeviceGetNumFans")
            .and_then(|s| s.parse::<u32>().map_err(|_| GpuError::InvalidData));
        if let Ok(count) = fans {
            for fan in 0..count.min(32) {
                let value = self.indexed(device, c"nvmlDeviceGetFanSpeed_v2", fan);
                put(
                    raw,
                    &format!("fan{}_percent", fan + 1),
                    GpuUnit::Percent,
                    value,
                    "NVML individual intended operating fan speed %",
                );
                let rpm = (|| {
                    let f = unsafe {
                        self.symbol::<unsafe extern "C" fn(Device, *mut Fan) -> Ret>(
                            c"nvmlDeviceGetFanSpeedRPM",
                        )
                    }?;
                    let mut value = Fan {
                        version: 12 | (1 << 24),
                        fan,
                        speed: 0,
                    };
                    status(unsafe { f(device, &mut value) })?;
                    Ok(value.speed.to_string())
                })();
                put(
                    raw,
                    &format!("fan{}_rpm", fan + 1),
                    GpuUnit::Rpm,
                    rpm,
                    "NVML individual fan RPM",
                );
            }
        } else {
            put(
                raw,
                "fan1_percent",
                GpuUnit::Percent,
                self.number(device, c"nvmlDeviceGetFanSpeed"),
                "NVML legacy fan % (без независимой привязки)",
            );
        }
    }
}
impl Drop for Nvml {
    fn drop(&mut self) {
        if let Ok(shutdown) =
            unsafe { self.symbol::<unsafe extern "C" fn() -> Ret>(c"nvmlShutdown") }
        {
            let _ = unsafe { shutdown() };
        }
        // SAFETY: all function calls have completed and this object uniquely owns the dlopen handle.
        unsafe { libc::dlclose(self.library) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "compile tests/fixtures/nvml_mock.c and set OLHM_TEST_NVML_LIBRARY"]
    fn nvml_mock_library_exercises_real_ffi_and_processing_without_hardware() {
        let path = CString::new(
            std::env::var("OLHM_TEST_NVML_LIBRARY").expect("path to compiled mock NVML"),
        )
        .unwrap();
        // SAFETY: the explicit test-only library exports the documented NVML signatures.
        let library = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_LOCAL | libc::RTLD_NOW) };
        assert!(!library.is_null());
        let mut nvml = Nvml {
            library,
            driver_version: None,
        };
        let init = unsafe { nvml.symbol::<unsafe extern "C" fn() -> Ret>(c"nvmlInit_v2") }.unwrap();
        assert_eq!(status(unsafe { init() }), Ok(()));
        nvml.driver_version = nvml.string(c"nvmlSystemGetDriverVersion");
        let mut raw = RawGpu {
            identity: "nvidia-fixture".into(),
            metadata: BTreeMap::from([
                ("pci".into(), "0000:01:00.0".into()),
                ("vendor_id".into(), "0x10de".into()),
            ]),
            values: BTreeMap::new(),
            engines: BTreeMap::new(),
            amd_metrics: Err(GpuError::NotSupported),
            energy_at: None,
        };
        nvml.sample(&mut raw);
        assert_eq!(raw.metadata["driver_version"], "999.123");
        assert_eq!(raw.metadata["model"], "Fixture NVIDIA");
        assert_eq!(raw.values["fan1_rpm"].value.as_deref(), Ok("0"));
        assert_eq!(raw.values["fan2_rpm"].value.as_deref(), Ok("1000"));
        let snapshot = crate::processing::gpu::GpuMonitor::default().update(RawGpuSample {
            at: Instant::now(),
            devices: Ok(vec![raw.clone()]),
        });
        let metrics = &snapshot.devices[1].metrics;
        for (id, expected) in [
            ("power", 75.0),
            ("power_usage", 50.0),
            ("memory_usage", 25.0),
            ("memory_temperature", 70.0),
            ("decode", 60.0),
            ("thermal_throttle", 1.0),
            ("power_throttle", 1.0),
        ] {
            assert_eq!(
                metrics.iter().find(|m| m.id == id).unwrap().value,
                crate::model::MetricValue::Available(expected),
                "{id}"
            );
        }
        assert_eq!(raw.values["encode"].value, Err(GpuError::NotSupported));
        raw.metadata.insert("pci".into(), "0000:02:00.0".into());
        raw.values.clear();
        nvml.sample(&mut raw);
        assert_eq!(raw.values["load"].value, Err(GpuError::PermissionDenied));
        put(
            &mut raw,
            "temperature",
            GpuUnit::Celsius,
            Ok("51".into()),
            "nouveau hwmon fallback",
        );
        nvml.sample(&mut raw);
        assert_eq!(raw.values["temperature"].value.as_deref(), Ok("51"));
    }

    #[test]
    fn ffi_layouts_and_error_codes_preserve_support_and_permissions() {
        assert_eq!(std::mem::size_of::<Memory>(), 24);
        assert_eq!(std::mem::size_of::<Util>(), 8);
        assert_eq!(std::mem::size_of::<Fan>(), 12);
        assert_eq!(std::mem::size_of::<Field>(), 40);
        assert_eq!(status(3), Err(GpuError::NotSupported));
        assert_eq!(status(4), Err(GpuError::PermissionDenied));
        assert_eq!(status(15), Err(GpuError::NotAvailable));
    }
}
