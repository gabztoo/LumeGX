//! Diagnostics (`VICTORY_PIPELINE_STATS=1`): what each graphics draw did on
//! the host GPU, from a pipeline-statistics query around it: vertex or mesh
//! shader invocations, the primitives they made, primitives entering and
//! leaving the clipper, pixel shader invocations. For indirect execution
//! also the first record as the GPU read it (after the clamp,
//! `crate::indirect`) and its count. Summed per pipeline and logged by the
//! host's meter: where a pass's geometry stops.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;

use crate::host::{HostList, host};
use crate::shadow::Shadow;

/// `VICTORY_PIPELINE_STATS=1`.
pub(crate) fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_PIPELINE_STATS").is_some_and(|v| v == "1"))
}

/// Queries per list; later draws are not measured.
const SLOTS: u32 = 4096;

/// Where an indirect draw's first record and count were: kind, the copy
/// and title address of the record, and of the count.
pub(crate) struct Args {
    pub(crate) kind: u32,
    pub(crate) record: (Arc<Shadow>, u64),
    pub(crate) count: Option<(Arc<Shadow>, u64)>,
}

/// A measured draw: its pipeline, and its arguments if indirect.
struct Entry {
    key: String,
    args: Option<Args>,
}

/// One list's queries, resolved into a mapped read-back buffer.
pub(crate) struct ListStats {
    heap: ID3D12QueryHeap,
    kind: D3D12_QUERY_TYPE,
    buffer: ID3D12Resource,
    /// Bytes per query result.
    stride: u64,
    entries: Arc<Mutex<Vec<Entry>>>,
}

#[derive(Default)]
struct Sum {
    draws: u64,
    /// Vertex or mesh shader invocations, and the primitives assembled or
    /// emitted by the mesh shader.
    shaded: u64,
    made: u64,
    clip_in: u64,
    clip_out: u64,
    pixels: u64,
    /// Indirect records read; of them, with no work (zero count, groups or
    /// vertices); mesh records with groups in x but none in y or z; the
    /// groups or vertex instances asked in all.
    indirect: u64,
    empty: u64,
    flat: u64,
    asked: u64,
}

static SUMS: Mutex<Option<HashMap<String, Sum>>> = Mutex::new(None);

/// Whether the GPU counts mesh shader work in pipeline statistics.
fn mesh_stats(device: &ID3D12Device) -> bool {
    let mut o9 = D3D12_FEATURE_DATA_D3D12_OPTIONS9::default();
    // SAFETY: a feature query into a struct of its size.
    unsafe {
        device.CheckFeatureSupport(D3D12_FEATURE_D3D12_OPTIONS9, (&raw mut o9).cast(), size_of_val(&o9) as u32)
    }
    .is_ok_and(|()| o9.MeshShaderPipelineStatsSupported.as_bool())
}

impl ListStats {
    /// Queries for `list`, read after it ran.
    pub(crate) fn new(list: &HostList) -> Option<ListStats> {
        let device = &host()?.device;
        let (heap_type, kind, stride) = if mesh_stats(device) {
            (
                D3D12_QUERY_HEAP_TYPE_PIPELINE_STATISTICS1,
                D3D12_QUERY_TYPE_PIPELINE_STATISTICS1,
                size_of::<D3D12_QUERY_DATA_PIPELINE_STATISTICS1>() as u64,
            )
        } else {
            (
                D3D12_QUERY_HEAP_TYPE_PIPELINE_STATISTICS,
                D3D12_QUERY_TYPE_PIPELINE_STATISTICS,
                size_of::<D3D12_QUERY_DATA_PIPELINE_STATISTICS>() as u64,
            )
        };
        let mut heap: Option<ID3D12QueryHeap> = None;
        let mut buffer: Option<ID3D12Resource> = None;
        let readback = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_READBACK, ..Default::default() };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: u64::from(SLOTS) * stride,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            ..Default::default()
        };
        let mut data = std::ptr::null_mut();
        // SAFETY: plain object creation and mapping of a read-back buffer.
        unsafe {
            device
                .CreateQueryHeap(&D3D12_QUERY_HEAP_DESC { Type: heap_type, Count: SLOTS, NodeMask: 0 }, &mut heap)
                .ok()?;
            device
                .CreateCommittedResource(&readback, D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_COPY_DEST, None, &mut buffer)
                .ok()?;
            buffer.as_ref()?.Map(0, None, Some(&mut data)).ok()?;
        }
        let (heap, buffer) = (heap?, buffer?);
        list.keep(&heap);
        list.keep(&buffer);
        let entries: Arc<Mutex<Vec<Entry>>> = Arc::default();
        let (taken, data) = (entries.clone(), data as usize);
        list.defer_after(move || collect(data, stride, &taken));
        Some(ListStats { heap, kind, buffer, stride, entries })
    }

    /// Starts measuring the next draw: its query slot, if one is left.
    pub(crate) fn begin(&self, list: &HostList) -> Option<u32> {
        let slot = self.entries.lock().unwrap_or_else(|e| e.into_inner()).len() as u32;
        if slot >= SLOTS {
            return None;
        }
        // SAFETY: an open list and our heap.
        unsafe { list.list.BeginQuery(&self.heap, self.kind, slot) };
        Some(slot)
    }

    /// Ends the query of `slot`, begun by [`Self::begin`] around one draw.
    pub(crate) fn end(&self, list: &HostList, slot: u32, key: String, args: Option<Args>) {
        // SAFETY: an open list, our heap and read-back buffer.
        unsafe {
            list.list.EndQuery(&self.heap, self.kind, slot);
            list.list
                .ResolveQueryData(&self.heap, self.kind, slot, 1, &self.buffer, u64::from(slot) * self.stride);
        }
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).push(Entry { key, args });
    }
}

