//! Machine telemetry sampler: CPU, RAM, temperature, GPU, network. Feeds the
//! SYSTEM rail every couple of seconds via `Event::Sys`.

use std::collections::HashMap;
use std::time::Duration;

use butai_protocol::api::{DiskKind, NetKind};
use tokio::sync::mpsc::UnboundedSender;

use crate::core::Event;
use crate::workbench::{Container, DiskStat, GpuStat, NetStat, SysStats};

const SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
/// Samples kept per series — about two and a half minutes at
/// [`SAMPLE_INTERVAL`].
///
/// Eight was enough while the rail drew a four-cell sparkline. It now draws a
/// braille trace across the full width of the rail at two samples per cell, and
/// BOOTH's compute column is wider still: 36 cells at its widest, so 72
/// samples. Short histories pad on the left, so this only has to cover the
/// widest trace anyone will draw rather than be exact.
const HIST: usize = 80;
/// Sampler ticks between two readings of the mount table.
///
/// Capacity is the one quantity here that does not move at [`SAMPLE_INTERVAL`]:
/// a disk that fills over an afternoon is unchanged between two ticks two
/// seconds apart. Sampling it every fifth tick is the same reading ten seconds
/// later, and it is the difference between one `statvfs` per mount every two
/// seconds and one every ten — which matters on a docker host, where the mount
/// table runs to dozens of entries. The published list is carried between
/// readings rather than emptied, so every push still has it.
const DISK_EVERY: u32 = 5;
/// How long the whole mount sweep gets to report. Generous, because it is only
/// ever spent when something is genuinely stuck: thirty healthy local mounts
/// answer in well under a millisecond.
const DISK_SWEEP_TIMEOUT: Duration = Duration::from_millis(750);
/// How long a mount that missed [`DISK_SWEEP_TIMEOUT`] is left alone before it
/// is asked again. See [`statvfs_sweep`] for why this is a minute and not a
/// tick.
const DISK_COOLDOWN: Duration = Duration::from_secs(60);
/// Mounts published before the list is cut. `read_docker`'s `.take(64)` for the
/// same reason: a machine with an unusual number of them must not turn one
/// field into the whole payload.
const DISK_MAX: usize = 64;
/// Cadence of the marquee/animation clock.
const TICK_INTERVAL: Duration = Duration::from_millis(450);
/// Cadence of the sprite clock. Marquees read fine at 450ms, but a walk cycle
/// at that rate is a slideshow, so the ALL AGENTS panel gets its own faster
/// phase rather than speeding every scrolling row up with it.
const FAST_TICK_INTERVAL: Duration = Duration::from_millis(150);

/// Drives rail marquees: emits a monotonically increasing phase so the core
/// can redraw scrolling text. The core only repaints on a tick when something
/// actually needs animating, so an idle workbench stays quiet.
pub fn spawn_ticker(tx: UnboundedSender<Event>) {
    tokio::spawn(async move {
        let mut phase: u64 = 0;
        loop {
            tokio::time::sleep(TICK_INTERVAL).await;
            phase = phase.wrapping_add(1);
            if tx.send(Event::Tick(phase)).is_err() {
                return; // core is gone
            }
        }
    });
}

/// Drives the ALL AGENTS panel's sprites. Same contract as [`spawn_ticker`],
/// three times as often — and the core repaints on it only while the panel is
/// open *and* an agent is actually working, so a workbench without the panel
/// pays one sleeping task and no frames.
pub fn spawn_fast_ticker(tx: UnboundedSender<Event>) {
    tokio::spawn(async move {
        let mut phase: u64 = 0;
        loop {
            tokio::time::sleep(FAST_TICK_INTERVAL).await;
            phase = phase.wrapping_add(1);
            if tx.send(Event::FastTick(phase)).is_err() {
                return; // core is gone
            }
        }
    });
}

pub fn spawn_sampler(tx: UnboundedSender<Event>) {
    tokio::spawn(async move {
        let mut prev_cpu: Option<(u64, u64)> = None;
        let mut prev_net: HashMap<String, (u64, u64)> = HashMap::new();
        let mut prev_net_at = std::time::Instant::now();
        // Mounts that failed to answer, and the instant each may be asked
        // again. Lives across ticks because that is the whole point of it.
        let mut disk_hung: HashMap<String, std::time::Instant> = HashMap::new();
        let mut disk_tick: u32 = 0;
        let mut stats = SysStats::default();
        // Static for the life of the daemon — the CPU does not change model
        // between ticks, so this is read once and carried on every sample.
        (stats.cpu_model, stats.cpu_cores, stats.cpu_threads) = read_cpu_id();
        loop {
            if let Some((busy, total)) = read_cpu() {
                if let Some((pb, pt)) = prev_cpu {
                    let db = busy.saturating_sub(pb) as f32;
                    let dt = total.saturating_sub(pt) as f32;
                    if dt > 0.0 {
                        stats.cpu_pct = (db / dt * 100.0).clamp(0.0, 100.0);
                    }
                }
                prev_cpu = Some((busy, total));
            }
            push(&mut stats.cpu_hist, stats.cpu_pct);
            stats.cpu_temp = read_cpu_temp();

            if let Some((used, total)) = read_ram_gb() {
                stats.ram_used_gb = used;
                stats.ram_total_gb = total;
                push(&mut stats.ram_hist, if total > 0.0 { used / total * 100.0 } else { 0.0 });
            }
            (stats.swap_used_gb, stats.swap_total_gb) = read_swap_gb();

            let readings = read_gpu().await;
            // Preserve each GPU's history across samples; add/drop rows as the
            // detected GPU set changes.
            stats.gpus.resize_with(readings.len(), GpuStat::default);
            for (g, r) in stats.gpus.iter_mut().zip(readings) {
                g.pct = r.pct;
                g.mem_used_gb = r.mem_used_gb;
                g.mem_total_gb = r.mem_total_gb;
                g.name = r.name;
                g.temp_c = r.temp_c;
                g.power_w = r.power_w;
                push(&mut g.hist, r.pct);
            }

            // Network: the kernel gives monotonic byte counters, so a rate needs
            // both the previous reading and how long ago it was taken. Elapsed
            // time is measured rather than assumed to be SAMPLE_INTERVAL —
            // reading GPUs and docker shells out, and a slow `docker ps` would
            // otherwise inflate every rate on the row.
            let readings = read_net().await;
            let now = std::time::Instant::now();
            let dt = now.duration_since(prev_net_at).as_secs_f32().max(0.001);
            prev_net_at = now;
            let mut kept: HashMap<String, NetStat> =
                stats.net.drain(..).map(|n| (n.name.clone(), n)).collect();
            stats.net = readings
                .into_iter()
                .map(|r| {
                    // History follows the name, so an interface that comes and
                    // goes — a VPN, a container's veth — keeps its trace.
                    let mut n = kept.remove(&r.name).unwrap_or_default();
                    // A missing previous sample reads as zero rather than as the
                    // counter's whole value since boot, which would draw the
                    // first tick after attach as a spike off the top. The same
                    // saturating subtraction absorbs a counter reset.
                    let (rx_bps, tx_bps) = match prev_net.get(&r.name) {
                        Some(&(prx, ptx)) => (
                            r.rx.saturating_sub(prx) as f32 / dt,
                            r.tx.saturating_sub(ptx) as f32 / dt,
                        ),
                        None => (0.0, 0.0),
                    };
                    prev_net.insert(r.name.clone(), (r.rx, r.tx));
                    n.name = r.name;
                    n.kind = r.kind;
                    n.carrier = r.carrier;
                    n.default_route = r.default_route;
                    n.speed_mbps = r.speed_mbps;
                    n.driver = r.driver;
                    n.rx_bps = rx_bps;
                    n.tx_bps = tx_bps;
                    if keeps_history(n.kind) {
                        push(&mut n.rx_hist, rx_bps);
                        push(&mut n.tx_hist, tx_bps);
                    } else {
                        // Reclassified mid-run — drop what was kept rather than
                        // publish a series that stopped being updated.
                        n.rx_hist.clear();
                        n.tx_hist.clear();
                    }
                    n
                })
                .collect();
            // Interfaces that went away take their counters with them, or a
            // reappearing name would diff against a stale reading.
            prev_net.retain(|name, _| stats.net.iter().any(|n| &n.name == name));

            // Every fifth tick, and the list persists in between — see
            // [`DISK_EVERY`]. Taken before docker rather than after so a slow
            // `docker ps` cannot delay it.
            //
            if disk_tick.is_multiple_of(DISK_EVERY) {
                stats.disks = read_disks(&stats.disks, &mut disk_hung).await;
            }
            disk_tick = disk_tick.wrapping_add(1);

            stats.containers = read_docker().await.unwrap_or_default();

            if tx.send(Event::Sys(stats.clone())).is_err() {
                return; // core is gone
            }
            tokio::time::sleep(SAMPLE_INTERVAL).await;
        }
    });
}

