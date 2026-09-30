use crate::helpers::{average_u32, copy_to_4_bytes};

use macmon::sources::IOHIDSensors;
use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Once,
};
use sysinfo::MemoryRefreshKind;

use core_foundation_sys::{
    array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef},
    base::{CFRelease, CFTypeRef},
    dictionary::{CFDictionaryGetValue, CFDictionaryRef},
    number::{kCFNumberIntType, CFNumberGetValue, CFNumberRef},
    string::{kCFStringEncodingUTF8, CFStringCreateWithCString, CFStringRef},
};

use std::ffi::CString;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOPSCopyPowerSourcesInfo() -> CFTypeRef;
    fn IOPSCopyPowerSourcesList(blob: CFTypeRef) -> CFArrayRef;
    fn IOPSGetPowerSourceDescription(blob: CFTypeRef, ps: CFTypeRef) -> CFDictionaryRef;
}

static TELEMETRY_START: Once = Once::new();
static TELEMETRY_ENABLED: AtomicBool = AtomicBool::new(false);

static GPU_USAGE: AtomicU8 = AtomicU8::new(u8::MAX);
static BATTERY_USAGE: AtomicU8 = AtomicU8::new(u8::MAX);

static CPU_TEMP: AtomicU8 = AtomicU8::new(u8::MAX);
static GPU_TEMP: AtomicU8 = AtomicU8::new(u8::MAX);
static SSD_TEMP: AtomicU8 = AtomicU8::new(u8::MAX);
static BATTERY_TEMP: AtomicU8 = AtomicU8::new(u8::MAX);

#[derive(Serialize, Debug, Clone)]
pub struct SystemInfo {
    pub cpu_usage: u8,

    pub ram_max: u16,
    pub ram_usage: u8,
    pub ram_unit: [u8; 4],

    pub gpu_usage: u8,
    pub battery_usage: u8,

    pub cpu_temp: u8,
    pub gpu_temp: u8,
    pub ssd_temp: u8,
    pub battery_temp: u8,
}

fn cf_string(value: &str) -> Option<CFStringRef> {
    let cstr = CString::new(value).ok()?;

    let value = unsafe {
        CFStringCreateWithCString(std::ptr::null(), cstr.as_ptr(), kCFStringEncodingUTF8)
    };

    if value.is_null() {
        None
    } else {
        Some(value)
    }
}

fn read_native_battery_percent() -> Option<u8> {
    unsafe {
        let info = IOPSCopyPowerSourcesInfo();

        if info.is_null() {
            return None;
        }

        let sources = IOPSCopyPowerSourcesList(info);

        if sources.is_null() {
            CFRelease(info);
            return None;
        }

        let current_key = cf_string("Current Capacity")?;
        let max_key = cf_string("Max Capacity")?;

        let mut result = None;
        let count = CFArrayGetCount(sources);

        for index in 0..count {
            let ps = CFArrayGetValueAtIndex(sources, index) as CFTypeRef;

            if ps.is_null() {
                continue;
            }

            let description = IOPSGetPowerSourceDescription(info, ps);

            if description.is_null() {
                continue;
            }

            let current_ref = CFDictionaryGetValue(description, current_key as _) as CFNumberRef;

            let max_ref = CFDictionaryGetValue(description, max_key as _) as CFNumberRef;

            if current_ref.is_null() || max_ref.is_null() {
                continue;
            }

            let mut current: i32 = 0;
            let mut maximum: i32 = 0;

            let current_ok =
                CFNumberGetValue(current_ref, kCFNumberIntType, &mut current as *mut _ as _);

            let max_ok = CFNumberGetValue(max_ref, kCFNumberIntType, &mut maximum as *mut _ as _);

            if current_ok && max_ok && maximum > 0 {
                let percentage = (current as f64 / maximum as f64 * 100.0)
                    .round()
                    .clamp(0.0, 100.0) as u8;

                result = Some(percentage);
                break;
            }
        }

        CFRelease(current_key as _);
        CFRelease(max_key as _);
        CFRelease(sources as _);
        CFRelease(info);

        result
    }
}

fn temperature_to_u8(value: f32) -> u8 {
    if value.is_finite() && value > 0.0 && value < 150.0 {
        value.round() as u8
    } else {
        u8::MAX
    }
}

fn max_matching_temperature<F>(sensors: &[(String, f32)], matches: F) -> u8
where
    F: Fn(&str) -> bool,
{
    sensors
        .iter()
        .filter(|(name, temp)| matches(name) && temp.is_finite() && *temp > 0.0 && *temp < 150.0)
        .map(|(_, temp)| *temp)
        .reduce(f32::max)
        .map(temperature_to_u8)
        .unwrap_or(u8::MAX)
}

pub fn set_telemetry_enabled(enabled: bool) {
    TELEMETRY_ENABLED.store(enabled, Ordering::Relaxed);
}

