//! Command queues. Work runs to completion on the host unless a fence
//! or a command-list memory wait holds back the queue's timeline.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::object::{self, E_INVALIDARG, HRESULT, Iid, Object, QUEUE, S_OK, Table, This, guid};
use crate::resource::PAGEABLE_IIDS;
use crate::timeline::{Op, Timeline};

const QUEUE_IIDS: [Iid; 4] = [
    guid(
        0x0ec870a6,
        0x5d7e,
        0x4c22,
        [0x8c, 0xfc, 0x5b, 0xaa, 0xe0, 0x76, 0x16, 0xed],
    ),
    PAGEABLE_IIDS[0],
    PAGEABLE_IIDS[1],
    PAGEABLE_IIDS[2],
];

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct QueueDesc {
    kind: u32,
    priority: i32,
    flags: u32,
    nodes: u32,
}

/// The title reads the list type right after the header.
#[repr(C)]
struct Queue {
    object: Object,
    kind: u32,
    desc: QueueDesc,
    timeline: Arc<Timeline>,
}
const _: () = assert!(std::mem::offset_of!(Queue, kind) == 0x10);

fn queue<'a>(this: This) -> &'a Queue {
    // SAFETY: called from queue methods only.
    unsafe { &*this.cast::<Queue>() }
}

pub(crate) extern "system" fn create(
    _: This,
    desc: *const QueueDesc,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(&desc) = (unsafe { desc.as_ref() }) else {
        return E_INVALIDARG;
    };
    tracing::debug!(?desc, "command queue");
    let queue = Box::new(Queue {
        object: if crate::device::newer() {
            Object::new(&NEWER_FUNCTIONS)
        } else {
            Object::new(&FUNCTIONS)
        },
        kind: desc.kind,
        desc,
        timeline: Arc::default(),
    });
    object::hand_out(QUEUE, queue, iid, &QUEUE_IIDS, out)
}

/// The console's queue description: an engine or pipe and a queue on it
/// in place of PC's priority and node mask.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct QueueDescX {
    kind: u32,
    flags: u32,
    engine_or_pipe: u32,
    queue: u32,
}

/// Device method `CreateCommandQueueX`. The host picks engines itself, so
/// the queue is created as on PC.
pub(crate) extern "system" fn create_x(
    this: This,
    desc: *const QueueDescX,
    iid: *const Iid,
    out: *mut This,
) -> HRESULT {
    // SAFETY: the title passes a description (null is tolerated).
    let Some(&x) = (unsafe { desc.as_ref() }) else {
        return E_INVALIDARG;
    };
    tracing::debug!(desc = ?x, "console command queue");
    let desc = QueueDesc {
        kind: x.kind,
        priority: 0,
        flags: x.flags,
        nodes: 1,
    };
    create(this, &desc, iid, out)
}

extern "system" fn query(this: This, iid: *const Iid, out: *mut This) -> HRESULT {
    object::answer(
        QUEUE,
        iid,
        object::is(iid, &QUEUE_IIDS).then_some(this),
        out,
    )
}
extern "system" fn release(this: This) -> u32 {
    object::release_boxed::<Queue>(this)
}

/// Queues the lists, with each list's gates after any earlier lists.
extern "system" fn execute(this: This, count: u32, lists: *const This) {
    if lists.is_null() {
        return;
    }
    #[cfg(windows)]
    if let Some(host) = crate::host::host() {
        // SAFETY: the title passes `count` lists.
        let lists = unsafe { std::slice::from_raw_parts(lists, count as usize) };
        let mut ops = Vec::new();
        let mut runs = Vec::new();
        for list in lists.iter().filter_map(|&l| crate::list::host_list(l)) {
            let gates = list.gates();
            if !gates.is_empty() && !runs.is_empty() {
                ops.push(host_run(host, std::mem::take(&mut runs)));
            }
            for (addr, cmp, value, width, writes) in gates {
                if !writes.is_empty() {
                    ops.push(Op::Run(Box::new(move || {
                        let mut woken = crate::fence::Woken::default();
                        for (at, what, bytes) in writes {
                            woken.add(what.write(at as *mut u8, bytes));
                        }
                        woken.run();
                    })));
                }
                ops.push(Op::WaitValue(addr, cmp, value, width));
            }
            runs.push(list);
        }
        if !runs.is_empty() {
            ops.push(host_run(host, runs));
        }
        queue(this).timeline.submit_batch(ops);
        return;
    }
    let _ = this;
    static ONCE: AtomicBool = AtomicBool::new(false);
    if !ONCE.swap(true, Ordering::Relaxed) {
        tracing::warn!(count, "command lists submitted; no host GPU runs them");
    }
}

