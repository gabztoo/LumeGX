//! Opt-in comparison of selected targets and the actual swap-chain image,
//! in the same present.

use std::sync::{Arc, Mutex, OnceLock};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

use crate::resource::HostTexture;

type Spec = (u64, u32, DXGI_FORMAT);

fn parse_spec(value: &str) -> Option<Spec> {
    let (size, format) = value.trim().split_once("-f")?;
    let (width, height) = size.split_once('x')?;
    let (width, height, format) = (
        width.parse().ok()?,
        height.parse().ok()?,
        DXGI_FORMAT(format.parse().ok()?),
    );
    (width != 0 && height != 0 && super::decoder(format.0 as u32).is_some())
        .then_some((width, height, format))
}

fn family(format: DXGI_FORMAT) -> Option<DXGI_FORMAT> {
    Some(match format {
        DXGI_FORMAT_R8G8B8A8_TYPELESS
        | DXGI_FORMAT_R8G8B8A8_UNORM
        | DXGI_FORMAT_R8G8B8A8_UNORM_SRGB => DXGI_FORMAT_R8G8B8A8_UNORM,
        DXGI_FORMAT_B8G8R8A8_TYPELESS
        | DXGI_FORMAT_B8G8R8A8_UNORM
        | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB => DXGI_FORMAT_B8G8R8A8_UNORM,
        _ => return None,
    })
}

fn sample_shape(desc: &D3D12_RESOURCE_DESC) -> bool {
    desc.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE2D
        && desc.DepthOrArraySize == 1
        && desc.MipLevels == 1
        && desc.SampleDesc
            == (DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            })
        && family(desc.Format).is_some()
}

// CopyResource needs the same dimensions and compatible DXGI format families.
fn compatible(source: &ID3D12Resource, back: &ID3D12Resource) -> bool {
    // SAFETY: both resources are live.
    let (s, b) = unsafe { (source.GetDesc(), back.GetDesc()) };
    sample_shape(&s)
        && sample_shape(&b)
        && (s.Width, s.Height, family(s.Format)) == (b.Width, b.Height, family(b.Format))
}

/// The most targets a present probes.
const KEPT: usize = 8;

/// A matching target: the title resource (as traces and target dumps name
/// it) and its host copy.
pub(crate) type Target = (usize, Arc<HostTexture>);

pub(crate) struct Selection {
    spec: Spec,
    /// Distinct matching targets since the last present, least recently
    /// noted first (at most [`KEPT`]).
    noted: Mutex<Vec<Target>>,
}

impl Selection {
    fn new(spec: Spec) -> Self {
        Self {
            spec,
            noted: Mutex::new(Vec::new()),
        }
    }

    fn note(&self, resource: usize, target: Arc<HostTexture>) {
        // SAFETY: the Arc keeps the texture live.
        let desc = unsafe { target.texture.GetDesc() };
        if desc.Dimension == D3D12_RESOURCE_DIMENSION_TEXTURE2D
            && desc.SampleDesc.Count == 1
            && (desc.Width, desc.Height, desc.Format) == self.spec
        {
            let mut noted = self.noted.lock().unwrap_or_else(|e| e.into_inner());
            noted.retain(|(_, t)| !Arc::ptr_eq(t, &target));
            if noted.len() == KEPT {
                noted.remove(0);
            }
            noted.push((resource, target));
        }
    }