fn start_apple_telemetry_worker() {
    TELEMETRY_START.call_once(|| {
        std::thread::spawn(|| {
            let mut gpu_sampler = match macmon::Sampler::new() {
                Ok(sampler) => {
                    eprintln!("macmon GPU sampler initialized");
                    Some(sampler)
                }
                Err(err) => {
                    eprintln!("macmon initialization failed: {err}");
                    None
                }
            };

            let hid_sensors = match IOHIDSensors::new_filtered(|name| {
                name.starts_with("pACC MTR Temp Sensor")
                    || name.starts_with("eACC MTR Temp Sensor")
                    || name.starts_with("GPU MTR Temp Sensor")
                    || name == "NAND CH0 temp"
                    || name == "gas gauge battery"
            }) {
                Ok(sensors) => {
                    eprintln!(
                        "macmon cached IOHID sensors initialized: {} sensors",
                        sensors.sensor_count()
                    );
                    Some(sensors)
                }
                Err(err) => {
                    eprintln!("macmon IOHID temperature initialization failed: {err}");
                    None
                }
            };

            let mut battery_poll_counter: u8 = 0;

            loop {
                if !TELEMETRY_ENABLED.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                }

                /*
                 * GPU utilization.
                 *
                 * Persistent 1-second sampler.
                 */
                if let Some(sampler) = gpu_sampler.as_mut() {
                    match sampler.get_metrics(1000) {
                        Ok(metrics) => {
                            let usage =
                                (metrics.gpu_active_ratio * 100.0).round().clamp(0.0, 100.0) as u8;

                            GPU_USAGE.store(usage, Ordering::Relaxed);
                        }

                        Err(err) => {
                            eprintln!("macmon sampling error: {err}");

                            std::thread::sleep(std::time::Duration::from_secs(1));
                        }
                    }
                } else {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }

                /*
                 * Raw IOHID temperatures.
                 *
                 * Verified on this MacBookAir10,1:
                 *
                 * CPU:
                 *   pACC MTR Temp Sensor*
                 *   eACC MTR Temp Sensor*
                 *
                 * GPU:
                 *   GPU MTR Temp Sensor*
                 *
                 * SSD:
                 *   NAND CH0 temp
                 *
                 * Battery:
                 *   gas gauge battery
                 */

                /*
                 * Native macOS battery percentage.
                 * Battery charge changes slowly, so read it
                 * once every ~10 telemetry cycles.
                 */
                if battery_poll_counter == 0 {
                    if let Some(percentage) = read_native_battery_percent() {
                        BATTERY_USAGE.store(percentage, Ordering::Relaxed);
                    }
                }

                battery_poll_counter = (battery_poll_counter + 1) % 10;

                if let Some(hid) = hid_sensors.as_ref() {
                    let raw = hid.get_metrics();

                    let cpu = max_matching_temperature(&raw, |name| {
                        name.starts_with("pACC MTR Temp Sensor")
                            || name.starts_with("eACC MTR Temp Sensor")
                    });

                    let gpu = max_matching_temperature(&raw, |name| {
                        name.starts_with("GPU MTR Temp Sensor")
                    });

                    let ssd = max_matching_temperature(&raw, |name| name == "NAND CH0 temp");

                    let battery =
                        max_matching_temperature(&raw, |name| name == "gas gauge battery");

                    CPU_TEMP.store(cpu, Ordering::Relaxed);

                    GPU_TEMP.store(gpu, Ordering::Relaxed);

                    SSD_TEMP.store(ssd, Ordering::Relaxed);

                    BATTERY_TEMP.store(battery, Ordering::Relaxed);
                }

                /*
                 * Battery percentage is updated separately
                 * from native macOS power-source telemetry.
                 */
            }
        });
    });
}

impl SystemInfo {
    fn get_unit(exp: u32) -> String {
        match exp {
            0 => "B",
            1 => "KB",
            2 => "MB",
            3 => "GB",
            4 => "TB",
            _ => "UB",
        }
        .to_owned()
    }

    fn get_exp(num: u64, base: u64) -> u32 {
        match num {
            x if x > u64::pow(base, 4) => 4,
            x if x > u64::pow(base, 3) => 3,
            x if x > u64::pow(base, 2) => 2,
            x if x > base => 1,
            _ => 0,
        }
    }

    pub async fn get_system_info(system_info: &mut sysinfo::System) -> Self {
        start_apple_telemetry_worker();

        system_info.refresh_memory_specifics(MemoryRefreshKind::new().with_ram());

        system_info.refresh_cpu_usage();

        let base = 1024u64;

        let ram_max = system_info.total_memory();
        let ram_exp = Self::get_exp(ram_max, base);

        let ram_usage = if ram_max > 0 {
            (system_info.used_memory() as f64 / ram_max as f64 * 100.0) as u8
        } else {
            0
        };

        SystemInfo {
            cpu_usage: average_u32(system_info.cpus().iter().map(|cpu| cpu.cpu_usage() as u32))
                .min(100) as u8,

            ram_max: (ram_max as f64 / u64::pow(base, ram_exp) as f64 * 10.0) as u16,

            ram_usage,

            ram_unit: copy_to_4_bytes(Self::get_unit(ram_exp).as_bytes()),

            gpu_usage: GPU_USAGE.load(Ordering::Relaxed),

            battery_usage: BATTERY_USAGE.load(Ordering::Relaxed),

            cpu_temp: CPU_TEMP.load(Ordering::Relaxed),

            gpu_temp: GPU_TEMP.load(Ordering::Relaxed),

            ssd_temp: SSD_TEMP.load(Ordering::Relaxed),

            battery_temp: BATTERY_TEMP.load(Ordering::Relaxed),
        }
    }
}