/// Whether an interface is worth keeping a trend series for.
///
/// Every interface's *rates* are published every tick regardless — nothing about
/// the machine is hidden, and a client that wants a trend for a bridge can
/// accumulate one from those. What this declines to do is pre-compute a series
/// that is a duplicate by construction: a veth's and a bridge's bytes are
/// counted again on whatever interface they egress from, and loopback traffic
/// never leaves the machine.
///
/// The reason is bandwidth, and it is not marginal. This is sampled every two
/// seconds and pushed to every attached client: a docker host with 36
/// interfaces spent 31 KB of a 39 KB payload on history for interfaces no
/// client draws, and it scales with the interface count — a Kubernetes node
/// with hundreds of veths would be far worse.
fn keeps_history(kind: NetKind) -> bool {
    !matches!(kind, NetKind::Loopback | NetKind::Bridge | NetKind::Veth)
}

fn push(hist: &mut Vec<f32>, v: f32) {
    hist.push(v);
    if hist.len() > HIST {
        hist.remove(0);
    }
}

/// (busy, total) jiffies from the aggregate cpu line.
#[cfg(target_os = "linux")]
fn read_cpu() -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let line = stat.lines().next()?;
    let fields: Vec<u64> = line.split_whitespace().skip(1).filter_map(|f| f.parse().ok()).collect();
    if fields.len() < 4 {
        return None;
    }
    let idle = fields[3] + fields.get(4).copied().unwrap_or(0); // idle + iowait
    let total: u64 = fields.iter().sum();
    Some((total.saturating_sub(idle), total))
}

#[cfg(target_os = "linux")]
fn read_ram_gb() -> Option<(f32, f32)> {
    let mem = std::fs::read_to_string("/proc/meminfo").ok()?;
    let get = |key: &str| -> Option<f32> {
        mem.lines()
            .find(|l| l.starts_with(key))?
            .split_whitespace()
            .nth(1)?
            .parse::<f32>()
            .ok()
            .map(|kb| kb / 1024.0 / 1024.0)
    };
    let total = get("MemTotal:")?;
    let avail = get("MemAvailable:")?;
    Some(((total - avail).max(0.0), total))
}

/// (busy, total) CPU ticks summed over all cores, via mach's per-processor
/// load counters — the same `(busy, total)` shape the sampler diffs, so no
/// caller changes. macOS has no `/proc`; this is the equivalent counter source.
///
/// `libc` deprecated its `mach_*` re-exports in favour of the `mach2` crate.
/// They are still the right symbols in libSystem and still work; taking a new
/// dependency to call the same two functions would buy nothing, so the
/// deprecation is allowed here rather than routed around.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn read_cpu() -> Option<(u64, u64)> {
    unsafe {
        let mut cpu_count: libc::natural_t = 0;
        let mut info: libc::processor_info_array_t = std::ptr::null_mut();
        let mut info_count: libc::mach_msg_type_number_t = 0;
        let kr = libc::host_processor_info(
            libc::mach_host_self(),
            libc::PROCESSOR_CPU_LOAD_INFO,
            &mut cpu_count,
            &mut info,
            &mut info_count,
        );
        if kr != libc::KERN_SUCCESS || info.is_null() {
            return None;
        }
        let states = libc::CPU_STATE_MAX as usize;
        let ticks = std::slice::from_raw_parts(info, cpu_count as usize * states);
        // ticks are unsigned counts carried in an i32 array; reinterpret the
        // bits (`as u32`) so a count past 2^31 doesn't read as negative.
        let at = |i: usize| ticks[i] as u32 as u64;
        let (mut busy, mut total) = (0u64, 0u64);
        for c in 0..cpu_count as usize {
            let base = c * states;
            let user = at(base + libc::CPU_STATE_USER as usize);
            let system = at(base + libc::CPU_STATE_SYSTEM as usize);
            let idle = at(base + libc::CPU_STATE_IDLE as usize);
            let nice = at(base + libc::CPU_STATE_NICE as usize);
            busy += user + system + nice;
            total += user + system + nice + idle;
        }
        // host_processor_info vm_allocates the array; hand it back.
        libc::vm_deallocate(
            libc::mach_task_self(),
            info as libc::vm_address_t,
            (info_count as usize * std::mem::size_of::<libc::integer_t>()) as libc::vm_size_t,
        );
        (total > 0).then_some((busy, total))
    }
}

/// (used, total) GiB. Total from `hw.memsize`; "used" is active + wired +
/// compressed pages (what Activity Monitor calls Memory Used) from mach's
/// `host_statistics64`. `mach_host_self` is deprecated in `libc` for the same
/// reason as in [`read_cpu`], and allowed here on the same grounds.
#[cfg(target_os = "macos")]
#[allow(deprecated)]
fn read_ram_gb() -> Option<(f32, f32)> {
    let page = {
        let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if p > 0 {
            p as u64
        } else {
            4096
        }
    };
    let total_bytes = sysctl_u64("hw.memsize")?;
    let vm = unsafe {
        let mut vm: libc::vm_statistics64 = std::mem::zeroed();
        let mut count = (std::mem::size_of::<libc::vm_statistics64>()
            / std::mem::size_of::<libc::integer_t>())
            as libc::mach_msg_type_number_t;
        let kr = libc::host_statistics64(
            libc::mach_host_self(),
            libc::HOST_VM_INFO64,
            &mut vm as *mut _ as libc::host_info64_t,
            &mut count,
        );
        if kr != libc::KERN_SUCCESS {
            return None;
        }
        vm
    };
    let used_pages =
        vm.active_count as u64 + vm.wire_count as u64 + vm.compressor_page_count as u64;
    let gib = 1024.0 * 1024.0 * 1024.0;
    let total = total_bytes as f32 / gib;
    let used = (used_pages * page) as f32 / gib;
    Some((used.min(total), total))
}

/// Read a `u64` sysctl by name (e.g. `hw.memsize`). Also used by the terminal
/// pane for `kern.argmax`; a narrower value simply leaves the high bytes zero.
#[cfg(target_os = "macos")]
pub(crate) fn sysctl_u64(name: &str) -> Option<u64> {
    let cname = std::ffi::CString::new(name).ok()?;
    let mut val: u64 = 0;
    let mut len = std::mem::size_of::<u64>();
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            &mut val as *mut u64 as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0).then_some(val)
}

/// Read a string sysctl by name (e.g. `machdep.cpu.brand_string`). Queried once
/// for its length, then again for the bytes, which is the documented two-call
/// form — the value is NUL-terminated and the trailing NUL is dropped here.
#[cfg(target_os = "macos")]
fn sysctl_string(name: &str) -> Option<String> {
    let cname = std::ffi::CString::new(name).ok()?;
    let mut len: usize = 0;
    let rc = unsafe {
        libc::sysctlbyname(cname.as_ptr(), std::ptr::null_mut(), &mut len, std::ptr::null_mut(), 0)
    };
    if rc != 0 || len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(len);
    while buf.last() == Some(&0) {
        buf.pop();
    }
    String::from_utf8(buf).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Platforms without a CPU/RAM sampler: the SYSTEM rail simply reads zero.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_cpu() -> Option<(u64, u64)> {
    None
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_ram_gb() -> Option<(f32, f32)> {
    None
}

/// One interface as the kernel currently reports it: cumulative byte counters
/// since boot, plus the facts a client needs to decide whether to draw it.
struct NetReading {
    name: String,
    rx: u64,
    tx: u64,
    kind: NetKind,
    carrier: bool,
    default_route: bool,
    speed_mbps: Option<u32>,
    driver: Option<String>,
}

/// Every interface, from `/proc/net/dev`, with its kind and carrier read out of
/// sysfs and the default route out of `/proc/net/route`.
///
/// Unfiltered: `lo` and the docker bridges are reported like anything else,
/// because which of them counts as "the network" is the client's question. See
/// [`butai_protocol::NetKind`].
#[cfg(target_os = "linux")]
async fn read_net() -> Vec<NetReading> {
    let Ok(dev) = std::fs::read_to_string("/proc/net/dev") else { return Vec::new() };
    let default = default_iface();
    // Two header lines, then `name: rx_bytes rx_packets ... tx_bytes ...`.
    dev.lines()
        .skip(2)
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let name = name.trim();
            let f: Vec<u64> = rest.split_whitespace().filter_map(|v| v.parse().ok()).collect();
            // Eight receive columns come first; transmit bytes is the ninth.
            let (&rx, &tx) = (f.first()?, f.get(8)?);
            let (speed_mbps, driver) = read_link(name);
            Some(NetReading {
                kind: net_kind(name),
                carrier: read_carrier(name),
                default_route: default.as_deref() == Some(name),
                name: name.to_string(),
                rx,
                tx,
                speed_mbps,
                driver,
            })
        })
        .collect()
}

