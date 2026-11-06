use serde::Serialize;

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use std::sync::Mutex;

/// Per-logical-core load and, when the machine exposes one, a Celsius temperature.
/// `load` and `temperatureC` are null when that reading is unavailable.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineStats {
    pub load: Option<Vec<f64>>,
    pub temperature_c: Option<f64>,
}

#[tauri::command]
pub fn machine_stats() -> MachineStats {
    MachineStats {
        load: per_core_load(),
        temperature_c: temperature_c(),
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", test))]
fn plausible_celsius(celsius: f64) -> Option<f64> {
    if !celsius.is_finite() || celsius <= 0.0 || celsius > 125.0 {
        return None;
    }
    let rounded = (celsius * 10.0).round() / 10.0;
    if rounded > 0.0 && rounded <= 125.0 {
        Some(rounded)
    } else {
        None
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn is_cpu_temperature_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "cpu", "pacc", "eacc", "peci", "tdie", "tdev", "core", "package", "pkg", "k10temp",
        "zenpower", "tctl",
    ]
    .iter()
    .any(|needle| name.contains(needle))
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn consider(best: &mut Option<f64>, sample: Option<f64>) {
    if let Some(sample) = sample {
        *best = Some(best.map_or(sample, |current| current.max(sample)));
    }
}

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows", test))]
fn utilization(idle_delta: u64, total_delta: u64) -> f64 {
    if total_delta == 0 {
        return 0.0;
    }
    let busy = total_delta.saturating_sub(idle_delta.min(total_delta));
    let percent = (busy as f64) * 100.0 / (total_delta as f64);
    (percent.clamp(0.0, 100.0) * 10.0).round() / 10.0
}

#[cfg(target_os = "macos")]
type TickSample = Vec<[u32; 4]>;

#[cfg(any(target_os = "linux", target_os = "windows"))]
type TickSample = Vec<CoreTicks>;

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
static PREVIOUS: Mutex<Option<TickSample>> = Mutex::new(None);

#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
fn per_core_load() -> Option<Vec<f64>> {
    let mut previous = PREVIOUS.lock().unwrap_or_else(|err| err.into_inner());
    let current = read_ticks()?;
    let loads = previous
        .as_ref()
        .and_then(|prior| loads_between(prior, &current));
    *previous = Some(current);
    loads
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn per_core_load() -> Option<Vec<f64>> {
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn temperature_c() -> Option<f64> {
    None
}

#[cfg(target_os = "macos")]
fn temperature_c() -> Option<f64> {
    hid_temperature().or_else(smc_temperature)
}

#[cfg(target_os = "linux")]
fn temperature_c() -> Option<f64> {
    let zones = thermal_zone_temperature();
    let hwmon = hwmon_temperature();
    match (zones, hwmon) {
        (Some(zones), Some(hwmon)) => Some(zones.max(hwmon)),
        (Some(zones), None) => Some(zones),
        (None, Some(hwmon)) => Some(hwmon),
        (None, None) => None,
    }
}

// mach/machine.h: CPU_STATE_USER, CPU_STATE_SYSTEM, CPU_STATE_IDLE, CPU_STATE_NICE.
#[cfg(target_os = "macos")]
const USER: usize = 0;
#[cfg(target_os = "macos")]
const SYSTEM: usize = 1;
#[cfg(target_os = "macos")]
const IDLE: usize = 2;
#[cfg(target_os = "macos")]
const NICE: usize = 3;
#[cfg(target_os = "macos")]
const PROCESSOR_CPU_LOAD_INFO: i32 = 2;

#[cfg(target_os = "macos")]
fn loads_between(previous: &[[u32; 4]], current: &[[u32; 4]]) -> Option<Vec<f64>> {
    if previous.len() != current.len() || current.is_empty() {
        return None;
    }
    Some(
        previous
            .iter()
            .zip(current)
            .map(|(before, after)| {
                let user = after[USER].wrapping_sub(before[USER]) as u64;
                let system = after[SYSTEM].wrapping_sub(before[SYSTEM]) as u64;
                let idle = after[IDLE].wrapping_sub(before[IDLE]) as u64;
                let nice = after[NICE].wrapping_sub(before[NICE]) as u64;
                let total = user
                    .wrapping_add(system)
                    .wrapping_add(idle)
                    .wrapping_add(nice);
                utilization(idle, total)
            })
            .collect(),
    )
}

#[cfg(target_os = "macos")]
fn read_ticks() -> Option<TickSample> {
    unsafe {
        let host = mach_host_self();
        if host == 0 {
            return None;
        }
        let mut cpu_count: u32 = 0;
        let mut info: *mut i32 = std::ptr::null_mut();
        let mut info_count: u32 = 0;
        let status = host_processor_info(
            host,
            PROCESSOR_CPU_LOAD_INFO,
            &mut cpu_count,
            &mut info,
            &mut info_count,
        );
        let guard = HostCpuInfo {
            host,
            info,
            info_count,
        };
        if status != 0 || guard.info.is_null() || cpu_count == 0 || cpu_count > 4096 {
            return None;
        }
        let states = 4usize;
        if (guard.info_count as usize) < cpu_count as usize * states {
            return None;
        }
        let mut cores = Vec::with_capacity(cpu_count as usize);
        for cpu in 0..cpu_count as usize {
            let base = cpu * states;
            cores.push([
                *guard.info.add(base + USER) as u32,
                *guard.info.add(base + SYSTEM) as u32,
                *guard.info.add(base + IDLE) as u32,
                *guard.info.add(base + NICE) as u32,
            ]);
        }
        Some(cores)
    }
}

#[cfg(target_os = "macos")]
struct HostCpuInfo {
    host: u32,
    info: *mut i32,
    info_count: u32,
}

#[cfg(target_os = "macos")]
impl Drop for HostCpuInfo {
    fn drop(&mut self) {
        unsafe {
            if !self.info.is_null() && self.info_count > 0 {
                vm_deallocate(
                    mach_task_self_,
                    self.info as usize,
                    self.info_count as usize * std::mem::size_of::<i32>(),
                );
            }
            if self.host != 0 {
                mach_port_deallocate(mach_task_self_, self.host);
            }
        }
    }
}

#[cfg(target_os = "macos")]
const TEMPERATURE_EVENT: u32 = 15;
#[cfg(target_os = "macos")]
const TEMPERATURE_FIELD: u32 = TEMPERATURE_EVENT << 16;
#[cfg(target_os = "macos")]
const CF_STRING_UTF8: u32 = 0x0800_0100;
#[cfg(target_os = "macos")]
const CF_NUMBER_SINT32: isize = 3;

#[cfg(target_os = "macos")]
fn hid_temperature() -> Option<f64> {
    let api = hid_api()?;
    let client = CfObject::new(unsafe { (api.create)(std::ptr::null()) })?;
    let matching = temperature_matching()?;
    unsafe { (api.set_matching)(client.as_ptr(), matching.as_ptr()) };
    let services = CfObject::new(unsafe { (api.copy_services)(client.as_ptr()) })?;
    let product = cf_string(c"Product")?;
    let count = unsafe { CFArrayGetCount(services.as_ptr()) };
    let mut best = None;
    for index in 0..count {
        let service = unsafe { CFArrayGetValueAtIndex(services.as_ptr(), index) };
        if service.is_null() {
            continue;
        }
        let Some(property) =
            CfObject::new(unsafe { (api.copy_property)(service, product.as_ptr()) })
        else {
            continue;
        };
        let Some(name) = cf_string_to_string(property.as_ptr()) else {
            continue;
        };
        if !is_cpu_temperature_name(&name) {
            continue;
        }
        let Some(event) = CfObject::new(unsafe {
            (api.copy_event)(service, TEMPERATURE_EVENT, std::ptr::null(), 0)
        }) else {
            continue;
        };
        let celsius = unsafe { (api.get_float)(event.as_ptr(), TEMPERATURE_FIELD) };
        consider(&mut best, plausible_celsius(celsius));
    }
    best
}

#[cfg(target_os = "macos")]
struct HidApi {
    create: unsafe extern "C" fn(*const std::ffi::c_void) -> *const std::ffi::c_void,
    set_matching: unsafe extern "C" fn(*const std::ffi::c_void, *const std::ffi::c_void),
    copy_services: unsafe extern "C" fn(*const std::ffi::c_void) -> *const std::ffi::c_void,
    copy_property: unsafe extern "C" fn(
        *const std::ffi::c_void,
        *const std::ffi::c_void,
    ) -> *const std::ffi::c_void,
    copy_event: unsafe extern "C" fn(
        *const std::ffi::c_void,
        u32,
        *const std::ffi::c_void,
        u32,
    ) -> *const std::ffi::c_void,
    get_float: unsafe extern "C" fn(*const std::ffi::c_void, u32) -> f64,
}

#[cfg(target_os = "macos")]
fn hid_api() -> Option<&'static HidApi> {
    static API: std::sync::OnceLock<Option<HidApi>> = std::sync::OnceLock::new();
    API.get_or_init(load_hid_api).as_ref()
}

#[cfg(target_os = "macos")]
fn load_hid_api() -> Option<HidApi> {
    unsafe {
        let handle = dlopen(
            c"/System/Library/Frameworks/IOKit.framework/IOKit".as_ptr(),
            1,
        );
        if handle.is_null() {
            return None;
        }
        let api = (|| {
            Some(HidApi {
                create: lookup(handle, c"IOHIDEventSystemClientCreate")?,
                set_matching: lookup(handle, c"IOHIDEventSystemClientSetMatching")?,
                copy_services: lookup(handle, c"IOHIDEventSystemClientCopyServices")?,
                copy_property: lookup(handle, c"IOHIDServiceClientCopyProperty")?,
                copy_event: lookup(handle, c"IOHIDServiceClientCopyEvent")?,
                get_float: lookup(handle, c"IOHIDEventGetFloatValue")?,
            })
        })();
        if api.is_none() {
            dlclose(handle);
        }
        // Leave IOKit mapped; these pointers are used for the process lifetime.
        api
    }
}

#[cfg(target_os = "macos")]
fn lookup<T>(handle: *mut std::ffi::c_void, name: &std::ffi::CStr) -> Option<T> {
    unsafe {
        let symbol = dlsym(handle, name.as_ptr());
        if symbol.is_null() {
            None
        } else {
            Some(std::mem::transmute_copy(&symbol))
        }
    }
}

#[cfg(target_os = "macos")]
struct CfObject(*const std::ffi::c_void);

#[cfg(target_os = "macos")]
impl Drop for CfObject {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

#[cfg(target_os = "macos")]
impl CfObject {
    fn new(ptr: *const std::ffi::c_void) -> Option<Self> {
        if ptr.is_null() {
            None
        } else {
            Some(Self(ptr))
        }
    }

    fn as_ptr(&self) -> *const std::ffi::c_void {
        self.0
    }
}

#[cfg(target_os = "macos")]
fn cf_string(name: &std::ffi::CStr) -> Option<CfObject> {
    CfObject::new(unsafe {
        CFStringCreateWithCString(std::ptr::null(), name.as_ptr(), CF_STRING_UTF8)
    })
}

#[cfg(target_os = "macos")]
fn cf_string_to_string(value: *const std::ffi::c_void) -> Option<String> {
    if value.is_null() {
        return None;
    }
    unsafe {
        if CFGetTypeID(value) != CFStringGetTypeID() {
            return None;
        }
        let mut buffer = [0i8; 256];
        let copied = CFStringGetCString(
            value,
            buffer.as_mut_ptr(),
            buffer.len() as isize,
            CF_STRING_UTF8,
        );
        if copied == 0 {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr(buffer.as_ptr())
                .to_string_lossy()
                .into_owned(),
        )
    }
}

#[cfg(target_os = "macos")]
fn temperature_matching() -> Option<CfObject> {
    unsafe {
        let page_key = cf_string(c"PrimaryUsagePage")?;
        let usage_key = cf_string(c"PrimaryUsage")?;
        let page: i32 = 0xFF00;
        let usage: i32 = 5;
        let page_num = CfObject::new(CFNumberCreate(
            std::ptr::null(),
            CF_NUMBER_SINT32,
            &page as *const i32 as *const std::ffi::c_void,
        ))?;
        let usage_num = CfObject::new(CFNumberCreate(
            std::ptr::null(),
            CF_NUMBER_SINT32,
            &usage as *const i32 as *const std::ffi::c_void,
        ))?;
        let keys = [page_key.as_ptr(), usage_key.as_ptr()];
        let values = [page_num.as_ptr(), usage_num.as_ptr()];
        CfObject::new(CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            2,
            &kCFTypeDictionaryKeyCallBacks as *const u8 as *const std::ffi::c_void,
            &kCFTypeDictionaryValueCallBacks as *const u8 as *const std::ffi::c_void,
        ))
    }
}

#[cfg(target_os = "macos")]
fn smc_temperature() -> Option<f64> {
    unsafe {
        let matching = IOServiceMatching(c"AppleSMC".as_ptr());
        if matching.is_null() {
            return None;
        }
        let service = IOServiceGetMatchingService(0, matching);
        if service == 0 {
            return None;
        }
        let mut connection = 0u32;
        let status = IOServiceOpen(service, mach_task_self_, 0, &mut connection);
        if status != 0 {
            IOObjectRelease(service);
            return None;
        }
        let mut best = None;
        for key in [*b"TC0P", *b"TC0D", *b"TC0E", *b"TC0F", *b"TC0H"] {
            consider(&mut best, read_smc_celsius(connection, &key));
        }
        IOServiceClose(connection);
        IOObjectRelease(service);
        best
    }
}

#[cfg(target_os = "macos")]
fn read_smc_celsius(connection: u32, key: &[u8; 4]) -> Option<f64> {
    let mut input = SmcKeyData::zero();
    let mut output = SmcKeyData::zero();
    input.key = u32::from_be_bytes(*key);
    input.data8 = 9;
    if !smc_call(connection, &input, &mut output) {
        return None;
    }
    let size = output.data_size;
    let data_type = output.data_type;
    if size == 0 || size > 32 {
        return None;
    }
    input.data_size = size;
    input.data8 = 5;
    output = SmcKeyData::zero();
    if !smc_call(connection, &input, &mut output) {
        return None;
    }
    decode_smc_temperature(data_type, &output.bytes[..size as usize])
}

#[cfg(target_os = "macos")]
fn smc_call(connection: u32, input: &SmcKeyData, output: &mut SmcKeyData) -> bool {
    let mut out_size = std::mem::size_of::<SmcKeyData>();
    let status = unsafe {
        IOConnectCallStructMethod(
            connection,
            2,
            std::ptr::from_ref(input).cast(),
            std::mem::size_of::<SmcKeyData>(),
            std::ptr::from_mut(output).cast(),
            &mut out_size,
        )
    };
    status == 0
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct SmcKeyData {
    key: u32,
    version: [u8; 6],
    pad0: [u8; 2],
    p_limit: [u8; 16],
    data_size: u32,
    data_type: u32,
    data_attributes: u8,
    pad1: [u8; 3],
    result: u8,
    status: u8,
    data8: u8,
    pad2: u8,
    data32: u32,
    bytes: [u8; 32],
}

#[cfg(target_os = "macos")]
const _: () = assert!(std::mem::size_of::<SmcKeyData>() == 80);

#[cfg(target_os = "macos")]
impl SmcKeyData {
    fn zero() -> Self {
        Self {
            key: 0,
            version: [0; 6],
            pad0: [0; 2],
            p_limit: [0; 16],
            data_size: 0,
            data_type: 0,
            data_attributes: 0,
            pad1: [0; 3],
            result: 0,
            status: 0,
            data8: 0,
            pad2: 0,
            data32: 0,
            bytes: [0; 32],
        }
    }
}

#[cfg(any(target_os = "macos", test))]
fn decode_smc_temperature(data_type: u32, bytes: &[u8]) -> Option<f64> {
    let sp78 = u32::from_be_bytes(*b"sp78");
    let sp87 = u32::from_be_bytes(*b"sp87");
    let float_type = u32::from_be_bytes(*b"flt ");
    let celsius = if data_type == sp78 && bytes.len() >= 2 {
        i16::from_be_bytes([bytes[0], bytes[1]]) as f64 / 256.0
    } else if data_type == sp87 && bytes.len() >= 2 {
        i16::from_be_bytes([bytes[0], bytes[1]]) as f64 / 128.0
    } else if data_type == float_type && bytes.len() >= 4 {
        f32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f64
    } else {
        return None;
    };
    plausible_celsius(celsius)
}

#[cfg(any(target_os = "linux", target_os = "windows", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CoreTicks {
    idle: u64,
    total: u64,
}

#[cfg(any(target_os = "linux", test))]
fn parse_proc_stat(text: &str) -> Option<Vec<CoreTicks>> {
    let mut cores = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(label) = parts.next() else {
            continue;
        };
        if !label
            .strip_prefix("cpu")
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        let mut numbers = [0u64; 8];
        let mut count = 0usize;
        for (index, part) in parts.take(8).enumerate() {
            let Ok(value) = part.parse::<u64>() else {
                count = 0;
                break;
            };
            numbers[index] = value;
            count = index + 1;
        }
        if count < 4 {
            continue;
        }
        let idle = numbers[3].wrapping_add(if count > 4 { numbers[4] } else { 0 });
        let total = numbers[0]
            .wrapping_add(numbers[1])
            .wrapping_add(numbers[2])
            .wrapping_add(idle)
            .wrapping_add(if count > 5 { numbers[5] } else { 0 })
            .wrapping_add(if count > 6 { numbers[6] } else { 0 })
            .wrapping_add(if count > 7 { numbers[7] } else { 0 });
        cores.push(CoreTicks { idle, total });
    }
    if cores.is_empty() {
        None
    } else {
        Some(cores)
    }
}

#[cfg(target_os = "linux")]
fn read_ticks() -> Option<TickSample> {
    let text = std::fs::read_to_string("/proc/stat").ok()?;
    parse_proc_stat(&text)
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn loads_between(previous: &[CoreTicks], current: &[CoreTicks]) -> Option<Vec<f64>> {
    if previous.len() != current.len() || current.is_empty() {
        return None;
    }
    Some(
        previous
            .iter()
            .zip(current)
            .map(|(before, after)| {
                utilization(
                    after.idle.wrapping_sub(before.idle),
                    after.total.wrapping_sub(before.total),
                )
            })
            .collect(),
    )
}

#[cfg(any(target_os = "windows", test))]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SystemProcessorPerformanceInformation {
    idle_time: i64,
    kernel_time: i64,
    user_time: i64,
    _dpc_time: i64,
    _interrupt_time: i64,
    _interrupt_count: u32,
}

#[cfg(any(target_os = "windows", test))]
const _: () = assert!(std::mem::size_of::<SystemProcessorPerformanceInformation>() == 48);

#[cfg(any(target_os = "windows", test))]
fn windows_ticks_from_perf_info(
    entries: &[SystemProcessorPerformanceInformation],
) -> Option<Vec<CoreTicks>> {
    if entries.is_empty() {
        return None;
    }
    Some(
        entries
            .iter()
            .map(|entry| {
                let idle = entry.idle_time.max(0) as u64;
                let kernel = entry.kernel_time.max(0) as u64;
                let user = entry.user_time.max(0) as u64;
                // SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION.KernelTime includes IdleTime.
                let kernel_busy = kernel.saturating_sub(idle);
                let total = idle.wrapping_add(kernel_busy).wrapping_add(user);
                CoreTicks { idle, total }
            })
            .collect(),
    )
}

#[cfg(target_os = "windows")]
fn read_ticks() -> Option<TickSample> {
    const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: u32 = 8;
    const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004u32 as i32;

    let elem_size = std::mem::size_of::<SystemProcessorPerformanceInformation>();
    let initial = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(64)
        .clamp(1, 4096);
    let mut buffer = vec![SystemProcessorPerformanceInformation::default(); initial];
    let mut return_length: u32 = 0;
    let mut status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION,
            buffer.as_mut_ptr().cast(),
            (buffer.len() * elem_size) as u32,
            &mut return_length,
        )
    };
    if status == STATUS_INFO_LENGTH_MISMATCH {
        let needed = (return_length as usize) / elem_size;
        if needed == 0 || needed > 4096 {
            return None;
        }
        buffer.resize(needed, SystemProcessorPerformanceInformation::default());
        return_length = 0;
        status = unsafe {
            NtQuerySystemInformation(
                SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * elem_size) as u32,
                &mut return_length,
            )
        };
    }
    if status != 0 {
        return None;
    }
    let count = (return_length as usize) / elem_size;
    if count == 0 || count > buffer.len() {
        return None;
    }
    windows_ticks_from_perf_info(&buffer[..count])
}