#[cfg(windows)]
fn host_run(host: &'static crate::host::Host, lists: Vec<Arc<crate::host::HostList>>) -> Op {
    Op::Run(Box::new(move || {
        let runs: Vec<_> = lists.iter().map(Arc::as_ref).collect();
        host.execute(&runs);
    }))
}

/// PresentX display plane (64-bit ABI). Only source_rect has established
/// semantics after resources; never chase the other pointer or tail words.
#[repr(C)]
pub(crate) struct Plane {
    token: u64,
    count: u32,
    resources: *const This,
    source_rect: *const [u32; 4],
    _common: *const u8,
    _reserved: [u64; 4],
}
const _: () = assert!(size_of::<Plane>() == 0x48);

#[cfg(windows)]
fn compose_planes() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_PRESENT_PLANES").is_some_and(|v| v == "1"))
}
/// Submission tagged with the frame it belongs to `(count, lists, frame
/// token, flags)`; frames are paced elsewhere, so it runs as a plain one.
extern "system" fn execute_x(this: This, count: u32, lists: *const This, _token: u64, _flags: u32) {
    execute(this, count, lists);
}
/// Signal with sync settings for the top and bottom of the pipe.
extern "system" fn signal2_x(
    this: This,
    fence: This,
    value: u64,
    _bottom: *const u8,
    _top: *const u8,
) -> HRESULT {
    signal(this, fence, value)
}
/// Markers and events only matter to profilers.
extern "system" fn marker(_: This) {}
extern "system" fn signal(this: This, fence: This, value: u64) -> HRESULT {
    if fence.is_null() {
        return E_INVALIDARG;
    }
    let fence = crate::fence::Held::new(fence);
    queue(this).timeline.submit(Op::Run(Box::new(move || fence.signal(value))));
    S_OK
}

extern "system" fn wait(this: This, fence: This, value: u64) -> HRESULT {
    if fence.is_null() {
        return E_INVALIDARG;
    }
    queue(this).timeline.submit(Op::Wait(crate::fence::Held::new(fence), value));
    S_OK
}
/// Ticks per second of GPU timestamps (see [`Written::GpuTimestamp`]).
extern "system" fn timestamp_frequency(_: This, out: *mut u64) -> HRESULT {
    if out.is_null() {
        return E_INVALIDARG;
    }
    // SAFETY: checked non-null.
    unsafe { out.write(TIMESTAMP_FREQUENCY) };
    S_OK
}
const TIMESTAMP_FREQUENCY: u64 = 100_000_000;

/// What a list's bottom-of-pipe write stores when the list has run.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Written {
    Value(u64),
    /// The GPU clock: [`TIMESTAMP_FREQUENCY`] ticks per second since the
    /// process started (our GPU work is done on the CPU's timeline).
    GpuTimestamp,
    /// The CPU's performance counter.
    SystemTimestamp,
}

impl Written {
    /// Stores the low `bytes` (4 or 8) and returns the queues it woke.
    pub(crate) fn write(self, at: *mut u8, bytes: usize) -> crate::fence::Woken {
        let value = match self {
            Written::Value(v) => v,
            Written::GpuTimestamp => {
                static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
                let elapsed = START.get_or_init(std::time::Instant::now).elapsed();
                (elapsed.as_nanos() / (1_000_000_000 / u128::from(TIMESTAMP_FREQUENCY))) as u64
            }
            Written::SystemTimestamp => system_timestamp(),
        };
        // SAFETY: the title names writable GPU memory (unified with ours).
        unsafe {
            if bytes == 4 && (at as usize).is_multiple_of(align_of::<AtomicU32>()) {
                (*at.cast::<AtomicU32>()).store(value as u32, Ordering::Release);
            } else if bytes == 4 {
                at.cast::<u32>().write_unaligned(value as u32);
            } else if (at as usize).is_multiple_of(align_of::<AtomicU64>()) {
                (*at.cast::<AtomicU64>()).store(value, Ordering::Release);
            } else {
                at.cast::<u64>().write_unaligned(value);
            }
        }
        crate::fence::written(at as usize, bytes)
    }
}

