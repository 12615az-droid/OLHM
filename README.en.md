# OLHM — Open Linux Hardware Monitor

English | [Русский](README.md)

OLHM 0.7 is a Linux hardware monitor with command-line and graphical interfaces. It currently detects and monitors CPUs, RAM and swap, storage devices and file systems, and AMD, Intel, and NVIDIA GPUs. The implementation is split into Collection, Processing, and Presentation layers and uses a shared device and metric model.

OLHM reads standard Linux interfaces such as `/proc`, `/sys`, DRM, hwmon, and `statvfs`. Optional helpers add information when available: `lsblk` for storage metadata, `smartctl` for SMART data, and NVML from the installed NVIDIA driver. Basic monitoring does not require root privileges. Missing sensors, denied access, and unsupported values are reported explicitly instead of being replaced with zero.

## Current status

Implemented roadmap stages:

- **0.1–0.4:** CPU detection, monitoring, three-layer architecture, and the first GUI.
- **0.5:** RAM modules, SPD/SMBIOS information, temperatures, swap, and zram.
- **0.6:** storage devices, partitions, file systems, I/O rates, temperatures, and SMART health data.
- **0.7:** AMD, Intel, and NVIDIA GPU monitoring with multi-GPU support.

The project is under active development. Physical AMD GPUs have been tested on the development system. NVIDIA and Intel paths are covered by fixtures and API tests, but still need validation on a wider range of real hardware and distributions.

## Running the GUI

```sh
nix-shell --run 'cargo run --features gui -- gui cpu'
nix-shell --run 'cargo run --features gui -- gui ram'
nix-shell --run 'cargo run --features gui -- gui storage'
nix-shell --run 'cargo run --features gui -- gui gpu'
nix-shell --run 'cargo run --features gui -- gui gpu --interval 0.5'
```

The GUI uses `egui/eframe`, OpenGL, and either Wayland or X11. Collection runs in a background thread through `MonitorSession`; the window receives processed snapshots and never reads hardware interfaces directly. Only the latest frame is retained. Slow or failed polling is shown in the interface, and closing the window interrupts the polling wait.

The `gui` feature is disabled by default so the CLI can be built without graphical dependencies.

## Command-line interface

```sh
nix-shell --run 'cargo run -- help'
nix-shell --run 'cargo run -- info cpu'
nix-shell --run 'cargo run -- snapshot ram --interval 0.5'
nix-shell --run 'cargo run -- monitor storage --interval 1 --count 5'
nix-shell --run 'cargo run -- snapshot gpu --interval 1'
```

The same targets are available for `info`, `snapshot`, and `monitor`:

```text
cpu | ram | storage | gpu
```

- `info` prints static information and currently available capabilities.
- `snapshot` takes the readings needed for calculated metrics and prints one frame without an endless loop.
- `monitor` updates continuously until Ctrl+C; `--count` limits the number of frames.
- `--interval` accepts values from 0.1 to 60 seconds and defaults to 1 second.
- `collect cpu --format json` exports diagnostic raw Collection data.

When stdout is redirected or `TERM=dumb` is set, frames are printed sequentially without ANSI screen control codes. A closed pipe terminates text output cleanly.

## CPU monitoring

OLHM reports the CPU vendor, model, architecture, logical processor count, and physical core count when the topology is available. Dynamic monitoring includes:

- total and per-logical-CPU utilization calculated from two `/proc/stat` samples;
- current frequencies from the available kernel interfaces;
- supported CPU temperatures and critical thresholds;
- confirmed thermal throttling states where the driver exposes them;
- CPU package power in watts calculated from energy counters over a measured interval;
- minimum and maximum frequencies, cache hierarchy, and virtualization information when available.

Sensor ownership is verified before a value is attributed to the CPU. Motherboard or unrelated hwmon channels are not presented as CPU sensors based only on their label.

## RAM, modules, swap, and zram

System RAM and swap totals come from `/proc/meminfo`. Used RAM is calculated as `MemTotal − MemAvailable`; `MemFree` is not substituted for `MemAvailable`. Linux `kB` values are converted using a factor of 1024 and displayed as KiB, MiB, GiB, or TiB.

When accessible, SMBIOS Type 17 and kernel-published DDR4/DDR5 SPD data add:

- installed module count, slots, capacity, vendor, part number, and serial number;
- configured SMBIOS speed and SPD rated data rate;
- supported geometry and the SPD timings tAA, tRCD, tRP, and tRAS;
- module temperatures from confirmed `jc42` or `spd5118` devices.

SPD CRC is checked before base parameters are accepted. OLHM does not claim that SPD timings are the controller's active timings or that XMP/EXPO is enabled. SMBIOS and SPD records are merged only when their identity match is unambiguous.

Each active `/proc/swaps` entry is shown separately. zram is distinguished from disk-backed swap and reports virtual capacity, uncompressed and compressed data, allocated RAM, and compression algorithm when the kernel exports them. zram capacity is not added to physical RAM.

## Storage and file systems

Physical disks, logical block devices, partitions, and mounted file systems are represented separately. Available information includes model, serial number, capacity, transport, GPT/MBR layout, partition offsets, file-system type, label, UUID, and mount points.

Read and write rates are calculated from `/sys/class/block/*/stat` deltas. File-system capacity and available space are read with `statvfs` for the mount point. Network and FUSE mounts are listed without making a potentially blocking capacity request.

Temperatures come from hwmon devices whose ownership is confirmed, with SMART used as an optional fallback. If installed, `smartctl` JSON provides the device health verdict, NVMe critical warnings, remaining spare, wear indicators, and supported error counters. SMART is refreshed at most once per 60 seconds. OLHM performs read-only queries, does not start self-tests, and does not wake ATA disks that are in standby.