#[cfg(target_os = "windows")]
#[link(name = "ntdll")]
extern "system" {
    fn NtQuerySystemInformation(
        system_information_class: u32,
        system_information: *mut std::ffi::c_void,
        system_information_length: u32,
        return_length: *mut u32,
    ) -> i32;
}

#[cfg(target_os = "linux")]
fn read_trimmed(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
}

#[cfg(any(target_os = "linux", test))]
fn celsius_from_millidegrees(text: &str) -> Option<f64> {
    let milli: f64 = text.trim().parse().ok()?;
    plausible_celsius(milli / 1000.0)
}

#[cfg(any(target_os = "linux", test))]
fn temp_input_index(name: &str) -> Option<u32> {
    let index = name.strip_prefix("temp")?.strip_suffix("_input")?;
    if index.is_empty() {
        None
    } else {
        index.parse().ok()
    }
}

#[cfg(target_os = "linux")]
fn thermal_zone_temperature() -> Option<f64> {
    let entries = std::fs::read_dir("/sys/class/thermal").ok()?;
    let mut best = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("thermal_zone") {
            continue;
        }
        let path = entry.path();
        let Some(kind) = read_trimmed(&path.join("type")) else {
            continue;
        };
        if !is_cpu_temperature_name(&kind) {
            continue;
        }
        let Some(raw) = read_trimmed(&path.join("temp")) else {
            continue;
        };
        consider(&mut best, celsius_from_millidegrees(&raw));
    }
    best
}