    pub(crate) fn take(&self) -> Vec<Target> {
        std::mem::take(&mut *self.noted.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

pub(crate) fn config() -> Option<&'static Selection> {
    static SELECTION: OnceLock<Option<Selection>> = OnceLock::new();
    SELECTION
        .get_or_init(|| {
            let value = std::env::var_os("VICTORY_PRESENT_COMPARE")?;
            let spec = value.to_str().and_then(parse_spec);
            if spec.is_none() {
                tracing::warn!(
                    "VICTORY_PRESENT_COMPARE expects <width>x<height>-f<8-bit DXGI format>"
                );
            }
            spec.map(Selection::new)
        })
        .as_ref()
}

pub(super) fn note_target(resource: crate::object::This) {
    if let Some(selection) = config()
        && let Some(target) = crate::resource::host_copy(resource)
    {
        selection.note(resource as usize, target);
    }
}

/// Uses the last matching target that can be copied into this back buffer.
/// Every one is probed either way ([`report_target`]).
pub(crate) fn source<'a>(
    selected: &'a [Target],
    original: &'a ID3D12Resource,
    back: &ID3D12Resource,
) -> (&'a ID3D12Resource, &'static str) {
    match selected.iter().rev().find(|(_, t)| compatible(&t.texture, back)) {
        Some((_, target)) => (&target.texture, "copied"),
        None if selected.is_empty() => (original, "not_observed"),
        None => (original, "not_copied"),
    }
}

/// Records the one-to-one blit. Both resources must be in their copy states.
///
/// SAFETY: the list is open, and the caller holds both compatible resources
/// until command execution has finished.
pub(crate) unsafe fn copy_to_back(
    device: &ID3D12Device,
    list: &ID3D12GraphicsCommandList,
    source: &ID3D12Resource,
    back: &ID3D12Resource,
    compare: bool,
) -> Option<ID3D12Resource> {
    // SAFETY: by the caller's contract above.
    unsafe { list.CopyResource(back, source) };
    compare.then(|| probe_back(device, list, back)).flatten()
}

/// Probes the actual back buffer after its copy and restores COPY_DEST for
/// the present path. Unsupported formats are reported as unavailable.
fn probe_back(
    device: &ID3D12Device,
    list: &ID3D12GraphicsCommandList,
    back: &ID3D12Resource,
) -> Option<ID3D12Resource> {
    // SAFETY: a live back buffer; the list is open and has left it in COPY_DEST.
    unsafe {
        if !sample_shape(&back.GetDesc()) {
            return None;
        }
        let dest = D3D12_RESOURCE_STATE_COPY_DEST;
        let src = D3D12_RESOURCE_STATE_COPY_SOURCE;
        list.ResourceBarrier(&[crate::host::transition(back, dest, src)]);
        let buffer = super::probe(device, list, back);
        list.ResourceBarrier(&[crate::host::transition(back, src, dest)]);
        buffer
    }
}

/// Logs a matching target probed in the same list as the present: its
/// largest sampled channel at full scale 1 (floats as they are), and how
/// many of the samples have a channel above 5/255. `None`: not read back.
pub(crate) fn report_target(present_id: u64, index: usize, target: &Target, probe: Option<&ID3D12Resource>) {
    let (resource, texture) = target;
    // SAFETY: the Arc keeps the texture live.
    let d = unsafe { texture.texture.GetDesc() };
    let values = probe.and_then(|b| super::probe_values(b, d.Format.0 as u32));
    let peak = values
        .as_ref()
        .map(|v| v.iter().flatten().copied().fold(0.0f32, f32::max));
    let lit = values
        .as_ref()
        .map(|v| v.iter().filter(|p| p.iter().any(|&c| c > 5.0 / 255.0)).count());
    tracing::info!(
        present_id,
        index,
        resource = format_args!("{resource:#x}"),
        target = format_args!("{}x{}-f{}", d.Width, d.Height, d.Format.0),
        ?peak,
        ?lit,
        samples = super::GRID * super::GRID,
        "present comparison target"
    );
}

pub(crate) fn report(
    present_id: u64,
    buffer_index: u32,
    selection: &'static str,
    result: windows::core::HRESULT,
    source: Option<&ID3D12Resource>,
    back: Option<&ID3D12Resource>,
) -> Option<u8> {
    let source = source.and_then(super::probe_colors);
    let back = back.and_then(super::probe_colors);
    let source_rgb_max = source.iter().flatten().flatten().copied().max();
    let back_rgb_max = back.iter().flatten().flatten().copied().max();
    let sampled_rgb_equal = source.zip(back).map(|(s, b)| s == b);
    tracing::info!(
        present_id,
        buffer_index,
        selection,
        dxgi_hresult = format_args!("{:#010x}", result.0 as u32),
        ?source_rgb_max,
        ?back_rgb_max,
        ?sampled_rgb_equal,
        "present comparison"
    );
    source_rgb_max
}

#[cfg(test)]
mod tests;
