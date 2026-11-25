//! Present-time viewer for title render targets. It views mip 0, slice 0.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::Ordering::Relaxed;

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::{
    D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST, Fxc::D3DCompile, ID3DBlob,
};
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::{Error, PCSTR, Result, s};

use crate::host::{Host, legal_for, transition};
use crate::resource::HostTexture;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    Float,
    Uint,
    Sint,
}

#[derive(Debug, PartialEq)]
struct View {
    format: DXGI_FORMAT,
    kind: Kind,
    scale: f32,
    flags: u32,
}

const TONEMAP: u32 = 1;
const ENCODE: u32 = 2;
const GREY: u32 = 4;
const FILTER: u32 = 8;

fn view(desc: &D3D12_RESOURCE_DESC) -> Option<View> {
    if desc.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D
        || desc.SampleDesc.Count != 1
        || desc
            .Flags
            .contains(D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE)
    {
        return None;
    }
    // Pick a typed SRV for common typeless color/depth targets. A typed
    // depth-stencil resource itself cannot be reinterpreted as an SRV.
    let depth = desc.Flags.contains(D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL);
    let format = DXGI_FORMAT(match desc.Format.0 {
        53 if !depth => 54,
        0 | 20 | 40 | 45 | 55 => return None,
        f => crate::resource::typed_view_format(f as u32).unwrap_or(f as u32) as i32,
    });
    let (kind, scale) = shader_kind(format);
    let mut flags = 0;
    if !depth && matches!(format.0, 2 | 6 | 10 | 16 | 26 | 34 | 41 | 54 | 67) {
        flags |= TONEMAP | ENCODE;
    }
    if matches!(format.0, 29 | 72 | 75 | 78 | 91 | 93 | 99) {
        flags |= ENCODE;
    }
    if depth || matches!(format.0, 21 | 41..=43 | 46 | 54 | 56..=59 | 61..=65) {
        flags |= GREY;
    }
    Some(View {
        format,
        kind,
        scale,
        flags,
    })
}

fn shader_kind(format: DXGI_FORMAT) -> (Kind, f32) {
    match format.0 {
        3 | 7 | 17 | 42 => (Kind::Uint, 1.0 / u32::MAX as f32),
        4 | 8 | 18 | 43 => (Kind::Sint, 1.0 / i32::MAX as f32),
        12 | 36 | 57 => (Kind::Uint, 1.0 / u16::MAX as f32),
        14 | 38 | 59 => (Kind::Sint, 1.0 / i16::MAX as f32),
        25 => (Kind::Uint, 1.0 / 1023.0),
        30 | 50 | 62 => (Kind::Uint, 1.0 / u8::MAX as f32),
        32 | 52 | 64 => (Kind::Sint, 1.0 / i8::MAX as f32),
        _ => (Kind::Float, 1.0),
    }
}

fn parse_exposure(value: &str) -> Option<f32> {
    let ev = value.trim().parse::<f32>().ok()?;
    (ev.is_finite() && ev > 0.0).then_some(ev)
}

pub(crate) fn exposure() -> f32 {
    static EXPOSURE: OnceLock<f32> = OnceLock::new();
    *EXPOSURE.get_or_init(|| {
        let Ok(value) = std::env::var("VICTORY_SHOW_EXPOSURE") else {
            return 1.0;
        };
        parse_exposure(&value).unwrap_or_else(|| {
            tracing::warn!(%value, "invalid VICTORY_SHOW_EXPOSURE, using 1.0");
            1.0
        })
    })
}

/// Source rectangle in normalized coordinates. A null rect denotes the full image.
fn uv_rect(rect: Option<[u32; 4]>, w: u32, h: u32) -> Option<[f32; 4]> {
    let [l,t,r,b] = rect.unwrap_or([0,0,w,h]);
    if l >= r || t >= b || r > w || b > h { return None; }
    Some([l as f32 / w as f32, t as f32 / h as f32, (r-l) as f32 / w as f32, (b-t) as f32 / h as f32])
}

#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Blend { Over, Opaque, IgnoreAlpha }

impl Blend {
    pub(crate) fn configured() -> Self {
        static MODE: OnceLock<Blend> = OnceLock::new();
        *MODE.get_or_init(|| match std::env::var("VICTORY_PRESENT_PLANES_BLEND").as_deref() {
            Ok("opaque") => Self::Opaque,
            Ok("ignore-alpha") => Self::IgnoreAlpha,
            _ => Self::Over,
        })
    }
}