/// After the list ran: its queries and arguments, added to [`SUMS`].
fn collect(data: usize, stride: u64, entries: &Mutex<Vec<Entry>>) {
    let entries = std::mem::take(&mut *entries.lock().unwrap_or_else(|e| e.into_inner()));
    let mut sums = SUMS.lock().unwrap_or_else(|e| e.into_inner());
    let sums = sums.get_or_insert_with(HashMap::new);
    let words = (stride / 8) as usize;
    for (i, e) in entries.iter().enumerate() {
        // SAFETY: the mapped read-back buffer, resolved by the finished list.
        let q = unsafe { std::slice::from_raw_parts((data as *const u64).add(i * words), words) };
        // IAVertices, IAPrimitives, VSInvocations, GSInvocations,
        // GSPrimitives, CInvocations, CPrimitives, PSInvocations, HS, DS,
        // CS, then (statistics 1) AS, MS invocations, MS primitives.
        let ms = |n: usize| q.get(n).copied().unwrap_or(0);
        let s = sums.entry(e.key.clone()).or_default();
        s.draws += 1;
        s.shaded += q[2] + ms(12);
        s.made += q[1] + ms(13);
        s.clip_in += q[5];
        s.clip_out += q[6];
        s.pixels += q[7];
        let Some(a) = &e.args else { continue };
        let word = |(copy, at): &(Arc<Shadow>, u64), n: u64| copy.dword(at + 4 * n).unwrap_or(0);
        let counted = a.count.as_ref().map(|c| word(c, 0));
        let [x, y, z] = [0, 1, 2].map(|n| word(&a.record, n));
        let asked = match a.kind {
            10 => u64::from(x) * u64::from(y) * u64::from(z),
            _ => u64::from(x) * u64::from(y),
        };
        s.indirect += 1;
        s.empty += u64::from(asked == 0 || counted == Some(0));
        s.flat += u64::from(a.kind == 10 && x != 0 && (y == 0 || z == 0));
        s.asked += asked;
    }
}

/// The sums since the last call, most drawn first, one line each (none if
/// nothing was measured).
pub(crate) fn take_report() -> Vec<String> {
    let Some(sums) = SUMS.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return Vec::new();
    };
    let mut rows: Vec<(String, Sum)> = sums.into_iter().collect();
    rows.sort_by_key(|(_, s)| std::cmp::Reverse(s.draws));
    let line = |key: &str, s: &Sum| {
        format!(
            "{} draws: {} vertex/mesh invocations, {} primitives made, clipper {} in {} out, {} pixel invocations; indirect {} records ({} empty, {} mesh with no y/z groups), {} asked: {key}",
            s.draws, s.shaded, s.made, s.clip_in, s.clip_out, s.pixels, s.indirect, s.empty, s.flat, s.asked
        )
    };
    let mut total = Sum::default();
    for (_, s) in &rows {
        total.draws += s.draws;
        total.shaded += s.shaded;
        total.made += s.made;
        total.clip_in += s.clip_in;
        total.clip_out += s.clip_out;
        total.pixels += s.pixels;
        total.indirect += s.indirect;
        total.empty += s.empty;
        total.flat += s.flat;
        total.asked += s.asked;
    }
    let mut out = vec![line("all", &total)];
    out.extend(rows.iter().take(40).map(|(k, s)| line(k, s)));
    out
}