#[cfg(target_os = "linux")]
fn hwmon_temperature() -> Option<f64> {
    let entries = std::fs::read_dir("/sys/class/hwmon").ok()?;
    let mut best = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let device = read_trimmed(&path.join("name")).unwrap_or_default();
        let device_is_cpu = is_cpu_temperature_name(&device);
        let Ok(files) = std::fs::read_dir(&path) else {
            continue;
        };
        for file in files.flatten() {
            let file_name = file.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if temp_input_index(file_name).is_none() {
                continue;
            }
            let label = file_name
                .strip_suffix("_input")
                .map(|prefix| {
                    read_trimmed(&path.join(format!("{prefix}_label"))).unwrap_or_default()
                })
                .unwrap_or_default();
            if !device_is_cpu && !is_cpu_temperature_name(&label) {
                continue;
            }
            let Some(raw) = read_trimmed(&file.path()) else {
                continue;
            };
            consider(&mut best, celsius_from_millidegrees(&raw));
        }
    }
    best
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: *const std::ffi::c_void);
    fn CFStringCreateWithCString(
        allocator: *const std::ffi::c_void,
        c_str: *const i8,
        encoding: u32,
    ) -> *const std::ffi::c_void;
    fn CFStringGetCString(
        value: *const std::ffi::c_void,
        buffer: *mut i8,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;
    fn CFStringGetTypeID() -> usize;
    fn CFGetTypeID(value: *const std::ffi::c_void) -> usize;
    fn CFArrayGetCount(array: *const std::ffi::c_void) -> isize;
    fn CFArrayGetValueAtIndex(
        array: *const std::ffi::c_void,
        index: isize,
    ) -> *const std::ffi::c_void;
    fn CFDictionaryCreate(
        allocator: *const std::ffi::c_void,
        keys: *const *const std::ffi::c_void,
        values: *const *const std::ffi::c_void,
        num_values: isize,
        key_callbacks: *const std::ffi::c_void,
        value_callbacks: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void;
    fn CFNumberCreate(
        allocator: *const std::ffi::c_void,
        number_type: isize,
        value: *const std::ffi::c_void,
    ) -> *const std::ffi::c_void;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
}

#[cfg(target_os = "macos")]
#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IOServiceMatching(name: *const i8) -> *const std::ffi::c_void;
    fn IOServiceGetMatchingService(main_port: u32, matching: *const std::ffi::c_void) -> u32;
    fn IOServiceOpen(service: u32, owning_task: u32, type_: u32, connect: *mut u32) -> i32;
    fn IOServiceClose(connect: u32) -> i32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IOConnectCallStructMethod(
        connection: u32,
        selector: u32,
        input: *const std::ffi::c_void,
        input_size: usize,
        output: *mut std::ffi::c_void,
        output_size: *mut usize,
    ) -> i32;
}