const SHADER: &str = r#"
#define RS "DescriptorTable(SRV(t0), visibility=SHADER_VISIBILITY_PIXEL)," \
           "RootConstants(num32BitConstants=8, b0, visibility=SHADER_VISIBILITY_PIXEL)," \
           "StaticSampler(s0, filter=FILTER_MIN_MAG_LINEAR_MIP_POINT," \
           "addressU=TEXTURE_ADDRESS_CLAMP, addressV=TEXTURE_ADDRESS_CLAMP, visibility=SHADER_VISIBILITY_PIXEL)"

Texture2DArray<ELEMENT> src : register(t0);
SamplerState linear_clamp : register(s0);
cbuffer Options : register(b0) { float exposure; float scale; uint flags; uint mode; float4 rect; };

struct Vertex { float4 position : SV_Position; float2 uv : TEXCOORD0; };

[RootSignature(RS)]
Vertex vs_main(uint id : SV_VertexID) {
    Vertex v;
    v.uv = float2((id << 1) & 2, id & 2);
    v.position = float4(v.uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return v;
}

[RootSignature(RS)]
float4 ps_main(Vertex v) : SV_Target {
    v.uv = rect.xy + v.uv * rect.zw;
    float4 rgba;
#if FILTERABLE
    if ((flags & 8) != 0) {
        rgba = src.SampleLevel(linear_clamp, float3(v.uv, 0), 0);
    } else
#endif
    {
        uint w, h, slices;
        src.GetDimensions(w, h, slices);
        uint2 p = min((uint2)(v.uv * float2(w, h)), uint2(w - 1, h - 1));
        rgba = (float4)src.Load(int4(p, 0, 0));
    }
    float3 rgb = rgba.rgb * scale;
    if ((flags & 4) != 0) rgb = rgb.rrr;
    if ((flags & 1) != 0) {
        // Reinhard. Clamp first so bright half-float infinities are white.
        rgb = clamp(rgb * exposure, 0.0, 1e20);
        rgb = rgb / (1.0 + rgb);
    }
    if ((flags & 2) != 0) rgb = pow(saturate(rgb), 1.0 / 2.2);
    return float4(saturate(rgb), mode == 1 ? saturate(rgba.a) : 1);
}
"#;

fn compile(kind: Kind, entry: PCSTR, profile: PCSTR) -> Result<ID3DBlob> {
    let prefix = match kind {
        Kind::Float => "#define ELEMENT float4\n#define FILTERABLE 1\n",
        Kind::Uint => "#define ELEMENT uint4\n#define FILTERABLE 0\n",
        Kind::Sint => "#define ELEMENT int4\n#define FILTERABLE 0\n",
    };
    let source = format!("{prefix}{SHADER}");
    let (mut code, mut errors): (Option<ID3DBlob>, Option<ID3DBlob>) = (None, None);
    // SAFETY: source and entry names live through the call; outputs are ours.
    let compiled = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            s!("show-target"),
            None,
            None,
            entry,
            profile,
            0,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = compiled {
        if let Some(errors) = errors {
            tracing::warn!(message = %String::from_utf8_lossy(bytes(&errors)), "show shader compile failed");
        }
        return Err(e);
    }
    code.ok_or_else(Error::empty)
}

fn bytes(blob: &ID3DBlob) -> &[u8] {
    // SAFETY: this slice borrows the blob's own bytes.
    unsafe { std::slice::from_raw_parts(blob.GetBufferPointer().cast(), blob.GetBufferSize()) }
}

struct Pipeline {
    root: ID3D12RootSignature,
    pso: ID3D12PipelineState,
    heap: ID3D12DescriptorHeap,
}

impl Pipeline {
    fn new(device: &ID3D12Device, format: DXGI_FORMAT, kind: Kind, blend: Blend) -> Result<Self> {
        let vs = compile(Kind::Float, s!("vs_main"), s!("vs_5_1"))?;
        let ps = compile(kind, s!("ps_main"), s!("ps_5_1"))?;
        let code = |blob: &ID3DBlob| D3D12_SHADER_BYTECODE {
            pShaderBytecode: bytes(blob).as_ptr().cast(),
            BytecodeLength: bytes(blob).len(),
        };
        // IgnoreAlpha is an additive diagnostic: both RGB surfaces remain
        // visible irrespective of UI alpha. Opaque replaces the whole base.
        let destination = match blend {
            Blend::Opaque => D3D12_BLEND_ZERO,
            Blend::Over => D3D12_BLEND_INV_SRC_ALPHA,
            Blend::IgnoreAlpha => D3D12_BLEND_ONE,
        };
        let target = D3D12_RENDER_TARGET_BLEND_DESC {
            SrcBlend: D3D12_BLEND_ONE,
            BlendEnable: (blend != Blend::Opaque).into(),
            DestBlend: destination,
            BlendOp: D3D12_BLEND_OP_ADD,
            SrcBlendAlpha: D3D12_BLEND_ONE,
            DestBlendAlpha: destination,
            BlendOpAlpha: D3D12_BLEND_OP_ADD,
            LogicOp: D3D12_LOGIC_OP_NOOP,
            RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL.0 as u8,
            ..Default::default()
        };
        let keep = D3D12_DEPTH_STENCILOP_DESC {
            StencilFailOp: D3D12_STENCIL_OP_KEEP,
            StencilDepthFailOp: D3D12_STENCIL_OP_KEEP,
            StencilPassOp: D3D12_STENCIL_OP_KEEP,
            StencilFunc: D3D12_COMPARISON_FUNC_ALWAYS,
        };
        let mut formats = [DXGI_FORMAT_UNKNOWN; 8];
        formats[0] = format;
        // SAFETY: descriptors borrow live objects and bytecode during creation.
        unsafe {
            let root: ID3D12RootSignature = device.CreateRootSignature(0, bytes(&vs))?;
            let pso = device.CreateGraphicsPipelineState(&D3D12_GRAPHICS_PIPELINE_STATE_DESC {
                pRootSignature: std::mem::transmute_copy(&root),
                VS: code(&vs),
                PS: code(&ps),
                BlendState: D3D12_BLEND_DESC {
                    RenderTarget: [target; 8],
                    ..Default::default()
                },
                SampleMask: u32::MAX,
                RasterizerState: D3D12_RASTERIZER_DESC {
                    FillMode: D3D12_FILL_MODE_SOLID,
                    CullMode: D3D12_CULL_MODE_NONE,
                    DepthClipEnable: true.into(),
                    ..Default::default()
                },
                DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
                    DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ZERO,
                    DepthFunc: D3D12_COMPARISON_FUNC_ALWAYS,
                    FrontFace: keep,
                    BackFace: keep,
                    ..Default::default()
                },
                PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                NumRenderTargets: 1,
                RTVFormats: formats,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                ..Default::default()
            })?;
            let heap = device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                Type: D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV,
                NumDescriptors: 4,
                Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                ..Default::default()
            })?;
            Ok(Self { root, pso, heap })
        }
    }
}