/// The interface carrying the default route, i.e. destination `0.0.0.0`. The
/// destination column is little-endian hex, so the default route is the one
/// that is all zeroes either way round.
#[cfg(target_os = "linux")]
fn default_iface() -> Option<String> {
    let route = std::fs::read_to_string("/proc/net/route").ok()?;
    route.lines().skip(1).find_map(|line| {
        let mut f = line.split_whitespace();
        let iface = f.next()?;
        let dest = f.next()?;
        (dest == "00000000").then(|| iface.to_string())
    })
}

#[cfg(target_os = "linux")]
fn net_kind(name: &str) -> NetKind {
    let base = std::path::Path::new("/sys/class/net").join(name);
    // Order matters: a bridge has a `device` link too, and a wireless
    // interface would otherwise be reported as plain wired.
    if name == "lo" {
        NetKind::Loopback
    } else if base.join("bridge").is_dir() {
        NetKind::Bridge
    } else if base.join("wireless").exists() || base.join("phy80211").exists() {
        NetKind::Wireless
    } else if name.starts_with("veth") {
        NetKind::Veth
    } else if ["tun", "tap", "wg", "ppp"].iter().any(|p| name.starts_with(p)) {
        NetKind::Vpn
    } else if base.join("device").exists() {
        NetKind::Wired
    } else {
        NetKind::Other
    }
}

/// Reading `carrier` on a down interface fails with `EINVAL` rather than
/// returning 0, so an unreadable value means "no carrier".
#[cfg(target_os = "linux")]
fn read_carrier(name: &str) -> bool {
    std::fs::read_to_string(format!("/sys/class/net/{name}/carrier")).is_ok_and(|s| s.trim() == "1")
}

/// An interface's negotiated speed and bound driver, both from sysfs.
///
/// `speed` is absent on wireless and on the virtual interfaces, and reads back
/// as `-1` on a link that is down — both of which become `None` rather than a
/// number nobody should draw. The veth pairs report a nominal 10000, which is
/// truthful and also why the client does not draw them.
#[cfg(target_os = "linux")]
fn read_link(name: &str) -> (Option<u32>, Option<String>) {
    let base = std::path::Path::new("/sys/class/net").join(name);
    let speed = std::fs::read_to_string(base.join("speed"))
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|&s| s > 0)
        .map(|s| s as u32);
    let driver = std::fs::read_to_string(base.join("device/uevent")).ok().and_then(|u| {
        u.lines().find_map(|l| l.strip_prefix("DRIVER=").map(|d| d.trim().to_string()))
    });
    (speed, driver.filter(|d| !d.is_empty()))
}

/// Every interface via `getifaddrs`, whose `AF_LINK` entries carry an
/// `if_data` with the same cumulative counters `/proc/net/dev` reports.
#[cfg(target_os = "macos")]
async fn read_net() -> Vec<NetReading> {
    let default = default_iface().await;
    let mut out: Vec<NetReading> = Vec::new();
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return out;
        }
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            cur = ifa.ifa_next;
            if ifa.ifa_addr.is_null() || ifa.ifa_data.is_null() {
                continue;
            }
            // Only the link-layer entry carries counters; the AF_INET ones are
            // the same interface again with an address instead.
            if (*ifa.ifa_addr).sa_family as i32 != libc::AF_LINK {
                continue;
            }
            let data = &*(ifa.ifa_data as *const libc::if_data);
            let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
            let kind = net_kind(&name);
            out.push(NetReading {
                carrier: ifa.ifa_flags & libc::IFF_RUNNING as u32 != 0,
                default_route: default.as_deref() == Some(name.as_str()),
                kind,
                // These are 32-bit here, unlike Linux's 64-bit counters, so
                // they wrap every 4 GiB — under a minute on a fast link. The
                // sampler's saturating subtraction turns the wrapped sample
                // into a zero, which loses one tick of a busy transfer rather
                // than drawing a 4 GiB spike.
                rx: u64::from(data.ifi_ibytes),
                tx: u64::from(data.ifi_obytes),
                // `if_baudrate` is the interface's nominal line rate in bits per
                // second; Mb/s is the unit the Linux path reports and the one
                // the rail prints. Zero means the driver published nothing.
                speed_mbps: (data.ifi_baudrate > 0)
                    .then_some((data.ifi_baudrate / 1_000_000) as u32)
                    .filter(|&s| s > 0),
                // There is no sysfs to read a driver name out of, and the BSD
                // equivalent needs an ioctl per interface for a string the rail
                // shows only when it has spare cells.
                driver: None,
                name,
            });
        }
        libc::freeifaddrs(ifap);
    }
    out
}

/// macOS has no routing table in the filesystem, so ask `route`. Shelling out
/// on the sample tick is what [`read_gpu_nvidia`] and [`read_docker`] already
/// do, and this one is cheaper than either.
#[cfg(target_os = "macos")]
async fn default_iface() -> Option<String> {
    let out =
        tokio::process::Command::new("route").args(["-n", "get", "default"]).output().await.ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.trim().strip_prefix("interface:").map(|v| v.trim().to_string()))
}