#[cfg(target_os = "macos")]
extern "C" {
    fn mach_host_self() -> u32;
    fn host_processor_info(
        host: u32,
        flavor: i32,
        out_processor_count: *mut u32,
        out_processor_info: *mut *mut i32,
        out_processor_info_count: *mut u32,
    ) -> i32;
    static mach_task_self_: u32;
    fn mach_port_deallocate(task: u32, name: u32) -> i32;
    fn vm_deallocate(target: u32, address: usize, size: usize) -> i32;
    fn dlopen(path: *const i8, mode: i32) -> *mut std::ffi::c_void;
    fn dlsym(handle: *mut std::ffi::c_void, symbol: *const i8) -> *mut std::ffi::c_void;
    fn dlclose(handle: *mut std::ffi::c_void) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_uses_null_when_unavailable() {
        let stats = MachineStats {
            load: None,
            temperature_c: None,
        };
        let json = serde_json::to_value(&stats).unwrap();
        assert!(json.get("load").unwrap().is_null());
        assert!(json.get("temperatureC").unwrap().is_null());
        assert!(json.get("temperature_c").is_none());

        let stats = MachineStats {
            load: Some(vec![0.0, 12.5]),
            temperature_c: Some(42.0),
        };
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["load"][0], 0.0);
        assert_eq!(json["load"][1], 12.5);
        assert_eq!(json["temperatureC"], 42.0);
    }

    #[test]
    fn machine_stats_grant_defaults_false() {
        use crate::models::{AppData, PreferencesPatch};

        let parsed: AppData = serde_json::from_str("{}").unwrap();
        assert!(!parsed.machine_stats);
        assert!(!AppData::default().machine_stats);
        let stored = serde_json::to_value(AppData::default()).unwrap();
        assert_eq!(stored["machineStats"], false);

        let enabled: PreferencesPatch = serde_json::from_str(r#"{"machineStats":true}"#).unwrap();
        assert_eq!(enabled.machine_stats, Some(true));
        let other: PreferencesPatch = serde_json::from_str(r#"{"pageSize":300}"#).unwrap();
        assert_eq!(other.page_size, Some(300));
        assert_eq!(other.machine_stats, None);
    }

    #[test]
    fn utilization_is_busy_share_of_ticks() {
        assert_eq!(utilization(25, 100), 75.0);
        assert_eq!(utilization(0, 0), 0.0);
        assert_eq!(utilization(100, 100), 0.0);
        assert_eq!(utilization(0, 3), 100.0);
        assert_eq!(utilization(2, 3), 33.3);
    }

    #[test]
    fn smc_decoder_accepts_celsius_types_only() {
        assert_eq!(
            decode_smc_temperature(u32::from_be_bytes(*b"sp78"), &[0x2d, 0x00]),
            Some(45.0)
        );
        assert_eq!(
            decode_smc_temperature(u32::from_be_bytes(*b"sp87"), &[0x16, 0x80]),
            Some(45.0)
        );
        let float_bytes = 45.5f32.to_be_bytes();
        assert_eq!(
            decode_smc_temperature(u32::from_be_bytes(*b"flt "), &float_bytes),
            Some(45.5)
        );
        assert_eq!(
            decode_smc_temperature(u32::from_be_bytes(*b"fpe2"), &[0x2d, 0x00]),
            None
        );
        assert_eq!(
            decode_smc_temperature(u32::from_be_bytes(*b"ui16"), &[0x00, 50]),
            None
        );
        assert_eq!(
            decode_smc_temperature(u32::from_be_bytes(*b"sp78"), &[0x00, 0x00]),
            None
        );
    }

    #[test]
    fn proc_stat_is_per_logical_core_and_iowait_is_idle() {
        let sample = parse_proc_stat(
            "cpu  20 0 10 70 0 0 0 0 0 0\ncpu0 10 0 10 70 10 0 0 0\ncpu1 5 5 0 90 0 0 0 0\nintr 1\n",
        )
        .unwrap();
        assert_eq!(sample.len(), 2);
        assert_eq!(sample[0].idle, 80);
        assert_eq!(sample[0].total, 100);
        assert_eq!(sample[1].idle, 90);
        assert_eq!(sample[1].total, 100);
        assert!(parse_proc_stat("intr 1\n").is_none());
    }

    #[test]
    fn millidegree_sysfs_values_stay_celsius() {
        assert_eq!(celsius_from_millidegrees("45123\n"), Some(45.1));
        assert_eq!(celsius_from_millidegrees("45"), None);
        assert_eq!(celsius_from_millidegrees("200000"), None);
        assert_eq!(celsius_from_millidegrees("-1000"), None);
        assert_eq!(temp_input_index("temp1_input"), Some(1));
        assert_eq!(temp_input_index("temp10_input"), Some(10));
        assert_eq!(temp_input_index("temp1_label"), None);
        assert_eq!(temp_input_index("temp_input"), None);
    }

    #[test]
    fn windows_perf_info_subtracts_idle_from_kernel_time() {
        let entries = [
            SystemProcessorPerformanceInformation {
                idle_time: 700,
                kernel_time: 800,
                user_time: 200,
                ..Default::default()
            },
            SystemProcessorPerformanceInformation {
                idle_time: 900,
                kernel_time: 900,
                user_time: 100,
                ..Default::default()
            },
        ];
        let ticks = windows_ticks_from_perf_info(&entries).unwrap();
        assert_eq!(
            ticks,
            vec![
                CoreTicks {
                    idle: 700,
                    total: 1000
                },
                CoreTicks {
                    idle: 900,
                    total: 1000
                },
            ]
        );
        assert!(windows_ticks_from_perf_info(&[]).is_none());
    }

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    #[test]
    fn second_sample_is_per_core_load() {
        let first = per_core_load();
        let second = per_core_load();
        assert!(first.is_none(), "first sample has no delta yet");
        let loads = second.expect("second sample");
        assert!(!loads.is_empty());
        assert!(loads.iter().all(|load| (0.0..=100.0).contains(load)));
    }

    #[test]
    fn temperature_reading_is_celsius_or_absent() {
        if let Some(celsius) = temperature_c() {
            assert!(
                (0.0..=125.0).contains(&celsius),
                "temperature {celsius} is outside Celsius"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn mac_tick_delta_skips_idle() {
        let previous = vec![[100, 50, 800, 50]];
        let current = vec![[150, 70, 850, 50]];
        assert_eq!(loads_between(&previous, &current).unwrap(), vec![58.3]);
        assert!(loads_between(&previous, &[]).is_none());
    }
}