#[derive(Default)]
pub(crate) struct Blit {
    pipelines: HashMap<(i32, Kind, Blend), Option<Pipeline>>,
    support: HashMap<i32, D3D12_FORMAT_SUPPORT1>,
    refused: Option<i32>,
}

impl Blit {
    /// Called when creating the presenter or changing swap-chain format,
    /// never when the selected target first appears during loading.
    #[cfg(feature = "diagnostics")]
    pub(crate) fn prepare(
        &mut self,
        device: &ID3D12Device,
        source: DXGI_FORMAT,
        dest: DXGI_FORMAT,
    ) {
        self.prepare_mode(device, source, dest, Blend::Opaque);
    }

    fn prepare_mode(&mut self, device: &ID3D12Device, source: DXGI_FORMAT, dest: DXGI_FORMAT, blend: Blend) {
        // Typeless formats in view() all map to float/normalized SRVs.
        let (kind, _) = shader_kind(source);
        self.pipelines.entry((dest.0, kind, blend)).or_insert_with(|| {
            // SAFETY: a plain query for the calling thread's ID.
            let tid = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
            tracing::info!(source = source.0, destination = dest.0, ?kind, tid, "creating show target blit PSO");
            match Pipeline::new(device, dest, kind, blend) {
                Ok(pipeline) => {
                    tracing::info!(source = source.0, destination = dest.0, tid, exposure = exposure(), "show target blit ready");
                    Some(pipeline)
                }
                Err(e) => {
                    tracing::warn!(error = %e, tid, "show target pipeline unavailable; showing title frame");
                    None
                }
            }
        });
    }

    /// Draws the selected texture to a PRESENT-state swap buffer, leaving
    /// it in COPY_SOURCE so it can be captured or probed. Restores the
    /// source state. On failure records nothing and leaves both untouched.
    /// The presenter serializes the heap's single SRV until execution ends.
    #[cfg(feature = "diagnostics")]
    pub(crate) fn record(
        &mut self,
        host: &Host,
        list: &ID3D12GraphicsCommandList,
        shown: &HostTexture,
        back: &ID3D12Resource,
        exposure: f32,
    ) -> bool {
        self.layer(host, list, shown, back, exposure, 0, None, Blend::Opaque, false)
    }