fn system_timestamp() -> u64 {
    #[cfg(windows)]
    {
        let mut count = 0;
        // SAFETY: a valid out pointer.
        unsafe { windows_sys::Win32::System::Performance::QueryPerformanceCounter(&mut count) };
        count as u64
    }
    #[cfg(not(windows))]
    0
}

/// Frames presented so far.
static FRAMES: AtomicU64 = AtomicU64::new(0);

#[cfg(feature = "diagnostics")]
pub(crate) fn presented() -> u64 {
    FRAMES.load(Ordering::Relaxed)
}

/// Lightweight first-plane trace. Buffer rotation alone is not a new shape.
#[cfg(all(windows, feature = "diagnostics"))]
fn trace_present(planes: u32, first: *const Plane, parameters: *const u8) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("VICTORY_PRESENT_TRACE").is_some_and(|v| v == "1")) {
        return;
    }
    type Shape = (u32, Option<u32>, bool, bool, Vec<Option<(u64, u32, u32)>>);
    static PREVIOUS: std::sync::Mutex<Option<Shape>> = std::sync::Mutex::new(None);
    // SAFETY: PresentX supplies at least its first prefix for a nonempty array.
    let plane = if planes != 0 { unsafe { first.as_ref() } } else { None };
    let count = plane.map(|p| p.count);
    let have_resources = plane.is_some_and(|p| !p.resources.is_null());
    // Bound diagnostic reads/output even for an unexpected count.
    let resources = plane.filter(|p| !p.resources.is_null()).map_or(&[][..], |p| {
        // SAFETY: the caller supplies count resource pointers for this plane,
        // valid until PresentX returns (no deferred reads or host allocation).
        unsafe { std::slice::from_raw_parts(p.resources, p.count.min(64) as usize) }
    });
    let descriptions = resources.iter().map(|&r| crate::resource::present_shape(r)).collect();
    let shape = (planes, count, have_resources, !parameters.is_null(), descriptions);
    let mut prev = PREVIOUS.lock().unwrap_or_else(|e| e.into_inner());
    if prev.as_ref() != Some(&shape) {
        tracing::info!(planes, plane0_count = ?count, token = ?plane.map(|p| p.token),
            plane_array = ?first, parameters = ?parameters, plane0_resources = ?resources,
            plane0_width_height_format = ?shape.4, uninspected_planes = planes.saturating_sub(1),
            "PresentX shape changed");
        *prev = Some(shape);
    }
}