SMART `PASSED` is the drive's own threshold result, not a guarantee of health. OLHM does not invent a universal SSD health percentage.

## GPU monitoring

GPU discovery uses PCI and DRM and supports multiple integrated and discrete devices. `cardN` and `renderDN` indices are not used as stable identities. Sensor layouts are periodically rediscovered, while static names and bindings are cached.

Depending on hardware and driver support, OLHM reports:

- full GPU name, vendor, driver, and driver version;
- current and maximum PCIe link speed and link width;
- total GPU load, memory-controller activity, and video encode/decode activity;
- core, hot-spot, and memory-junction temperatures;
- current core and memory clocks;
- dedicated VRAM or shared UMA capacity, used/free memory, and utilization;
- board, package, chip, or SoC power with the measurement scope clearly identified;
- Power Limit and its utilization only when the power domains match;
- individual fan RPM and available PWM or relative level;
- thermal and power throttling only when the driver supplies an explicit reason.

Current PCIe values may drop while the device is idle because of link power management, so current and maximum speed/width are displayed separately.

### NVIDIA

NVML is loaded dynamically from `libnvidia-ml.so.1` supplied by the installed NVIDIA driver. OLHM does not invoke `nvidia-smi` and does not require the NVIDIA SDK at build time. Devices are matched by PCI BDF. If NVML is unavailable, the PCI device remains visible and any supported standard Linux sensors can still be used.

### AMD

The `amdgpu` and `radeon` paths use sysfs, hwmon, DRM, and versioned `gpu_metrics` layouts. Unknown metrics-table versions are not decoded by guessing. Power readings such as PPT on an APU may include CPU/SoC power and are labelled accordingly. VRAM and GTT/shared-memory budgets remain separate.

### Intel

The `i915` and `xe` paths use DRM memory-region queries, sysfs frequencies and throttle reasons, and available device-wide PMU engine counters. Integrated graphics system memory is presented as shared memory rather than physical VRAM. OLHM does not infer an overall GPU load by summing overlapping engine counters.

## Portable Linux packages

Prebuilt x86_64 archives are stored in `dist/`; checksums are in `dist/SHA256SUMS`.

- `olhm-0.6.0-linux-x86_64.tar.gz` is a static musl CLI package for testing on Debian/Ubuntu, Fedora, openSUSE, Arch, Alpine, and other x86_64 Linux systems.
- `olhm-0.7.0-gui-linux-x86_64.tar.gz` bundles the GUI launcher, glibc, and user-space X11/Wayland libraries. It uses OpenGL/EGL and the hardware driver from the host system.

Example:

```sh
tar -xzf dist/olhm-0.7.0-gui-linux-x86_64.tar.gz
cd olhm-0.7.0-gui-linux-x86_64
./AppRun
```

The portable GUI package targets modern glibc-based x86_64 distributions. Alpine without a compatible glibc graphical stack is not claimed as supported by this package.

## Building

With the project Nix environment:

```sh
nix-shell
cargo build --offline
cargo build --offline --features gui
```

Without Nix, install a current Rust toolchain and the development libraries required by `eframe` for X11/Wayland and OpenGL, then run the corresponding Cargo command. Runtime helpers `lsblk` and `smartctl` are optional.

The static CLI package can be built with:

```sh
nix-build portable.nix
```

## Architecture

OLHM keeps hardware access out of the interfaces:

1. **Collection** discovers devices and reads unprocessed Linux and vendor API values.
2. **Processing** validates readings, converts units, calculates deltas, and builds the shared model.
3. **Presentation** renders the shared model in the CLI or GUI.

Main modules:

- `src/collector/`: Linux and vendor-specific device discovery and collection.
- `src/raw/`: unprocessed readings with source, unit, and per-field error status.
- `src/processing/`: validation, calculations, device identity, and normalized metrics.
- `src/model.rs`: shared devices, properties, metrics, units, and availability states.
- `src/monitor.rs`: category sessions connecting Collection and Processing.
- `src/presentation/`: CLI formatting, GUI rendering, and the background GUI worker.

The CLI and GUI never read `/proc`, `/sys`, or vendor APIs directly. Collection does not depend on the GUI, and Processing does not depend on a presentation method.

## Validation

The project has fixture coverage for Intel, AMD, and NVIDIA GPU paths; CPU counter resets and hotplug; RAM/SPD/SMBIOS validation; swap and zram; storage topology, mount parsing, I/O deltas, and SMART interpretation. A test NVML shared library verifies the real dynamic-loading and FFI path without requiring NVIDIA hardware.

The development system has been used to validate AMD Ryzen CPU monitoring, two DDR5 SPD devices and their temperature sensors, NVMe/SATA/HDD discovery, file-system capacity, and simultaneous AMD Radeon RX 5700 XT plus integrated Radeon 610M monitoring. The GUI has been smoke-tested under Xvfb. Physical NVIDIA and Intel GPUs, Wayland, USB/SAS bridges, hotplug, and additional distributions still require real-hardware testing.

Known issues found during review are documented in [OLHM_0.6_CODE_REVIEW.md](OLHM_0.6_CODE_REVIEW.md). Unsupported or inaccessible hardware data should remain visible as a status and must not crash monitoring.

## Project scope

Until version 1.0, OLHM focuses on hardware detection, read-only metric collection, processing, and CLI/GUI presentation. It does not implement stress tests, benchmarks, overclocking, hardware control, or remote monitoring.

See [OLHM_Roadmap_to_1.0.md](OLHM_Roadmap_to_1.0.md) for the roadmap and planned post-1.0 configuration and localization stages.