    /// Leaves back in COPY_SOURCE after each successful layer. Each draw
    /// gets its own descriptor slot until the present fence completes.
    pub(crate) fn layer(&mut self, host: &Host, list: &ID3D12GraphicsCommandList,
        shown: &HostTexture, back: &ID3D12Resource, exposure: f32, index: usize,
        rect: Option<[u32;4]>, blend: Blend, compose: bool) -> bool {
        let texture = &shown.texture;
        // SAFETY: live source and destination, owned by the caller.
        let (d, b) = unsafe { (texture.GetDesc(), back.GetDesc()) };
        let device = &host.device;
        let supported_view = view(&d).and_then(|mut v| {
            let support = *self.support.entry(v.format.0).or_insert_with(|| {
                let mut data = D3D12_FEATURE_DATA_FORMAT_SUPPORT {
                    Format: v.format,
                    ..Default::default()
                };
                // SAFETY: properly sized, initialized format support query.
                unsafe {
                    device.CheckFeatureSupport(
                        D3D12_FEATURE_FORMAT_SUPPORT,
                        (&raw mut data).cast(),
                        size_of::<D3D12_FEATURE_DATA_FORMAT_SUPPORT>() as u32,
                    )
                }
                .map_or(D3D12_FORMAT_SUPPORT1_NONE, |()| data.Support1)
            });
            if !support
                .contains(D3D12_FORMAT_SUPPORT1_TEXTURE2D | D3D12_FORMAT_SUPPORT1_SHADER_LOAD)
            {
                return None;
            }
            // Video/planar resources need explicitly selected plane formats.
            if matches!(v.format.0, 100..=114 | 130..=132) {
                return None;
            }
            if v.kind == Kind::Float && support.contains(D3D12_FORMAT_SUPPORT1_SHADER_SAMPLE) {
                v.flags |= FILTER;
            }
            Some(v)
        });
        let Some(v) = supported_view else {
            if self.refused != Some(d.Format.0) {
                tracing::warn!(format = d.Format.0, samples = d.SampleDesc.Count, flags = ?d.Flags,
                    "show target cannot be read as a single-sample 2D SRV; showing title frame");
                self.refused = Some(d.Format.0);
            }
            return false;
        };
        let Some(uv) = uv_rect(rect, d.Width as u32, d.Height) else { return false; };
        if index >= 4 { return false; }
        let blend = if index == 0 { Blend::Opaque } else { blend };
        if compose { self.prepare_mode(device, v.format, b.Format, blend); }
        let Some(Some(p)) = self.pipelines.get(&(b.Format.0, v.kind, blend)) else {
            return false;
        };
        let state = D3D12_RESOURCE_STATES(legal_for(d.Flags.0 as u32, shown.state.load(Relaxed)) as i32);
        let read = D3D12_RESOURCE_STATE_PIXEL_SHADER_RESOURCE;
        let render = D3D12_RESOURCE_STATE_RENDER_TARGET;
        let constants = [exposure.to_bits(), v.scale.to_bits(), v.flags, u32::from(blend == Blend::Over),
            uv[0].to_bits(), uv[1].to_bits(), uv[2].to_bits(), uv[3].to_bits()];
        let srv = D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: v.format,
            ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2DARRAY,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING,
            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2DArray: D3D12_TEX2D_ARRAY_SRV {
                    MipLevels: 1,
                    ArraySize: 1,
                    ..Default::default()
                },
            },
        };
        // SAFETY: an open presenter list and live resources. Its previous use
        // of the heap finished before the presenter could record another.
        unsafe {
            let step = device.GetDescriptorHandleIncrementSize(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV) as usize;
            let mut cpu = p.heap.GetCPUDescriptorHandleForHeapStart();
            let mut gpu = p.heap.GetGPUDescriptorHandleForHeapStart();
            cpu.ptr += index * step;
            gpu.ptr += (index * step) as u64;
            device.CreateShaderResourceView(texture, Some(&srv), cpu);
            let rtv = host.rtv(back, None);
            if state != read {
                list.ResourceBarrier(&[transition(texture, state, read)]);
            }
            list.ResourceBarrier(&[transition(back, if index == 0 { D3D12_RESOURCE_STATE_PRESENT } else { D3D12_RESOURCE_STATE_COPY_SOURCE }, render)]);
            list.SetPipelineState(&p.pso);
            list.SetGraphicsRootSignature(&p.root);
            list.SetDescriptorHeaps(&[Some(p.heap.clone())]);
            list.SetGraphicsRootDescriptorTable(0, gpu);
            list.SetGraphicsRoot32BitConstants(
                1,
                constants.len() as u32,
                constants.as_ptr().cast(),
                0,
            );
            list.RSSetViewports(&[D3D12_VIEWPORT {
                Width: b.Width as f32,
                Height: b.Height as f32,
                MaxDepth: 1.0,
                ..Default::default()
            }]);
            list.RSSetScissorRects(&[RECT {
                left: 0,
                top: 0,
                right: b.Width as i32,
                bottom: b.Height as i32,
            }]);
            list.OMSetRenderTargets(1, Some(&rtv), false, None);
            list.IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            list.DrawInstanced(3, 1, 0, 0);
            if state != read {
                list.ResourceBarrier(&[transition(texture, read, state)]);
            }
            list.ResourceBarrier(&[transition(back, render, D3D12_RESOURCE_STATE_COPY_SOURCE)]);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposure_is_a_positive_finite_multiplier() {
        assert_eq!(parse_exposure(" 2.5 "), Some(2.5));
        for bad in ["", "wat", "NaN", "inf", "0", "-1"] {
            assert_eq!(parse_exposure(bad), None);
        }
    }

    #[test]
    fn views_distinguish_hdr_ldr_depth_and_uint() {
        let mut d = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            ..Default::default()
        };
        for f in [2, 10, 26] {
            d.Format = DXGI_FORMAT(f);
            assert_eq!(view(&d).unwrap().flags, TONEMAP | ENCODE);
        }
        d.Format = DXGI_FORMAT_R8G8B8A8_TYPELESS;
        assert_eq!(view(&d).unwrap().format, DXGI_FORMAT_R8G8B8A8_UNORM);
        assert_eq!(view(&d).unwrap().flags, 0);
        d.Format = DXGI_FORMAT_R8G8B8A8_UINT;
        assert_eq!(view(&d).unwrap().kind, Kind::Uint);
        d.Format = DXGI_FORMAT_R32_TYPELESS;
        d.Flags = D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL;
        assert_eq!(view(&d).unwrap().flags, GREY);
        d.Flags |= D3D12_RESOURCE_FLAG_DENY_SHADER_RESOURCE;
        assert_eq!(view(&d), None);
        d.Flags = D3D12_RESOURCE_FLAG_NONE;
        d.SampleDesc.Count = 4;
        assert_eq!(view(&d), None);
    }

    #[test]
    fn plane_source_rect_is_bounded_and_normalized() {
        assert_eq!(uv_rect(Some([1,2,3,6]), 4,8), Some([0.25,0.25,0.5,0.5]));
        assert_eq!(uv_rect(None,1920,1080), Some([0.,0.,1.,1.]));
        assert!(uv_rect(Some([0,5,2,4]),4,8).is_none());
        assert!(uv_rect(Some([0,0,5,8]),4,8).is_none());
    }

    #[cfg(feature = "diagnostics")]
    #[test]
    fn hdr_blit_fills_destination_with_tonemapped_color() {
        let Some(host) = crate::host::host() else {
            return;
        };
        let device = &host.device;
        let make_target = |format, size, state| {
            let mut target = None;
            // SAFETY: small, single-sample color targets; output is ours.
            unsafe {
                device
                    .CreateCommittedResource(
                        &D3D12_HEAP_PROPERTIES {
                            Type: D3D12_HEAP_TYPE_DEFAULT,
                            ..Default::default()
                        },
                        D3D12_HEAP_FLAG_NONE,
                        &D3D12_RESOURCE_DESC {
                            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                            Width: u64::from(size),
                            Height: size,
                            DepthOrArraySize: 1,
                            MipLevels: 1,
                            Format: format,
                            SampleDesc: DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
                            ..Default::default()
                        },
                        state,
                        None,
                        &mut target,
                    )
                    .unwrap();
            }
            target.unwrap()
        };
        let render = D3D12_RESOURCE_STATE_RENDER_TARGET;
        // Synthetic two-layer PresentX, including GPU readback. Base blue
        // plus premultiplied half-red must produce half-red, half-blue.
        let rgba = DXGI_FORMAT_R8G8B8A8_UNORM;
        for (mode, expected) in [(Blend::Over, [128,0,127]), (Blend::Opaque, [128,0,0]),
            (Blend::IgnoreAlpha, [128,0,255])] {
            let base = HostTexture::new(make_target(rgba, 2, render), render.0 as u32, Vec::new());
            let ui = HostTexture::new(make_target(rgba, 4, render), render.0 as u32, Vec::new());
            let back = make_target(rgba, 2, D3D12_RESOURCE_STATE_PRESENT);
            let list = host.new_list().unwrap();
            let mut blit = Blit::default();
            // SAFETY: both inputs are live render targets. The right half of UI
            // has a different color to check scaling from a cropped source.
            unsafe {
                list.list.ClearRenderTargetView(host.rtv(&base.texture,None), &[0.,0.,1.,1.], None);
                list.list.ClearRenderTargetView(host.rtv(&ui.texture,None), &[0.,1.,0.,1.], None);
                list.list.ClearRenderTargetView(host.rtv(&ui.texture,None), &[0.5,0.,0.,0.5],
                    Some(&[RECT {left:2,top:0,right:4,bottom:4}]));
            }
            assert!(blit.layer(host,&list.list,&base,&back,1.,0,None,mode,true));
            assert!(blit.layer(host,&list.list,&ui,&back,1.,1,Some([2,0,4,4]),mode,true));
            let (buf, fp) = crate::capture::read_back(device,&list.list,&back).unwrap();
            list.close();
            host.execute(&[&list]);
            // SAFETY: completed fence, mapped readback of 2x2 RGBA8 pixels.
            unsafe {
                let mut data = std::ptr::null_mut();
                buf.Map(0,None,Some(&mut data)).unwrap();
                for y in 0..2 { for x in 0..2 {
                    let px = std::slice::from_raw_parts(data.cast::<u8>().add(y * fp.Footprint.RowPitch as usize + x*4), 3);
                    assert!(px.iter().zip(expected).all(|(&v,e)| v.abs_diff(e)<=1), "{mode:?}: {px:?}");
                }}
                buf.Unmap(0,None);
            }
        }
        for format in [DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_R11G11B10_FLOAT] {
            let source = HostTexture::new(make_target(format, 2, render), render.0 as u32, Vec::new());
            let back = make_target(DXGI_FORMAT_R8G8B8A8_UNORM, 9, D3D12_RESOURCE_STATE_PRESENT);
            let list = host.new_list().unwrap();
            let mut blit = Blit::default();
            assert!(!blit.record(host, &list.list, &source, &back, 2.0));
            blit.prepare(device, format, DXGI_FORMAT_R8G8B8A8_UNORM);
            // SAFETY: the source starts in RENDER_TARGET, the RTV was just made.
            unsafe {
                list.list.ClearRenderTargetView(
                    host.rtv(&source.texture, None),
                    &[2.0, 0.5, 0.0, 1.0],
                    None,
                );
            }
            assert!(blit.record(host, &list.list, &source, &back, 2.0));
            let (buffer, footprint) = crate::capture::read_back(device, &list.list, &back).unwrap();
            list.close();
            host.execute(&[&list]);
            assert!(host.log_messages().is_none());
            // SAFETY: the copy finished and the footprint covers 9 x 9 RGBA pixels.
            unsafe {
                let mut data = std::ptr::null_mut();
                buffer.Map(0, None, Some(&mut data)).unwrap();
                let pitch = footprint.Footprint.RowPitch as usize;
                let pixels = std::slice::from_raw_parts(data.cast::<u8>(), pitch * 8 + 9 * 4);
                // Reinhard at exposure 2: (4/5, 1/2, 0), gamma encoded.
                let pixel = |x: usize, y: usize| &pixels[y * pitch + x * 4..][..4];
                for (x, y) in [(0, 0), (8, 0), (4, 4), (0, 8), (8, 8)] {
                    let rgba = pixel(x, y);
                    assert!(
                        rgba[0].abs_diff(230) <= 2 && rgba[1].abs_diff(186) <= 2,
                        "{format:?}: {rgba:?}"
                    );
                    assert_eq!(&rgba[2..], &[0, 255]);
                }
                buffer.Unmap(0, None);
            }
        }
    }

    #[test]
    fn show_shaders_compile() {
        compile(Kind::Float, s!("vs_main"), s!("vs_5_1")).unwrap();
        for kind in [Kind::Float, Kind::Uint, Kind::Sint] {
            compile(kind, s!("ps_main"), s!("ps_5_1")).unwrap();
        }
    }
}