/// BSD names are structural: `en0` is ethernet or wifi, `bridge0` a bridge,
/// `utun0` a VPN. Telling ethernet from wifi needs a CoreWLAN query, which is
/// not worth a framework link — both are `Wired` here, and the client draws
/// them the same way.
#[cfg(target_os = "macos")]
fn net_kind(name: &str) -> NetKind {
    if name.starts_with("lo") {
        NetKind::Loopback
    } else if name.starts_with("bridge") {
        NetKind::Bridge
    } else if ["utun", "ipsec", "ppp", "gif", "stf"].iter().any(|p| name.starts_with(p)) {
        NetKind::Vpn
    } else if name.starts_with("vmenet") || name.starts_with("veth") {
        NetKind::Veth
    } else if name.starts_with("en") {
        NetKind::Wired
    } else {
        NetKind::Other
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
async fn read_net() -> Vec<NetReading> {
    Vec::new()
}

fn read_cpu_temp() -> Option<f32> {
    let hwmon = std::fs::read_dir("/sys/class/hwmon").ok()?;
    for entry in hwmon.flatten() {
        let dir = entry.path();
        let name = std::fs::read_to_string(dir.join("name")).unwrap_or_default();
        let name = name.trim();
        if matches!(name, "coretemp" | "k10temp" | "zenpower" | "cpu_thermal") {
            if let Ok(t) = std::fs::read_to_string(dir.join("temp1_input")) {
                if let Ok(milli) = t.trim().parse::<f32>() {
                    return Some(milli / 1000.0);
                }
            }
        }
    }
    None
}

/// One GPU's sampled telemetry.
#[derive(Default)]
struct GpuReading {
    pct: f32,
    mem_used_gb: f32,
    mem_total_gb: f32,
    name: String,
    temp_c: Option<f32>,
    power_w: Option<f32>,
}

/// Per-GPU telemetry — every NVIDIA GPU via nvidia-smi, else the primary AMD
/// card via sysfs; empty when none present.
async fn read_gpu() -> Vec<GpuReading> {
    let nv = read_gpu_nvidia().await;
    if !nv.is_empty() {
        return nv;
    }
    read_gpu_amd().into_iter().collect()
}

/// Shorten a vendor GPU name for the narrow rail, e.g.
/// "NVIDIA GeForce RTX 4090" -> "RTX 4090", "Radeon RX 7900 XTX" -> "RX 7900 XTX".
fn short_gpu_name(full: &str) -> String {
    let noise = ["NVIDIA", "GeForce", "AMD", "Advanced Micro Devices", "Corporation"];
    let mut s = full.to_string();
    for n in noise {
        s = s.replace(n, "");
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Shorten a vendor CPU name the way [`short_gpu_name`] does, e.g.
/// "AMD Ryzen 7 5700 Eight-Core Processor" -> "Ryzen 7 5700",
/// "Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz" -> "Core i7-9750H".
///
/// The rail has fourteen cells beside the CPU value at the default width, so
/// this is aggressive by design: the core count and the clock are already on
/// screen or not worth the room, and the marketing words never were.
fn short_cpu_name(full: &str) -> String {
    // Everything from the clock suffix on is noise: "CPU @ 2.60GHz".
    let head = full.split(" @ ").next().unwrap_or(full);
    let noise = [
        "(R)",
        "(TM)",
        "(tm)",
        "Intel",
        "AMD",
        "Genuine",
        "Authentic",
        "Processor",
        "CPU",
        "with Radeon Graphics",
    ];
    let mut s = head.to_string();
    for n in noise {
        s = s.replace(n, " ");
    }
    // "Eight-Core", "16-Core" and friends: the thread count is reported
    // separately and as a number, which is the form that fits.
    let words: Vec<&str> =
        s.split_whitespace().filter(|w| !w.to_ascii_lowercase().ends_with("-core")).collect();
    words.join(" ")
}

/// Model, physical cores and threads. Static for the life of the daemon, so the
/// sampler reads this once instead of reopening `/proc/cpuinfo` every tick.
#[cfg(target_os = "linux")]
fn read_cpu_id() -> (Option<String>, Option<u16>, Option<u16>) {
    let Ok(info) = std::fs::read_to_string("/proc/cpuinfo") else { return (None, None, None) };
    let field = |key: &str| -> Option<String> {
        info.lines()
            .find(|l| l.starts_with(key) && l.contains(':'))
            .and_then(|l| l.split_once(':'))
            .map(|(_, v)| v.trim().to_string())
    };
    let model = field("model name").map(|m| short_cpu_name(&m)).filter(|m| !m.is_empty());
    // One "processor" line per scheduler-visible thread. Physical cores are
    // "cpu cores" per socket, so multiply by the number of distinct physical
    // ids rather than trusting a single line.
    let threads = info.lines().filter(|l| l.starts_with("processor")).count();
    let sockets = info
        .lines()
        .filter(|l| l.starts_with("physical id"))
        .filter_map(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
        .collect::<std::collections::HashSet<_>>()
        .len()
        .max(1);
    let cores = field("cpu cores")
        .and_then(|c| c.parse::<usize>().ok())
        .map(|per_socket| per_socket * sockets)
        .filter(|&c| c > 0);
    (model, cores.map(|c| c as u16), (threads > 0).then_some(threads as u16))
}

/// `machdep.cpu.brand_string`, plus the two core counts as separate sysctls.
#[cfg(target_os = "macos")]
fn read_cpu_id() -> (Option<String>, Option<u16>, Option<u16>) {
    let model = sysctl_string("machdep.cpu.brand_string")
        .map(|m| short_cpu_name(&m))
        .filter(|m| !m.is_empty());
    let cores = sysctl_u64("hw.physicalcpu").filter(|&c| c > 0).map(|c| c as u16);
    let threads = sysctl_u64("hw.logicalcpu").filter(|&c| c > 0).map(|c| c as u16);
    (model, cores, threads)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_cpu_id() -> (Option<String>, Option<u16>, Option<u16>) {
    (None, None, None)
}

/// (used, total) swap in GB. A machine with no swap reports `(0.0, 0.0)`, which
/// is a fact rather than a missing reading — the rail draws nothing for it.
#[cfg(target_os = "linux")]
fn read_swap_gb() -> (f32, f32) {
    let Ok(mem) = std::fs::read_to_string("/proc/meminfo") else { return (0.0, 0.0) };
    let get = |key: &str| -> f32 {
        mem.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<f32>().ok())
            .map(|kb| kb / 1024.0 / 1024.0)
            .unwrap_or(0.0)
    };
    let total = get("SwapTotal:");
    let free = get("SwapFree:");
    ((total - free).max(0.0), total)
}

/// `vm.swapusage` is a formatted string ("total = 2048.00M used = 512.25M
/// free = ..."), not a number, so it is parsed rather than read as a u64.
#[cfg(target_os = "macos")]
fn read_swap_gb() -> (f32, f32) {
    let Some(s) = sysctl_string("vm.swapusage") else { return (0.0, 0.0) };
    // Values carry a unit suffix; only M is ever emitted in practice, but the
    // suffix is honoured rather than assumed.
    let field = |key: &str| -> f32 {
        s.split_whitespace()
            .skip_while(|w| !w.starts_with(key))
            .nth(2)
            .map(|v| {
                let (num, unit) = v.split_at(v.len().saturating_sub(1));
                let n: f32 = num.parse().unwrap_or(0.0);
                match unit {
                    "K" => n / 1024.0 / 1024.0,
                    "M" => n / 1024.0,
                    "G" => n,
                    _ => 0.0,
                }
            })
            .unwrap_or(0.0)
    };
    (field("used"), field("total"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_swap_gb() -> (f32, f32) {
    (0.0, 0.0)
}

/// What a filesystem type is, or `None` for one with no capacity to report.
///
/// `None` is an optimization rather than the correctness mechanism: `proc`,
/// `sysfs` and the cgroup filesystems all report zero blocks, so a type missing
/// from this list is still dropped further down by [`read_disks`]'s `total > 0`
/// test. What naming them buys is not making the syscall at all — and on a
/// systemd machine that is thirty-odd mounts per reading.
fn disk_kind(fstype: &str, source: &str) -> Option<DiskKind> {
    const PSEUDO: &[&str] = &[
        "proc",
        "sysfs",
        "cgroup",
        "cgroup2",
        "devpts",
        "securityfs",
        "debugfs",
        "tracefs",
        "pstore",
        "bpf",
        "configfs",
        "fusectl",
        "hugetlbfs",
        "mqueue",
        "autofs",
        "binfmt_misc",
        "rpc_pipefs",
        "nsfs",
        "selinuxfs",
        "efivarfs",
        // macOS's own pseudo filesystem, mounted at `/dev`.
        "devfs",
    ];
    const MEMORY: &[&str] = &["tmpfs", "devtmpfs", "ramfs"];
    const LAYER: &[&str] = &["overlay", "overlayfs", "squashfs", "aufs", "erofs"];
    const NETWORK: &[&str] = &[
        "nfs",
        "nfs4",
        "cifs",
        "smbfs",
        "smb3",
        "sshfs",
        "fuse.sshfs",
        "afs",
        "afpfs",
        "ceph",
        "glusterfs",
        "9p",
        "ncpfs",
        // Not a Linux mount, but the classification decides sweep order and a
        // WebDAV server that has gone away hangs exactly like an NFS one.
        "webdav",
    ];
    if PSEUDO.contains(&fstype) {
        return None;
    }
    Some(if MEMORY.contains(&fstype) {
        DiskKind::Memory
    } else if LAYER.contains(&fstype) {
        DiskKind::Layer
    } else if NETWORK.contains(&fstype) {
        DiskKind::Network
    } else if source.starts_with("/dev/") || fstype == "zfs" {
        // What is left after the four named sets is real storage if something
        // real is behind it. `zfs` is called out because its source is a pool
        // path (`tank/home`) rather than a device node, and it would otherwise
        // be the one common filesystem reported as `Other`.
        DiskKind::Local
    } else {
        DiskKind::Other
    })
}

/// One `statvfs` result as `(used, total)` GiB, or `None` for a filesystem
/// with no capacity to report.
fn vfs_gb(vfs: &rustix::fs::StatVfs) -> Option<(f32, f32)> {
    // `f_frsize` is the fragment size, and the unit `f_blocks` and `f_bavail`
    // are counted in. `f_bsize` is the *preferred I/O* size and is not the same
    // question, though the two are usually equal.
    let unit = if vfs.f_frsize > 0 { vfs.f_frsize } else { vfs.f_bsize } as f64;
    let gib = 1024.0 * 1024.0 * 1024.0;
    let total = vfs.f_blocks as f64 * unit / gib;
    // Available, not free: the blocks a filesystem reserves for root are not
    // space a build can have, and `df` reports it the same way.
    let avail = vfs.f_bavail as f64 * unit / gib;
    (total > 0.0).then(|| ((total - avail).max(0.0) as f32, total as f32))
}

/// Capacity for each mount in `mounts`, in the order given. `None` is a mount
/// that had not answered when the sweep ran out of time.
///
/// **`statvfs` on a mount whose server has gone away blocks in uninterruptible
/// sleep** — not for a timeout, but until the mount is forced away or the
/// server comes back. On the sampler task directly that would stop telemetry
/// for every attached client, and this daemon's own tree is routinely on an SMB
/// mount. So the sweep runs on a blocking thread with a deadline.
///
/// **One thread for the whole sweep, not one per mount.** Per-mount
/// `spawn_blocking` is the obvious shape and it is the wrong one: the mount
/// table is long where a GPU list is short — thirty-odd entries on an ordinary
/// workstation, more on a docker host — so it puts a thread on the blocking
/// pool per mount per reading, to run a syscall that takes microseconds. The
/// sweep is the same work at one.
///
/// Results stream back as they land, so a deadline that expires part-way still
/// keeps every mount read up to that point — which is why the caller probes the
/// local disks before the network ones. The deadline bounds the *wait*, not the
/// syscall: the thread stays parked until the kernel releases it, so a hung
/// mount asked again every tick would park another thread every tick. That is
/// what [`DISK_COOLDOWN`] prevents, and why it is measured in minutes.
async fn statvfs_sweep(mounts: Vec<String>) -> Vec<Option<(f32, f32)>> {
    let mut out = vec![None; mounts.len()];
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::task::spawn_blocking(move || {
        for (i, m) in mounts.iter().enumerate() {
            let cap = rustix::fs::statvfs(m.as_str()).ok().as_ref().and_then(vfs_gb);
            // The sampler stopped waiting; nothing left to report to.
            if tx.send((i, cap)).is_err() {
                return;
            }
        }
    });
    let deadline = tokio::time::Instant::now() + DISK_SWEEP_TIMEOUT;
    // Ends on the deadline, or when the sweep finishes and drops its sender.
    while let Ok(Some((i, cap))) = tokio::time::timeout_at(deadline, rx.recv()).await {
        out[i] = cap;
    }
    out
}

/// A mount table field, with the octal escapes `/proc` writes for the four
/// characters that would otherwise split a line.
#[cfg(target_os = "linux")]
fn unescape_mount(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let oct: String = it.clone().take(3).collect();
        match u8::from_str_radix(&oct, 8) {
            Ok(b) if oct.len() == 3 => {
                out.push(b as char);
                it.nth(2);
            }
            // A backslash that is not an escape is a backslash. Paths may
            // legitimately contain one.
            _ => out.push('\\'),
        }
    }
    out
}

/// The mounts worth asking about, from the text of a mount table.
///
/// Split from [`read_disks`] because everything interesting here is a decision
/// about text — what a filesystem is, which entries are the same disk twice —
/// and none of it needs a syscall to test. Against the real `/proc` these paths
/// are only exercised if the machine running the tests happens to have the
/// mounts that trigger them, which is how an assertion comes to pass whatever
/// the code does.
///
/// Each entry is `(source, mount, fstype, kind)`, in mount-table order.
#[cfg(target_os = "linux")]
fn parse_mounts(table: &str) -> Vec<(String, String, String, DiskKind)> {
    let mut out = Vec::new();
    let mut seen_devices: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in table.lines() {
        let mut f = line.split_whitespace();
        let (Some(source), Some(mount), Some(fstype)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let Some(kind) = disk_kind(fstype, source) else { continue };
        let (mount, source) = (unescape_mount(mount), unescape_mount(source));
        // One row per real device. A btrfs subvolume set and a bind mount both
        // list the same device at several mount points sharing one capacity, so
        // without this a single disk is reported as four full ones. Only
        // `Local` is deduplicated: every tmpfs calls itself `tmpfs`, and
        // collapsing those would report one of them for all.
        if kind == DiskKind::Local && !seen_devices.insert(source.clone()) {
            continue;
        }
        out.push((source, mount, fstype.to_string(), kind));
    }
    out
}

/// The mounts worth asking about, as `(source, mount, fstype, kind)`.
#[cfg(target_os = "linux")]
fn mount_table() -> Vec<(String, String, String, DiskKind)> {
    let Ok(table) = std::fs::read_to_string("/proc/self/mounts") else { return Vec::new() };
    parse_mounts(&table)
}

/// One row of the macOS mount table, before it is judged.
///
/// `nobrowse` is `MNT_DONTBROWSE` — the flag `mount(8)` prints under that name
/// and Finder reads to decide what counts as a disk somebody has. macOS mounts
/// a great deal that does not: `/System/Volumes/VM`, `Preboot`, `Update`,
/// `xarts`, `iSCPreboot`, `Hardware` and `Data` are all mounted on an ordinary
/// machine, all `nobrowse`, and all report the boot container's own size.
#[cfg(target_os = "macos")]
struct MacMount {
    source: String,
    mount: String,
    fstype: String,
    nobrowse: bool,
}

/// The container behind an APFS volume's device node, or the node unchanged.
///
/// `/dev/disk3s1s1` is volume `s1s1` of container `disk3`, and an APFS volume
/// has no size of its own: every volume in a container reports the container's
/// total and the container's free space. The boot disk answers with the same
/// 460 GiB through `/`, `/System/Volumes/Data` and `/System/Volumes/Preboot`
/// alike, so publishing them as rows publishes one disk several times.
///
/// Only APFS is collapsed this way. `/dev/disk4s1` and `/dev/disk4s2` are two
/// partitions of one USB stick, each sized on its own, and they share this
/// prefix while sharing no capacity at all.
#[cfg(target_os = "macos")]
fn apfs_container(source: &str) -> &str {
    const DEV: &str = "/dev/disk";
    let Some(rest) = source.strip_prefix(DEV) else { return source };
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        source
    } else {
        &source[..DEV.len() + digits]
    }
}

/// The macOS mount table, cut down to the disks a person has.
///
/// Split from the syscall for the reason [`parse_mounts`] is split from
/// `/proc`: every decision here is about text and flags, and checked against
/// the machine's own table an assertion only fires if that host happens to
/// have the mounts which trigger it.
#[cfg(target_os = "macos")]
fn select_mounts(rows: Vec<MacMount>) -> Vec<(String, String, String, DiskKind)> {
    let mut out: Vec<(String, String, String, DiskKind)> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for m in rows {
        // macOS's own word for "not a disk the user has", and the whole
        // difference between one row for the boot disk and seven.
        if m.nobrowse {
            continue;
        }
        let Some(kind) = disk_kind(&m.fstype, &m.source) else { continue };
        if kind != DiskKind::Local {
            out.push((m.source, m.mount, m.fstype, kind));
            continue;
        }
        // The Linux reader's rule — one row per real device — with APFS keyed
        // by the container, because that is what holds the capacity.
        let key = if m.fstype == "apfs" { apfs_container(&m.source) } else { m.source.as_str() }
            .to_string();
        match seen.get(&key) {
            // `/` is the name for the boot container whichever of its volumes
            // the table lists first.
            Some(&i) if m.mount == "/" => out[i] = (m.source, m.mount, m.fstype, kind),
            Some(_) => {}
            None => {
                seen.insert(key, out.len());
                out.push((m.source, m.mount, m.fstype, kind));
            }
        }
    }
    out
}

/// A fixed-width C string field, read to its NUL.
#[cfg(target_os = "macos")]
fn c_str_field(raw: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = raw.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// The mounts worth asking about, as `(source, mount, fstype, kind)`.
///
/// `MNT_NOWAIT` answers from the statistics the kernel already holds instead of
/// asking every filesystem to refresh its own. The waiting form blocks right
/// here, on the sampler task, for as long as a dead SMB or NFS server takes to
/// give up — which is the failure [`statvfs_sweep`] exists to keep off it, and
/// this daemon's own tree is routinely on an SMB mount.
#[cfg(target_os = "macos")]
fn mount_table() -> Vec<(String, String, String, DiskKind)> {
    // SAFETY: a null buffer with a zero size is the documented way to ask for
    // nothing but the count.
    let n = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if n <= 0 {
        return Vec::new();
    }
    // Room for mounts that appear between the two calls: the second fills what
    // fits and returns how many it wrote, so spare space is never a lie. The
    // ceiling is for the arithmetic below, and sits far above any real mount
    // table — `DISK_MAX` is what actually cuts the published list.
    let cap = (n as usize + 8).min(4096);
    let mut buf: Vec<libc::statfs> = Vec::with_capacity(cap);
    let bytes = (std::mem::size_of::<libc::statfs>() * cap) as libc::c_int;
    // SAFETY: `buf` has room for `cap` entries and `bytes` says exactly that.
    let n = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    if n <= 0 {
        return Vec::new();
    }
    // SAFETY: the call wrote `n` entries, and cannot have written past `cap`.
    unsafe { buf.set_len((n as usize).min(cap)) };
    select_mounts(
        buf.iter()
            .map(|fs| MacMount {
                source: c_str_field(&fs.f_mntfromname),
                mount: c_str_field(&fs.f_mntonname),
                fstype: c_str_field(&fs.f_fstypename),
                nobrowse: fs.f_flags & libc::MNT_DONTBROWSE as u32 != 0,
            })
            .collect(),
    )
}

/// Platforms with no mount-table reader yet: the rail simply has no disks.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn mount_table() -> Vec<(String, String, String, DiskKind)> {
    Vec::new()
}

/// Every mounted filesystem with a capacity, largest first.
///
/// `prev` is the last reading, and it is not an optimization: a mount that
/// times out keeps the numbers it had rather than reporting zero, which would
/// draw a full disk as an empty one.
///
/// Only [`mount_table`] is per-platform. Everything below it — the cooldown,
/// the order the sweep asks in, the `total > 0` filter, the cap — is one
/// implementation, so a platform that learns to enumerate its mounts gets the
/// rest of the behaviour rather than a second copy of it to drift from.
async fn read_disks(
    prev: &[DiskStat],
    hung: &mut HashMap<String, std::time::Instant>,
) -> Vec<DiskStat> {
    let now = std::time::Instant::now();
    let mut rows: Vec<DiskStat> = mount_table()
        .into_iter()
        .map(|(source, mount, fstype, kind)| {
            let was = prev.iter().find(|d| d.mount == mount);
            DiskStat {
                used_gb: was.map_or(0.0, |d| d.used_gb),
                // A filesystem's size is fixed for the life of the mount, so
                // even a row that never answers can say how big the disk is.
                total_gb: was.map_or(0.0, |d| d.total_gb),
                stale: true,
                mount,
                source,
                fstype,
                kind,
            }
        })
        .collect();

    // Which rows to ask about: everything not resting on a cooldown, ordered so
    // the local disks are read before the network ones. The sweep reports in
    // order and stops at its deadline, so this is what decides who gets an
    // answer when something is stuck — and a hung NFS mount must not cost the
    // machine its own disks.
    let mut probe: Vec<usize> = (0..rows.len())
        .filter(|&i| !hung.get(&rows[i].mount).is_some_and(|until| now < *until))
        .collect();
    probe.sort_by_key(|&i| rows[i].kind == DiskKind::Network);
    let caps = statvfs_sweep(probe.iter().map(|&i| rows[i].mount.clone()).collect()).await;

    for (&i, cap) in probe.iter().zip(caps) {
        match cap {
            Some((used, total)) => {
                hung.remove(&rows[i].mount);
                (rows[i].used_gb, rows[i].total_gb, rows[i].stale) = (used, total, false);
            }
            None => {
                hung.insert(rows[i].mount.clone(), now + DISK_COOLDOWN);
            }
        }
    }
    // Nothing that never had a capacity: the pseudo filesystems that escaped
    // `disk_kind`'s list land here, reporting zero blocks.
    let mut out: Vec<DiskStat> = rows.into_iter().filter(|d| d.total_gb > 0.0).collect();
    // Mounts that went away take their cooldown with them, or a reappearing
    // path would be skipped for a minute it never earned.
    hung.retain(|mount, _| out.iter().any(|d| &d.mount == mount));
    out.sort_by(|a, b| b.total_gb.total_cmp(&a.total_gb));
    out.truncate(DISK_MAX);
    out
}

async fn read_gpu_nvidia() -> Vec<GpuReading> {
    let out = tokio::time::timeout(
        Duration::from_secs(1),
        tokio::process::Command::new("nvidia-smi")
            .args([
                "--query-gpu=utilization.gpu,memory.used,memory.total,name,temperature.gpu,power.draw",
                "--format=csv,noheader,nounits",
            ])
            .output(),
    )
    .await;
    let out = match out {
        Ok(Ok(out)) if out.status.success() => out,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    // One line per GPU: util, mem_used, mem_total, name, temp, power.
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(',').map(str::trim).collect();
            if f.len() < 3 {
                return None;
            }
            let num = |i: usize| f.get(i).and_then(|s| s.parse::<f32>().ok());
            Some(GpuReading {
                pct: num(0)?,
                mem_used_gb: num(1)? / 1024.0,
                mem_total_gb: num(2)? / 1024.0,
                name: f.get(3).map(|s| short_gpu_name(s)).unwrap_or_default(),
                temp_c: num(4),
                power_w: num(5),
            })
        })
        .collect()
}

fn read_gpu_amd() -> Option<GpuReading> {
    let base = std::path::Path::new("/sys/class/drm/card0/device");
    let pct: f32 =
        std::fs::read_to_string(base.join("gpu_busy_percent")).ok()?.trim().parse().ok()?;
    let used: f32 =
        std::fs::read_to_string(base.join("mem_info_vram_used")).ok()?.trim().parse().ok()?;
    let total: f32 =
        std::fs::read_to_string(base.join("mem_info_vram_total")).ok()?.trim().parse().ok()?;
    let gib = 1024.0 * 1024.0 * 1024.0;
    // hwmon exposes temp (millidegrees) and power (microwatts) when present.
    let hwmon = amd_hwmon_dir(base);
    let temp_c = hwmon
        .as_ref()
        .and_then(|d| std::fs::read_to_string(d.join("temp1_input")).ok())
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(|milli| milli / 1000.0);
    let power_w = hwmon
        .as_ref()
        .and_then(|d| std::fs::read_to_string(d.join("power1_average")).ok())
        .and_then(|s| s.trim().parse::<f32>().ok())
        .map(|micro| micro / 1_000_000.0);
    Some(GpuReading {
        pct,
        mem_used_gb: used / gib,
        mem_total_gb: total / gib,
        name: String::new(),
        temp_c,
        power_w,
    })
}

/// The first hwmon directory under an AMD card's device dir.
fn amd_hwmon_dir(base: &std::path::Path) -> Option<std::path::PathBuf> {
    std::fs::read_dir(base.join("hwmon")).ok()?.flatten().map(|e| e.path()).next()
}

/// Containers from `docker ps -a` with their compose project + working_dir
/// labels (empty for standalone containers). `None` when docker is missing.
async fn read_docker() -> Option<Vec<Container>> {
    let out = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::process::Command::new("docker")
            .args([
                "ps",
                "-a",
                "--format",
                "{{.Names}}\t{{.State}}\t{{.Label \"com.docker.compose.project\"}}\t{{.Label \"com.docker.compose.project.working_dir\"}}",
            ])
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .filter_map(|l| {
                let mut parts = l.splitn(4, '\t');
                let name = parts.next()?;
                let state = parts.next()?;
                let project = parts.next().unwrap_or("");
                let workdir = parts.next().unwrap_or("");
                Some(Container {
                    name: name.to_string(),
                    state: state.to_string(),
                    project: project.to_string(),
                    workdir: workdir.to_string(),
                })
            })
            .take(64)
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both Linux (procfs) and macOS (mach) have a real sampler; other targets
    // read zero and are skipped.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn cpu_and_ram_readable() {
        let (busy, total) = read_cpu().expect("cpu counters readable");
        assert!(total >= busy);
        let (used, total) = read_ram_gb().expect("ram readable");
        assert!(total > 0.0 && used <= total);
    }

    /// Every interface is reported, and the real one carries the default route.
    ///
    /// A machine with no network at all would have neither, so the assertions
    /// are conditional on there being something to assert about — what this
    /// pins down is that the reader parses its source rather than that CI has a
    /// particular link up.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn every_interface_is_reported_with_its_kind() {
        let ifaces = tokio::runtime::Runtime::new().unwrap().block_on(read_net());
        if ifaces.is_empty() {
            return; // no interfaces at all: nothing this test can say
        }
        assert!(ifaces.iter().all(|i| !i.name.is_empty()), "an interface came back unnamed");
        assert!(
            ifaces.iter().filter(|i| i.default_route).count() <= 1,
            "two interfaces claimed the default route"
        );
        // Loopback exists on every machine that has any interface at all, and
        // it is the one classification that is the same everywhere.
        if let Some(lo) = ifaces.iter().find(|i| i.name == "lo" || i.name == "lo0") {
            assert_eq!(lo.kind, NetKind::Loopback, "loopback misclassified");
        }
    }

    /// The rail has fourteen cells for a CPU name, so the vendor's marketing
    /// has to come off before it will fit. These are the strings the three
    /// common vendors actually publish.
    #[test]
    fn a_cpu_name_loses_its_marketing() {
        assert_eq!(short_cpu_name("AMD Ryzen 7 5700 Eight-Core Processor"), "Ryzen 7 5700");
        assert_eq!(short_cpu_name("Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz"), "Core i7-9750H");
        assert_eq!(short_cpu_name("AMD Ryzen 9 7950X 16-Core Processor"), "Ryzen 9 7950X");
        assert_eq!(short_cpu_name("Apple M2 Pro"), "Apple M2 Pro");
        // Nothing recognisable is left alone rather than emptied: a name this
        // does not understand is still better than no name.
        assert_eq!(short_cpu_name("Some Unknown Chip 9000"), "Some Unknown Chip 9000");
    }

    /// Whatever the platform reports, the pair has to be self-consistent: SMT
    /// gives threads >= cores, and neither is ever zero when it is reported at
    /// all. A machine that publishes nothing reports `None`, not `Some(0)`.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn the_cpu_core_counts_agree_with_each_other() {
        let (_, cores, threads) = read_cpu_id();
        if let Some(c) = cores {
            assert!(c > 0, "a reported core count must not be zero");
        }
        if let Some(t) = threads {
            assert!(t > 0, "a reported thread count must not be zero");
        }
        if let (Some(c), Some(t)) = (cores, threads) {
            assert!(t >= c, "{t} threads on {c} cores");
        }
    }

    /// Swap is a pair, and used can never exceed total. A machine with swap off
    /// reports `(0, 0)`, which the rail reads as "nothing to say".
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn swap_used_never_exceeds_swap_total() {
        let (used, total) = read_swap_gb();
        assert!(used >= 0.0 && total >= 0.0, "swap went negative: {used}/{total}");
        assert!(used <= total + f32::EPSILON, "{used} GB used of {total} GB total");
    }

    /// A filesystem type is classified by what it *is*, not by whether the
    /// terminal would draw it.
    ///
    /// The four named sets are the ones a client has a reason to skip, and each
    /// for a different reason: memory-backed bytes are already counted as RAM,
    /// an image layer's space is charged again to its backing filesystem, and a
    /// network mount is the one that can hang. Everything behind a device node
    /// is storage.
    #[test]
    fn a_filesystem_is_classified_by_what_it_is() {
        let k = |fs: &str, src: &str| disk_kind(fs, src);
        assert_eq!(k("ext4", "/dev/nvme0n1p2"), Some(DiskKind::Local));
        assert_eq!(k("btrfs", "/dev/sda1"), Some(DiskKind::Local));
        assert_eq!(k("zfs", "tank/home"), Some(DiskKind::Local), "a pool path is still a disk");
        assert_eq!(k("tmpfs", "tmpfs"), Some(DiskKind::Memory));
        assert_eq!(k("squashfs", "/dev/loop3"), Some(DiskKind::Layer), "a snap is not a disk");
        assert_eq!(k("overlay", "overlay"), Some(DiskKind::Layer));
        assert_eq!(k("cifs", "//nas/vol0"), Some(DiskKind::Network));
        assert_eq!(k("nfs4", "nas:/export"), Some(DiskKind::Network));
        // No capacity to report, so there is no row to publish.
        assert_eq!(k("proc", "proc"), None);
        assert_eq!(k("cgroup2", "cgroup2"), None);
        // Unrecognised, and not obviously backed by anything: reported, and
        // left for the client to ignore. Being wrong here costs a row nobody
        // draws; dropping it would hide a real disk on a filesystem this list
        // has not heard of.
        assert_eq!(k("exfat", "/dev/sdb1"), Some(DiskKind::Local));
        assert_eq!(k("fuse.gvfsd-fuse", "gvfsd-fuse"), Some(DiskKind::Other));
    }

    /// `/proc` escapes the four characters that would otherwise split a mount
    /// line. A path with a space in it is ordinary on a removable disk.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_mount_path_survives_its_escapes() {
        assert_eq!(unescape_mount("/media/My\\040Disk"), "/media/My Disk");
        assert_eq!(unescape_mount("/mnt/tab\\011here"), "/mnt/tab\there");
        assert_eq!(unescape_mount("/plain/path"), "/plain/path");
        // A lone backslash is a character, not a broken escape.
        assert_eq!(unescape_mount("/odd\\path"), "/odd\\path");
        assert_eq!(unescape_mount("/odd\\\\134path"), "/odd\\\\path");
    }

    /// The real mount table parses, and what comes back is self-consistent.
    ///
    /// Conditional in the same way [`every_interface_is_reported_with_its_kind`]
    /// is: what this pins down is that the reader parses its source and drops
    /// what has no capacity, not that CI has a particular disk.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn every_mount_reported_has_a_capacity() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut hung = HashMap::new();
        let disks = rt.block_on(read_disks(&[], &mut hung));
        if disks.is_empty() {
            return; // nothing mounted this test can say anything about
        }
        for d in &disks {
            assert!(!d.mount.is_empty(), "a mount came back unnamed");
            assert!(d.total_gb > 0.0, "{} published with no capacity", d.mount);
            assert!(d.used_gb <= d.total_gb + f32::EPSILON, "{} is over full", d.mount);
        }
        // Largest first, or the cap would drop real disks before snaps.
        for pair in disks.windows(2) {
            assert!(pair[0].total_gb >= pair[1].total_gb, "the list is not largest-first");
        }
        // A machine that has any filesystem at all has a root one.
        assert!(disks.iter().any(|d| d.mount == "/"), "no root filesystem");
    }

    /// One disk is one row, however many places it is mounted.
    ///
    /// A synthetic table rather than the machine's own, because this assertion
    /// is only *reachable* on a host that has duplicate mounts — checked
    /// against the real `/proc/self/mounts` here it passed with the
    /// deduplication deleted, which is a test that proves nothing.
    ///
    /// The table below is a btrfs subvolume set, a bind mount, four snaps and
    /// the tmpfs crowd: the shape an ordinary Ubuntu workstation actually has.
    #[test]
    #[cfg(target_os = "linux")]
    fn one_disk_is_one_row_however_often_it_is_mounted() {
        let table = "\
/dev/nvme0n1p2 / btrfs rw,subvol=@ 0 0
/dev/nvme0n1p2 /home btrfs rw,subvol=@home 0 0
/dev/nvme0n1p2 /.snapshots btrfs rw,subvol=@snapshots 0 0
/dev/nvme0n1p2 /var/lib/docker btrfs rw,subvol=@docker 0 0
/dev/nvme0n1p1 /boot/efi vfat rw 0 0
proc /proc proc rw 0 0
sysfs /sys sysfs rw 0 0
cgroup2 /sys/fs/cgroup cgroup2 rw 0 0
tmpfs /run tmpfs rw 0 0
tmpfs /dev/shm tmpfs rw 0 0
/dev/loop0 /snap/core22/1 squashfs ro 0 0
/dev/loop1 /snap/firefox/2 squashfs ro 0 0
//nas/vol0 /mnt/nas cifs rw 0 0
";
        let got = parse_mounts(table);
        let by_mount = |m: &str| got.iter().find(|(_, mount, _, _)| mount == m).cloned();

        // The four subvolumes are one disk. The first mount point wins, which
        // in table order is the one a person would name.
        let btrfs: Vec<_> =
            got.iter().filter(|(src, _, _, _)| src == "/dev/nvme0n1p2").collect::<Vec<_>>();
        assert_eq!(btrfs.len(), 1, "one device came back as {} rows", btrfs.len());
        assert_eq!(btrfs[0].1, "/", "the wrong mount point survived deduplication");

        // A second real device is still its own row.
        assert!(by_mount("/boot/efi").is_some(), "a separate device was collapsed into the first");

        // The pseudo filesystems never reach a syscall.
        for gone in ["/proc", "/sys", "/sys/fs/cgroup"] {
            assert!(by_mount(gone).is_none(), "{gone} should not be asked for a capacity");
        }

        // Everything else is published, classified, for the client to skip.
        assert_eq!(by_mount("/run").unwrap().3, DiskKind::Memory);
        assert_eq!(by_mount("/mnt/nas").unwrap().3, DiskKind::Network);
        // Both snaps survive: they share no device, so nothing deduplicates
        // them, and each is honestly its own read-only image.
        let snaps: Vec<_> = got.iter().filter(|(_, _, fs, _)| fs == "squashfs").collect::<Vec<_>>();
        assert_eq!(snaps.len(), 2);
        assert!(snaps.iter().all(|(_, _, _, k)| *k == DiskKind::Layer));
    }

    #[cfg(target_os = "macos")]
    fn mac_mount(source: &str, mount: &str, fstype: &str, nobrowse: bool) -> MacMount {
        MacMount {
            source: source.to_string(),
            mount: mount.to_string(),
            fstype: fstype.to_string(),
            nobrowse,
        }
    }

    /// The boot disk is one row, not the seven volumes it is made of.
    ///
    /// The table is this machine's own `mount` output, verbatim: an Apple
    /// silicon Mac mounts nine filesystems to boot and every one of the eight
    /// that are not `/` reports the same 460 GiB container. Against the real
    /// table the assertion is only *reachable* on a host shaped this way, which
    /// is the same reason [`one_disk_is_one_row_however_often_it_is_mounted`]
    /// uses a written-down one.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_boot_container_is_one_row_and_the_hidden_volumes_are_none() {
        let got = select_mounts(vec![
            mac_mount("/dev/disk3s1s1", "/", "apfs", false),
            mac_mount("devfs", "/dev", "devfs", true),
            mac_mount("/dev/disk3s6", "/System/Volumes/VM", "apfs", true),
            mac_mount("/dev/disk3s2", "/System/Volumes/Preboot", "apfs", true),
            mac_mount("/dev/disk3s4", "/System/Volumes/Update", "apfs", true),
            mac_mount("/dev/disk1s2", "/System/Volumes/xarts", "apfs", true),
            mac_mount("/dev/disk1s1", "/System/Volumes/iSCPreboot", "apfs", true),
            mac_mount("/dev/disk1s3", "/System/Volumes/Hardware", "apfs", true),
            mac_mount("/dev/disk3s5", "/System/Volumes/Data", "apfs", true),
            mac_mount("map auto_home", "/System/Volumes/Data/home", "autofs", true),
            mac_mount("//paul@10.0.0.1/nvme", "/Volumes/nvme", "smbfs", false),
        ]);
        let mounts: Vec<&str> = got.iter().map(|(_, m, _, _)| m.as_str()).collect();
        assert_eq!(mounts, ["/", "/Volumes/nvme"], "the rail got a row it should not have");
        assert_eq!(got[0].3, DiskKind::Local);
        // The SMB mount the daemon's own tree lives on is real storage, kept
        // and classified so the sweep asks it last and the client can skip it.
        assert_eq!(got[1].3, DiskKind::Network);
    }

    /// An APFS container is one disk; two partitions of one stick are two.
    ///
    /// Both pairs share a `/dev/diskN` prefix, and only the APFS pair shares a
    /// capacity — collapsing the other would report a 64 GB stick's two halves
    /// as one and lose whichever was listed second.
    #[test]
    #[cfg(target_os = "macos")]
    fn only_apfs_volumes_collapse_onto_their_container() {
        let got = select_mounts(vec![
            mac_mount("/dev/disk5s1", "/Volumes/Backup", "apfs", false),
            mac_mount("/dev/disk5s2", "/Volumes/Scratch", "apfs", false),
            mac_mount("/dev/disk4s1", "/Volumes/BOOT", "msdos", false),
            mac_mount("/dev/disk4s2", "/Volumes/DATA", "exfat", false),
        ]);
        let mounts: Vec<&str> = got.iter().map(|(_, m, _, _)| m.as_str()).collect();
        assert_eq!(
            mounts,
            ["/Volumes/Backup", "/Volumes/BOOT", "/Volumes/DATA"],
            "the container collapsed too much or too little"
        );
    }

    /// `/` names the boot container however the table is ordered.
    ///
    /// `getfsstat` gives no order worth relying on, and the row that survives
    /// is the one the rail prints — `/System/Volumes/Update` standing in for
    /// the boot disk is the same number under a name nobody recognises.
    #[test]
    #[cfg(target_os = "macos")]
    fn the_root_volume_names_its_container_whenever_it_appears() {
        let got = select_mounts(vec![
            mac_mount("/dev/disk3s4", "/System/Volumes/Update", "apfs", false),
            mac_mount("/dev/disk3s1s1", "/", "apfs", false),
        ]);
        assert_eq!(got.len(), 1, "one container came back as {} rows", got.len());
        assert_eq!(got[0].1, "/", "a volume of the boot container outranked `/`");
    }

    /// The volume suffix is cut, the disk number is not.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_container_is_the_disk_number_not_the_volume() {
        assert_eq!(apfs_container("/dev/disk3s1s1"), "/dev/disk3");
        assert_eq!(apfs_container("/dev/disk10s2"), "/dev/disk10");
        // Already a container, and nothing device-shaped at all.
        assert_eq!(apfs_container("/dev/disk3"), "/dev/disk3");
        assert_eq!(apfs_container("//paul@10.0.0.1/nvme"), "//paul@10.0.0.1/nvme");
        assert_eq!(apfs_container("map auto_home"), "map auto_home");
    }

    /// A mount that does not answer keeps the numbers it had and says so.
    ///
    /// The failure this guards is reporting a full disk as an empty one: zeroing
    /// a stale row would draw 0/932G, which reads as a disk with everything
    /// free rather than one nobody can currently measure.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn a_mount_that_stops_answering_keeps_its_last_reading() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let mut hung = HashMap::new();
        let first = rt.block_on(read_disks(&[], &mut hung));
        let Some(root) = first.iter().find(|d| d.mount == "/").cloned() else { return };
        assert!(!root.stale, "a healthy root filesystem answered but was marked stale");
        // Put `/` on cooldown by hand — the same state a timeout leaves behind,
        // without needing an unreachable NFS server to produce it.
        hung.insert("/".to_string(), std::time::Instant::now() + DISK_COOLDOWN);
        let second = rt.block_on(read_disks(&first, &mut hung));
        let again = second.iter().find(|d| d.mount == "/").expect("root vanished while resting");
        assert!(again.stale, "a mount on cooldown was not marked stale");
        assert_eq!(again.total_gb, root.total_gb, "a resting mount forgot how big it is");
        assert_eq!(again.used_gb, root.used_gb, "a resting mount forgot its last reading");
    }

    /// The virtual interfaces keep no trend series, which is what holds the
    /// `/v1/system` payload down on a docker host — 36 interfaces there spent
    /// 31 KB of a 39 KB body on history nothing draws.
    #[test]
    fn double_counted_interfaces_keep_no_history() {
        for kind in [NetKind::Loopback, NetKind::Bridge, NetKind::Veth] {
            assert!(!keeps_history(kind), "{kind:?} should not carry a series");
        }
        for kind in [NetKind::Wired, NetKind::Wireless, NetKind::Vpn, NetKind::Other] {
            assert!(keeps_history(kind), "{kind:?} should carry a series");
        }
    }
}