/// Shows the first plane's first resource in our window.
///
/// shortcut: one plane, no scaling or placement, the source expected in
/// the common state.
extern "system" fn present(
    this: This,
    planes: u32,
    plane_parameters: *const Plane,
    _parameters: *const u8,
) -> HRESULT {
    #[cfg(all(windows, feature = "diagnostics"))]
    trace_present(planes, plane_parameters, _parameters);
    // The parameter arrays belong to the caller, so hold only the resource.
    #[cfg(windows)]
    let resource = if planes > 0
        // SAFETY: the title passes `planes` plane descriptions.
        && let Some(plane) = unsafe { plane_parameters.as_ref() }
        && plane.count > 0
        && !plane.resources.is_null()
    {
        // SAFETY: the plane lists `count` resources.
        let resource = unsafe { *plane.resources };
        (!resource.is_null()).then(|| object::Held::new(resource))
    } else {
        None
    };
    #[cfg(not(windows))]
    let resource = { let _ = (planes, plane_parameters); None };
    // Snapshot the rectangles and retain resources before returning to the
    // caller. The only display ABI confirmed so far is one resource per plane.
    #[cfg(windows)]
    let layers = if compose_planes() && (1..=4).contains(&planes) && !plane_parameters.is_null() {
        // SAFETY: caller supplies planes records at the measured stride.
        unsafe { std::slice::from_raw_parts(plane_parameters, planes as usize) }.iter().map(|p| {
            if p.count != 1 || p.resources.is_null() { return None; }
            // SAFETY: the one resource and optional source rect are caller-owned until return.
            let (r, rect) = unsafe { (*p.resources, p.source_rect.as_ref().copied()) };
            (!r.is_null()).then(|| (object::Held::new(r), rect))
        }).collect::<Option<Vec<_>>>()
    } else { None };
    queue(this).timeline.submit(Op::Run(Box::new(move || show(resource, #[cfg(windows)] layers))));
    S_OK
}

fn show(resource: Option<object::Held>, #[cfg(windows)] layers: Option<Vec<(object::Held, Option<[u32;4]>)>>) {
    let frame = FRAMES.fetch_add(1, Ordering::Relaxed) + 1;
    if frame.is_power_of_two() {
        let (draws, dispatches) = crate::list::take_counts();
        #[cfg(windows)]
        let (drawn, dispatched) = {
            static DRAWN: AtomicU64 = AtomicU64::new(0);
            static DISPATCHED: AtomicU64 = AtomicU64::new(0);
            let since = |last: &AtomicU64, now: u64| now - last.swap(now, Ordering::Relaxed);
            (
                since(&DRAWN, crate::draw::drawn()),
                since(&DISPATCHED, crate::draw::dispatched()),
            )
        };
        #[cfg(not(windows))]
        let (drawn, dispatched) = (0, 0);
        tracing::info!(
            frame,
            draws,
            drawn,
            dispatches,
            dispatched,
            "frame presented (since the last line: draws and dispatches, and those run on the host)"
        );
        #[cfg(windows)]
        if frame >= 256 {
            let (hits, misses) = crate::shader_cache::counts();
            tracing::info!(hits, misses, "shaders from the cache and translated so far");
        }
    }
    #[cfg(all(windows, feature = "diagnostics"))]
    {
        crate::capture::presented(frame);
        if crate::capture::listed(frame) || crate::capture::has_targets() {
            crate::capture::dump_targets(frame);
        }
    }
    #[cfg(all(windows, feature = "diagnostics"))]
    let shown_target = crate::capture::take_show_target(frame);
    #[cfg(windows)]
    if let Some(resource) = resource.as_ref().map(object::Held::get)
        && let Some(texture) = crate::resource::host_copy(resource)
        && let Some(host) = crate::host::host()
    {
        let layers = layers.as_ref().and_then(|items| items.iter().map(|(r, rect)| {
            Some((crate::resource::host_copy(r.get())?, *rect))
        }).collect::<Option<Vec<_>>>());
        let shown = host.present(
            &texture,
            frame,
            layers.as_deref(),
            #[cfg(feature = "diagnostics")]
            shown_target.as_deref(),
        );
        #[cfg(feature = "diagnostics")]
        blinks(frame, resource, shown.map_or(0, |(state, _)| state), shown.map(|(_, black)| black));
        #[cfg(not(feature = "diagnostics"))]
        let _ = shown;
    }
    #[cfg(not(windows))]
    let _ = resource;
}
/// Diagnostics: logs a presented frame that is black after one that was
/// not, with what was presented and how many draws ran since.
#[cfg(all(windows, feature = "diagnostics"))]
fn blinks(frame: u64, resource: This, state: u32, shown: Option<bool>) {
    static LAST: std::sync::Mutex<(bool, u64, u64)> = std::sync::Mutex::new((false, 0, 0));
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let drawn = crate::draw::drawn();
    let black = shown.unwrap_or(false);
    let draws = crate::draw::take_frame();
    if black && !last.0 {
        last.2 += 1;
        tracing::warn!(
            frame,
            resource = ?resource,
            state = format_args!("{state:#x}"),
            draws = drawn - last.1,
            copied = shown.is_some(),
            black_frames = last.2,
            levels = ?crate::capture::recent_levels(),
            "a black frame after a frame that was not"
        );
        for d in draws {
            tracing::warn!("black frame {frame}: {d}");
        }
        AFTER.store(frame + 6, Ordering::Relaxed);
    }
    // Six frames on: the levels around it (a fade or a single drop).
    static AFTER: AtomicU64 = AtomicU64::new(0);
    if AFTER.load(Ordering::Relaxed) == frame {
        tracing::warn!(frame, levels = ?crate::capture::recent_levels(), "levels after the black frame");
    }
    *last = (black, drawn, last.2);
}

/// A run of destination pages: how they are mapped (0 pool pages from
/// `start` on, 1 pool page `start` each, 2 unmapped, 3 left as they are)
/// and how many.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct PageRange {
    kind: u32,
    start: u32,
    count: u32,
}

/// The ranges mapped from a destination address on.
#[repr(C)]
pub(crate) struct PageBatch {
    destination: u64,
    count: u32,
    ranges: *const PageRange,
}

/// Bytes per mapped page.
const PAGE: u64 = 64 << 10;

/// `CopyPageMappingsBatchX(count, batches, pool, pool pages, flags)`: maps
/// pages of a pool (memory the title mapped physical pages into) at the
/// batches' addresses. Grand Theft Auto VI streams textures into pool pages
/// and maps them into its tiled heap this way (title+0xf2c470, ranges of
/// one constant page or of unmapped pages). Mapping updates follow the
/// earlier work in the queue, including waits.
extern "system" fn copy_page_mappings(
    this: This,
    count: u32,
    batches: *const PageBatch,
    pool: u64,
    pool_pages: u32,
    _flags: u32,
) -> HRESULT {
    let Some(map) = crate::resource::page_mapper() else {
        return E_INVALIDARG;
    };
    if batches.is_null() && count > 0 {
        return E_INVALIDARG;
    }
    let batches = (count > 0).then(|| {
        // SAFETY: the title passes `count` batches.
        unsafe { std::slice::from_raw_parts(batches, count as usize) }
    });
    let batches: Vec<_> = batches.into_iter().flatten().filter_map(|b| {
        if b.ranges.is_null() || b.count == 0 {
            return None;
        }
        // SAFETY: each batch holds `count` caller-owned ranges. Copy now,
        // because they do not necessarily outlive this call.
        let ranges = unsafe { std::slice::from_raw_parts(b.ranges, b.count as usize) };
        Some((b.destination, ranges.to_vec()))
    }).collect();
    queue(this).timeline.submit(Op::Run(Box::new(move || {
        let mut ok = true;
        for (mut at, ranges) in batches {
            for r in ranges {
                let len = u64::from(r.count) * PAGE;
                let Some(next) = at.checked_add(len) else { ok = false; break; };
                let inside = |pages: u32| r.start.checked_add(pages).is_some_and(|e| e <= pool_pages);
                let source = pool.checked_add(u64::from(r.start) * PAGE);
                ok &= match (r.kind, source) {
                    (0, Some(source)) if inside(r.count) => map(at, source, len),
                    (1, Some(source)) if inside(1) => (0..u64::from(r.count)).all(|k| map(at + k * PAGE, source, PAGE)),
                    (2, _) => map(at, 0, len),
                    (3, _) => true,
                    _ => false,
                };
                at = next;
            }
        }
        if !ok {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| tracing::warn!(pool = format_args!("{pool:#x}"), "a page mapping failed"));
        }
    })));
    S_OK
}

extern "system" fn desc(this: This, out: *mut QueueDesc) {
    if !out.is_null() {
        // SAFETY: checked non-null; the title passes room for a description.
        unsafe { out.write(queue(this).desc) };
    }
}

/// The queue methods by their slot in older titles' queues (the 2020-era
/// layout): IUnknown 3, object 4, device child 1, queue 26 (12 as on PC,
/// then console ones).
const METHODS: [(usize, *const ()); 15] = [
    (0, query as *const ()),
    (2, release as *const ()),
    (7, object::parent_device as *const ()), // 0x38
    (10, execute as *const ()),              // 0x50
    (11, marker as *const ()),               // 0x58 set marker
    (12, marker as *const ()),               // 0x60 begin event
    (13, marker as *const ()),               // 0x68 end event
    (14, signal as *const ()),               // 0x70
    (15, wait as *const ()),                 // 0x78
    (16, timestamp_frequency as *const ()),  // 0x80
    (26, desc as *const ()),                 // 0xd0
    (29, copy_page_mappings as *const ()),   // 0xe8 page mapping batch
    (30, present as *const ()),              // 0xf0
    (33, execute_x as *const ()),            // 0x108
    (32, signal2_x as *const ()),            // 0x100
];

/// Newer titles' queues have two more methods before Signal, which moved
/// from 0x70 to 0x80: every slot from 0x70 on is two later (taken here as
/// starting exactly at 0x70, the methods before it kept in place).
/// Inferred from Grand Theft Auto VI's calls: Signal 0x80 `(fence,
/// value)`, timestamp frequency 0x90 (title+0x24841a6), suspend and resume
/// 0xb8/0xc0, PresentX 0x100, the frame-token submission 0x118 `(count,
/// lists, token, flags)` (title+0x24821dd).
const fn newer_slot(slot: usize) -> usize {
    if slot >= 14 { slot + 2 } else { slot }
}

const fn queue_table(newer: bool) -> [*const (); 36] {
    let mut f = object::d3d12_object(object::missing_table::<QUEUE, 36>());
    let mut i = 0;
    while i < METHODS.len() {
        let (slot, method) = METHODS[i];
        f[if newer { newer_slot(slot) } else { slot }] = method;
        i += 1;
    }
    f
}

static FUNCTIONS: Table<36> = Table(queue_table(false));
static NEWER_FUNCTIONS: Table<36> = Table(queue_table(true));

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::slot;

    #[cfg(all(windows, feature = "diagnostics"))]
    #[test]
    fn present_trace_requires_only_the_first_prefix() {
        let resources = [std::ptr::null(); 2];
        let first = Plane { token: 123, count: 2, resources: resources.as_ptr(),
            source_rect: std::ptr::null(), _common: std::ptr::null(), _reserved: [0;4] };
        // Deliberately supply just the known prefix even for a two-plane report.
        trace_present(2, &first, std::ptr::null());
        trace_present(0, std::ptr::null(), std::ptr::null());
    }

    #[test]
    fn display_plane_abi() {
        assert_eq!(size_of::<Plane>(), 0x48);
        assert_eq!(std::mem::offset_of!(Plane, token), 0);
        assert_eq!(std::mem::offset_of!(Plane, count), 8);
        assert_eq!(std::mem::offset_of!(Plane, resources), 0x10);
        assert_eq!(std::mem::offset_of!(Plane, source_rect), 0x18);
        assert_eq!(std::mem::offset_of!(Plane, _common), 0x20);
    }

    /// Where a queue's signal is: newer layouts have it two slots later.
    /// Another test may switch layouts while this one runs, so the queue's
    /// own table decides.
    fn signal_slot(q: This) -> usize {
        // SAFETY: one of our queues; its first field is the table.
        let table = unsafe { *q.cast::<*const *const ()>() };
        if table == NEWER_FUNCTIONS.0.as_ptr() {
            0x80
        } else {
            0x70
        }
    }

    #[test]
    fn queue_signals_complete_at_once() {
        let null = std::ptr::null();
        let desc = QueueDesc {
            kind: 0,
            priority: 0,
            flags: 0,
            nodes: 0,
        };
        let mut q = null;
        assert_eq!(create(null, &desc, &QUEUE_IIDS[0], &mut q), S_OK);
        // SAFETY: the title reads the type at +0x10.
        assert_eq!(unsafe { q.cast::<u8>().add(0x10).cast::<u32>().read() }, 0);
        let mut fence = null;
        let fence_iid = &crate::fence::FENCE_IIDS[0];
        assert_eq!(
            crate::fence::create(null, 0, 0, fence_iid, &mut fence),
            S_OK
        );
        let signal = slot::<extern "system" fn(This, This, u64) -> HRESULT>(q, signal_slot(q));
        assert_eq!(signal(q, fence, 7), S_OK);
        assert_eq!(
            slot::<extern "system" fn(This) -> u64>(fence, 0x40)(fence),
            7
        );
        assert_eq!(slot::<extern "system" fn(This) -> u32>(q, 0x10)(q), 0);
    }

    /// Grand Theft Auto VI's compute queue: `{COMPUTE, 0, pipe 1, queue 0}`.
    #[test]
    fn console_queues_are_created_as_on_pc() {
        let null = std::ptr::null();
        let desc = QueueDescX {
            kind: 2,
            flags: 0,
            engine_or_pipe: 1,
            queue: 0,
        };
        let mut q = null;
        assert_eq!(create_x(null, &desc, &QUEUE_IIDS[0], &mut q), S_OK);
        assert_eq!(queue(q).desc.kind, 2);
        let none = std::ptr::null();
        assert_eq!(create_x(null, none, &QUEUE_IIDS[0], &mut q), E_INVALIDARG);
    }

    #[test]
    fn newer_queues_have_their_methods_two_slots_later() {
        let (old, new) = (queue_table(false), queue_table(true));
        assert_eq!(old[10], new[10]); // submission stays at 0x50
        assert_eq!(old[14], new[16]); // signal 0x70 → 0x80
        assert_eq!(old[30], new[32]); // present 0xf0 → 0x100
        assert_eq!(new[35], execute_x as *const ()); // 0x118
    }

    #[test]
    fn timestamps_and_values_are_written() {
        let mut at = [0u64; 2];
        Written::Value(0x1_0000_0002).write(at.as_mut_ptr().cast(), 4).run();
        assert_eq!(at[0], 2);
        Written::GpuTimestamp.write(at[1..].as_mut_ptr().cast(), 8).run();
        let first = at[1];
        std::thread::sleep(std::time::Duration::from_millis(2));
        Written::GpuTimestamp.write(at[1..].as_mut_ptr().cast(), 8).run();
        assert!(at[1] > first);
    }
}
