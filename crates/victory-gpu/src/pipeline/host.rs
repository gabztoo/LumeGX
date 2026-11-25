//! Host pipelines for title pipelines (decision 0007, step 3): the
//! title's graphics state with the translated shaders, under a root
//! signature of our own that the translated shaders' bindings define
//! (`victory_shader::emit`): per stage one block of root constants for the
//! user data, root views for the title's root views, static samplers for
//! the immediate samplers, and directly indexed heaps.

use std::ffi::{CStr, CString};
use std::sync::Arc;

use victory_shader::emit::{MEMORY_SPACE, ROOT_CBV_SPACE, SAMPLER_SPACE, USER_DATA_SPACE};
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::PCSTR;

use super::{Shader, Translated};
use crate::host::host;

/// A stream's graphics state, owned (the title's stream lives only for the
/// creation call). Subobjects the stream leaves out keep D3D12's defaults.
pub(crate) struct GraphicsState {
    blend: D3D12_BLEND_DESC,
    sample_mask: u32,
    rasterizer: D3D12_RASTERIZER_DESC,
    depth: D3D12_DEPTH_STENCIL_DESC,
    /// The title's `DepthBoundsTestEnable` (a `DEPTH_STENCIL1` subobject).
    depth_bounds: bool,
    /// Input elements, their names pointing into `names`.
    inputs: Vec<D3D12_INPUT_ELEMENT_DESC>,
    names: Vec<CString>,
    strip_cut: D3D12_INDEX_BUFFER_STRIP_CUT_VALUE,
    pub(crate) topology: D3D12_PRIMITIVE_TOPOLOGY_TYPE,
    pub(crate) targets: D3D12_RT_FORMAT_ARRAY,
    depth_format: DXGI_FORMAT,
    samples: DXGI_SAMPLE_DESC,
}

impl Default for GraphicsState {
    fn default() -> Self {
        let target = D3D12_RENDER_TARGET_BLEND_DESC {
            SrcBlend: D3D12_BLEND_ONE,
            DestBlend: D3D12_BLEND_ZERO,
            BlendOp: D3D12_BLEND_OP_ADD,
            SrcBlendAlpha: D3D12_BLEND_ONE,
            DestBlendAlpha: D3D12_BLEND_ZERO,
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
        GraphicsState {
            blend: D3D12_BLEND_DESC {
                RenderTarget: [target; 8],
                ..Default::default()
            },
            sample_mask: u32::MAX,
            rasterizer: D3D12_RASTERIZER_DESC {
                FillMode: D3D12_FILL_MODE_SOLID,
                CullMode: D3D12_CULL_MODE_BACK,
                DepthClipEnable: true.into(),
                ..Default::default()
            },
            depth: D3D12_DEPTH_STENCIL_DESC {
                DepthEnable: true.into(),
                DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ALL,
                DepthFunc: D3D12_COMPARISON_FUNC_LESS,
                StencilReadMask: 0xff,
                StencilWriteMask: 0xff,
                FrontFace: keep,
                BackFace: keep,
                ..Default::default()
            },
            inputs: Vec::new(),
            names: Vec::new(),
            strip_cut: D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_DISABLED,
            topology: D3D12_PRIMITIVE_TOPOLOGY_TYPE_UNDEFINED,
            targets: D3D12_RT_FORMAT_ARRAY::default(),
            depth_format: DXGI_FORMAT_UNKNOWN,
            depth_bounds: false,
            samples: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
        }
    }
}

impl GraphicsState {
    /// Takes in one subobject of a stream: its type, then its data at +4
    /// (+8 for those holding pointers).
    ///
    /// # Safety
    /// `at` points at a whole subobject of type `kind`.
    pub(crate) unsafe fn read(&mut self, kind: u32, at: *const u8) {
        // SAFETY: the caller passes a whole subobject.
        unsafe {
            let data = at.add(4);
            match kind {
                8 => self.blend = data.cast::<D3D12_BLEND_DESC>().read_unaligned(),
                9 => self.sample_mask = data.cast::<u32>().read_unaligned(),
                10 => self.rasterizer = data.cast::<D3D12_RASTERIZER_DESC>().read_unaligned(),
                11 => {
                    self.depth = data.cast::<D3D12_DEPTH_STENCIL_DESC>().read_unaligned();
                    self.depth_bounds = false;
                }
                21 => {
                    // The depth bounds test is kept apart: the plain
                    // description has no room for it (see `depth1`).
                    let d = data.cast::<D3D12_DEPTH_STENCIL_DESC1>().read_unaligned();
                    self.depth_bounds = d.DepthBoundsTestEnable.as_bool();
                    self.depth = D3D12_DEPTH_STENCIL_DESC {
                        DepthEnable: d.DepthEnable,
                        DepthWriteMask: d.DepthWriteMask,
                        DepthFunc: d.DepthFunc,
                        StencilEnable: d.StencilEnable,
                        StencilReadMask: d.StencilReadMask,
                        StencilWriteMask: d.StencilWriteMask,
                        FrontFace: d.FrontFace,
                        BackFace: d.BackFace,
                    };
                }
                12 => {
                    let layout = at.add(8).cast::<D3D12_INPUT_LAYOUT_DESC>().read_unaligned();
                    if layout.pInputElementDescs.is_null() {
                        return;
                    }
                    let elements = std::slice::from_raw_parts(
                        layout.pInputElementDescs,
                        layout.NumElements as usize,
                    );
                    self.names = elements
                        .iter()
                        .map(|e| CStr::from_ptr(e.SemanticName.0.cast()).to_owned())
                        .collect();
                    self.inputs = elements
                        .iter()
                        .zip(&self.names)
                        .map(|(e, name)| D3D12_INPUT_ELEMENT_DESC {
                            SemanticName: PCSTR(name.as_ptr().cast()),
                            Format: DXGI_FORMAT(
                                crate::resource::pc_format(e.Format.0 as u32) as i32
                            ),
                            ..*e
                        })
                        .collect();
                }
                13 => {
                    self.strip_cut =
                        D3D12_INDEX_BUFFER_STRIP_CUT_VALUE(data.cast::<i32>().read_unaligned())
                }
                14 => {
                    self.topology =
                        D3D12_PRIMITIVE_TOPOLOGY_TYPE(data.cast::<i32>().read_unaligned())
                }
                15 => {
                    self.targets = data.cast::<D3D12_RT_FORMAT_ARRAY>().read_unaligned();
                    for f in &mut self.targets.RTFormats {
                        f.0 = crate::resource::pc_format(f.0 as u32) as i32;
                    }
                }
                16 => {
                    let f = data.cast::<u32>().read_unaligned();
                    self.depth_format = DXGI_FORMAT(crate::resource::pc_format(f) as i32);
                }
                17 => self.samples = data.cast::<DXGI_SAMPLE_DESC>().read_unaligned(),
                _ => {}
            }
        }
    }

    /// Everything the state holds, as text: part of a pipeline's name in
    /// the library and of the key a title pipeline is started early by.
    pub(crate) fn describe(&self) -> String {
        let inputs: Vec<String> = self
            .inputs
            .iter()
            .zip(&self.names)
            .map(|(e, name)| {
                format!(
                    "{name:?} {} {} {} {} {} {}",
                    e.SemanticIndex, e.Format.0, e.InputSlot, e.AlignedByteOffset, e.InputSlotClass.0, e.InstanceDataStepRate
                )
            })
            .collect();
        // Only with the test on, so names of pipelines without it stay as
        // they were (and their library entries valid).
        let bounds = if self.depth_bounds { " depth-bounds" } else { "" };
        format!(
            "{:?} {} {:?} {:?}{bounds} {inputs:?} {} {} {:?} {} {:?}",
            self.blend,
            self.sample_mask,
            self.rasterizer,
            self.depth,
            self.strip_cut.0,
            self.topology.0,
            self.targets,
            self.depth_format.0,
            self.samples
        )
    }

    /// The depth-stencil description with the title's depth bounds test,
    /// for a pipeline-state stream (`DEPTH_STENCIL1`).
    fn depth1(&self) -> D3D12_DEPTH_STENCIL_DESC1 {
        let d = self.depth;
        D3D12_DEPTH_STENCIL_DESC1 {
            DepthEnable: d.DepthEnable,
            DepthWriteMask: d.DepthWriteMask,
            DepthFunc: d.DepthFunc,
            StencilEnable: d.StencilEnable,
            StencilReadMask: d.StencilReadMask,
            StencilWriteMask: d.StencilWriteMask,
            FrontFace: d.FrontFace,
            BackFace: d.BackFace,
            DepthBoundsTestEnable: self.depth_bounds.into(),
        }
    }

    /// The vertex buffer slots the input layout reads.
    fn slots(&self) -> u32 {
        self.inputs
            .iter()
            .fold(0, |m, e| m | 1 << e.InputSlot.min(31))
    }

    /// The input elements with their byte offsets worked out.
    fn elements(&self) -> Vec<Input> {
        let mut next = [0u32; 32];
        self.inputs
            .iter()
            .map(|e| {
                let slot = e.InputSlot.min(31) as usize;
                let format = e.Format.0 as u32;
                let offset = if e.AlignedByteOffset == D3D12_APPEND_ALIGNED_ELEMENT {
                    next[slot]
                } else {
                    e.AlignedByteOffset
                };
                let bytes = crate::resource::element(format).map_or(0, |(b, _)| b as u32);
                next[slot] = offset + bytes;
                Input {
                    slot: slot as u32,
                    offset,
                    format,
                    per_instance: e.InputSlotClass == D3D12_INPUT_CLASSIFICATION_PER_INSTANCE_DATA,
                }
            })
            .collect()
    }
}

/// One input element: vertex buffer slot, byte offset, format.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Input {
    pub(crate) slot: u32,
    pub(crate) offset: u32,
    pub(crate) format: u32,
    pub(crate) per_instance: bool,
}

/// The console's own primitive topology types.
const RECTANGLES: i32 = 19;
const QUADS: i32 = 32;

/// Whether root SRVs are bound as tables of one raw view of the title
/// buffer's host copy, which ends at the copy's end (reads past it return
/// zeros), rather than as root descriptors, which have no bound: the mesh
/// shaders of the world's hangs fetch through a root SRV with no record
/// limit and read megabytes past the copy (upstream #951).
/// `VICTORY_ROOT_SRV_DIRECT=1` binds root descriptors as before, for A/B
/// runs with one binary; it changes no translation (the root signature is
/// part of the pipeline library's names).
pub(crate) fn bounded_root_srvs() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let direct = std::env::var_os("VICTORY_ROOT_SRV_DIRECT").is_some_and(|v| v == "1");
        if direct {
            tracing::warn!("VICTORY_ROOT_SRV_DIRECT=1: root SRVs bound as root descriptors");
        }
        !direct
    })
}

/// One stage of a host pipeline and where its bindings go.
pub(crate) struct Stage {
    pub(crate) shader: Arc<Shader>,
    /// The root parameter of its user data, and its size in dwords.
    pub(crate) user_data: Option<(u32, u32)>,
    /// Root parameters of its root views: (title root parameter, ours).
    pub(crate) root_cbvs: Vec<(u8, u32)>,
    pub(crate) root_srvs: Vec<(u8, u32)>,
    pub(crate) root_uavs: Vec<(u8, u32)>,
    /// Our root parameter of the heap-usage bitmap
    /// (`crate::descriptor::usage_bitmap`), if the stage marks heap slots.
    pub(crate) heap_used: Option<u32>,
}

impl Stage {
    pub(crate) fn translated(&self) -> &Translated {
        self.shader
            .translated()
            .expect("host pipelines are made from translated shaders")
    }
}

pub(crate) struct HostPipeline {
    /// A compute pipeline (else graphics).
    pub(crate) compute: bool,
    /// A mesh pipeline (graphics without vertex input).
    pub(crate) mesh: bool,
    pub(crate) pso: ID3D12PipelineState,
    pub(crate) root: ID3D12RootSignature,
    /// Vertex, then pixel stage, or the compute stage.
    pub(crate) stages: Vec<Stage>,
    /// The vertex buffer slots it reads (bit per slot).
    pub(crate) slots: u32,
    pub(crate) inputs: Vec<Input>,
    /// Its shaders, for reports of slow submissions.
    pub(crate) name: String,
    /// The host pipeline runs the title's depth bounds test.
    pub(crate) depth_bounds: bool,
    /// A graphics pipeline of the console's rectangle lists whose geometry
    /// shader completes each rectangle (`victory_shader::emit::rectangle_gs`):
    /// it draws them as triangles, one per rectangle.
    pub(crate) rectangles: bool,
}

/// Logs a pipeline that gets no host pipeline, once per reason.
fn refuse(reason: &'static str, shaders: &[Arc<Shader>]) -> Option<HostPipeline> {
    static SEEN: std::sync::Mutex<Vec<&str>> = std::sync::Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if !seen.contains(&reason) {
        seen.push(reason);
        tracing::warn!(?shaders, "no host pipeline: {reason} (logged once)");
    }
    None
}

impl HostPipeline {
    /// A host pipeline for vertex and pixel shaders with the graphics
    /// state `g`, or for one compute shader.
    pub(crate) fn new(shaders: &[Arc<Shader>], g: Option<&GraphicsState>) -> Option<HostPipeline> {
        let find = |stage| shaders.iter().find(|s| s.stage == stage);
        let compute = match shaders {
            [cs] if cs.stage == "compute" => Some(cs),
            _ => None,
        };
        if compute.is_none()
            && shaders
                .iter()
                .any(|s| !matches!(s.stage, "vertex" | "pixel" | "mesh"))
        {
            return refuse(
                "only vertex, mesh, pixel and compute stages are translated",
                shaders,
            );
        }
        let mesh = find("mesh");
        let stages = if let Some(cs) = compute {
            vec![(cs, D3D12_SHADER_VISIBILITY_ALL)]
        } else {
            let first = match (mesh, find("vertex")) {
                (Some(ms), _) => (ms, D3D12_SHADER_VISIBILITY_MESH),
                (None, Some(vs)) => (vs, D3D12_SHADER_VISIBILITY_VERTEX),
                (None, None) => return refuse("no vertex or mesh shader", shaders),
            };
            let mut stages = vec![first];
            stages.extend(find("pixel").map(|ps| (ps, D3D12_SHADER_VISIBILITY_PIXEL)));
            stages
        };
        if mesh.is_some() && !host()?.mesh_shaders() {
            return refuse("the host GPU has no mesh shaders (or no shader model 6.6)", shaders);
        }
        for (s, _) in &stages {
            s.translated()?;
        }
        let host = host()?;

        // The root signature the translated shaders' bindings define.
        let mut params: Vec<D3D12_ROOT_PARAMETER1> = Vec::new();
        let mut samplers = Vec::new();
        let mut bound = Vec::new();
        // Root view tables (UAVs, and SRVs when bounded): their ranges, and
        // (parameter, range, visibility).
        let mut view_ranges: Vec<D3D12_DESCRIPTOR_RANGE1> = Vec::new();
        let mut table_of = Vec::new();
        for &(shader, visibility) in &stages {
            let t = &shader.translated()?.host;
            let mut stage = Stage {
                shader: shader.clone(),
                user_data: None,
                root_cbvs: Vec::new(),
                root_srvs: Vec::new(),
                root_uavs: Vec::new(),
                heap_used: None,
            };
            let dwords = t.user_data_dwords();
            if dwords > 0 {
                stage.user_data = Some((params.len() as u32, dwords));
                params.push(D3D12_ROOT_PARAMETER1 {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
                    Anonymous: D3D12_ROOT_PARAMETER1_0 {
                        Constants: D3D12_ROOT_CONSTANTS {
                            ShaderRegister: 0,
                            RegisterSpace: USER_DATA_SPACE,
                            Num32BitValues: dwords,
                        },
                    },
                    ShaderVisibility: visibility,
                });
            }
            for (slots, kind, out) in [
                (
                    &t.root_cbvs,
                    D3D12_ROOT_PARAMETER_TYPE_CBV,
                    &mut stage.root_cbvs,
                ),
                (
                    &t.root_srvs,
                    D3D12_ROOT_PARAMETER_TYPE_SRV,
                    &mut stage.root_srvs,
                ),
            ] {
                for &slot in slots {
                    out.push((slot, params.len() as u32));
                    if kind == D3D12_ROOT_PARAMETER_TYPE_SRV && bounded_root_srvs() {
                        // A table of one bounded view (see `bounded_root_srvs`).
                        view_ranges.push(D3D12_DESCRIPTOR_RANGE1 {
                            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
                            NumDescriptors: 1,
                            BaseShaderRegister: u32::from(slot),
                            RegisterSpace: ROOT_CBV_SPACE,
                            Flags: D3D12_DESCRIPTOR_RANGE_FLAG_DATA_VOLATILE
                                | D3D12_DESCRIPTOR_RANGE_FLAG_DESCRIPTORS_VOLATILE,
                            OffsetInDescriptorsFromTableStart: 0,
                        });
                        params.push(D3D12_ROOT_PARAMETER1 {
                            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                            ..Default::default()
                        });
                        table_of.push((params.len() - 1, view_ranges.len() - 1, visibility));
                        continue;
                    }
                    params.push(D3D12_ROOT_PARAMETER1 {
                        ParameterType: kind,
                        Anonymous: D3D12_ROOT_PARAMETER1_0 {
                            Descriptor: D3D12_ROOT_DESCRIPTOR1 {
                                ShaderRegister: u32::from(slot),
                                RegisterSpace: ROOT_CBV_SPACE,
                                Flags: D3D12_ROOT_DESCRIPTOR_FLAG_DATA_VOLATILE,
                            },
                        },
                        ShaderVisibility: visibility,
                    });
                }
            }
            // Root UAVs: a table of one bounded view each (a root view has
            // no bounds; see `draw::State::bind`).
            for &slot in &t.root_uavs {
                stage.root_uavs.push((slot, params.len() as u32));
                view_ranges.push(D3D12_DESCRIPTOR_RANGE1 {
                    RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
                    NumDescriptors: 1,
                    BaseShaderRegister: u32::from(slot),
                    RegisterSpace: ROOT_CBV_SPACE,
                    Flags: D3D12_DESCRIPTOR_RANGE_FLAG_DATA_VOLATILE
                        | D3D12_DESCRIPTOR_RANGE_FLAG_DESCRIPTORS_VOLATILE,
                    OffsetInDescriptorsFromTableStart: 0,
                });
                params.push(D3D12_ROOT_PARAMETER1 {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                    ..Default::default()
                });
                table_of.push((params.len() - 1, view_ranges.len() - 1, visibility));
            }
            // The heap-usage bitmap: a table of one view, bounded, as the
            // shader's skipped marks go past its end (`draw::State::bind`).
            if t.heap_usage {
                stage.heap_used = Some(params.len() as u32);
                view_ranges.push(D3D12_DESCRIPTOR_RANGE1 {
                    RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
                    NumDescriptors: 1,
                    BaseShaderRegister: 1,
                    RegisterSpace: MEMORY_SPACE,
                    Flags: D3D12_DESCRIPTOR_RANGE_FLAG_DATA_VOLATILE
                        | D3D12_DESCRIPTOR_RANGE_FLAG_DESCRIPTORS_VOLATILE,
                    OffsetInDescriptorsFromTableStart: 0,
                });
                params.push(D3D12_ROOT_PARAMETER1 {
                    ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                    ..Default::default()
                });
                table_of.push((params.len() - 1, view_ranges.len() - 1, visibility));
            }
            for (n, s) in t.static_samplers.iter().enumerate() {
                samplers.push(D3D12_STATIC_SAMPLER_DESC {
                    Filter: D3D12_FILTER(s.filter as i32),
                    AddressU: D3D12_TEXTURE_ADDRESS_MODE(s.address[0] as i32),
                    AddressV: D3D12_TEXTURE_ADDRESS_MODE(s.address[1] as i32),
                    AddressW: D3D12_TEXTURE_ADDRESS_MODE(s.address[2] as i32),
                    MipLODBias: s.mip_lod_bias,
                    MaxAnisotropy: s.max_anisotropy,
                    ComparisonFunc: D3D12_COMPARISON_FUNC(s.comparison as i32),
                    BorderColor: D3D12_STATIC_BORDER_COLOR(s.border as i32),
                    MinLOD: s.min_lod,
                    MaxLOD: s.max_lod,
                    ShaderRegister: n as u32,
                    RegisterSpace: SAMPLER_SPACE,
                    ShaderVisibility: visibility,
                });
            }
            bound.push(stage);
        }
        // The ranges are complete: point the tables at them.
        for (param, range, visibility) in table_of {
            params[param].Anonymous.DescriptorTable = D3D12_ROOT_DESCRIPTOR_TABLE1 {
                NumDescriptorRanges: 1,
                pDescriptorRanges: &view_ranges[range],
            };
            params[param].ShaderVisibility = visibility;
        }
        let (root, root_blob) = match root_signature(&host.device, &params, &samplers, compute.is_some()) {
            Ok(root) => root,
            Err(e) => {
                tracing::warn!(error = %e, ?shaders, "host root signature refused");
                return None;
            }
        };
        let code = |i: usize| {
            stages.get(i).and_then(|(s, _)| s.translated()).map_or(
                D3D12_SHADER_BYTECODE::default(),
                |t| D3D12_SHADER_BYTECODE {
                    pShaderBytecode: t.host.dxil.as_ptr().cast(),
                    BytecodeLength: t.host.dxil.len(),
                },
            )
        };
        if compute.is_some() {
            let desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
                // SAFETY: a borrowed pointer the description never releases.
                pRootSignature: unsafe { std::mem::transmute_copy(&root) },
                CS: code(0),
                ..Default::default()
            };
            let name = super::library::name("compute", &root_blob, &[&code_bytes(&code(0))], "");
            let made = super::library::pipeline(
                &name,
                // SAFETY: the description points at live data.
                |library, name| unsafe { library.LoadComputePipeline(name, &desc) },
                // SAFETY: as above.
                || unsafe { host.device.CreateComputePipelineState(&desc) },
            );
            let pso = match made {
                Ok(pso) => pso,
                Err(e) => {
                    // A program that declares the console's wave size needs
                    // a host with waves that size (NVIDIA's are 32 lanes).
                    let wave_lanes = stages
                        .first()
                        .and_then(|(s, _)| s.translated())
                        .and_then(|t| wave_lanes(&t.host.dxil));
                    tracing::warn!(error = %e, ?shaders, ?wave_lanes, "host pipeline refused");
                    // (diagnostics: kept with the untranslated ones, for
                    // `victory translate --pipelines` offline)
                    #[cfg(feature = "diagnostics")]
                    if let Some((s, _)) = stages.first() {
                        super::dump_untranslated(s);
                    }
                    host.log_messages();
                    return None;
                }
            };
            tracing::info!(?shaders, "host pipeline");
            crate::host::name(&pso, || format!("{shaders:?}"));
            return Some(HostPipeline {
                compute: true,
                mesh: false,
                pso,
                root,
                stages: bound,
                name: format!("{shaders:?}"),
                slots: 0,
                inputs: Vec::new(),
                depth_bounds: false,
                rectangles: false,
            });
        }
        let Some(g) = g else {
            return refuse("graphics state not kept (plain description)", shaders);
        };
        // The title's depth bounds test, where the host runs it
        // (`Host::depth_bounds`) and a depth target is there to test.
        let depth_bounds = g.depth_bounds && host.depth_bounds() && g.depth_format != DXGI_FORMAT_UNKNOWN;
        let mut state = g.describe();
        if g.depth_bounds && !depth_bounds {
            // A pipeline of its own in the library: the one with the test
            // must not be loaded for one without it, nor the other way.
            state.push_str(" unapplied");
            static WARNED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            if WARNED.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 4 {
                tracing::warn!(
                    ?shaders,
                    host = host.depth_bounds(),
                    dsv = g.depth_format.0,
                    "a pipeline with the depth bounds test, which the host does not run: its draws pass at every depth"
                );
            }
        }
        if mesh.is_some() {
            let name = super::library::name(
                "mesh",
                &root_blob,
                &[&code_bytes(&code(0)), &code_bytes(&code(1))],
                &state,
            );
            let made = mesh_pipeline(&host.device, &root, code(0), code(1), g, depth_bounds, &name);
            let pso = match made {
                Ok(pso) => pso,
                Err(e) => {
                    tracing::warn!(error = %e, ?shaders, "host mesh pipeline refused");
                    host.log_messages();
                    return None;
                }
            };
            tracing::info!(?shaders, depth_bounds, "host mesh pipeline");
            crate::host::name(&pso, || format!("{shaders:?}"));
            return Some(HostPipeline {
                compute: false,
                mesh: true,
                pso,
                root,
                stages: bound,
                name: format!("{shaders:?}"),
                slots: 0,
                inputs: Vec::new(),
                depth_bounds,
                rectangles: false,
            });
        }

        // Rectangle lists: a geometry shader made for the vertex shader adds
        // each rectangle's fourth corner, for indirect draws too (else
        // direct draws expand them on the CPU from vertex inputs, see
        // `draw`).
        let gs = match (g.topology.0, stages.first().and_then(|(s, _)| s.translated())) {
            (RECTANGLES, Some(vs)) => match victory_shader::emit::rectangle_gs(&vs.host.dxil) {
                Ok(gs) => gs,
                Err(e) => {
                    tracing::warn!(error = %e, ?shaders, "no rectangle geometry shader");
                    Vec::new()
                }
            },
            _ => Vec::new(),
        };
        let desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            // SAFETY: a borrowed pointer the description never releases.
            pRootSignature: unsafe { std::mem::transmute_copy(&root) },
            VS: code(0),
            PS: code(1),
            GS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: if gs.is_empty() { std::ptr::null() } else { gs.as_ptr().cast() },
                BytecodeLength: gs.len(),
            },
            BlendState: g.blend,
            SampleMask: g.sample_mask,
            RasterizerState: g.rasterizer,
            DepthStencilState: g.depth,
            InputLayout: D3D12_INPUT_LAYOUT_DESC {
                pInputElementDescs: g.inputs.as_ptr(),
                NumElements: g.inputs.len() as u32,
            },
            IBStripCutValue: g.strip_cut,
            // The console's rectangle and quad types draw as triangles on
            // the host (see `draw`).
            PrimitiveTopologyType: match g.topology.0 {
                RECTANGLES | QUADS => D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
                _ => g.topology,
            },
            NumRenderTargets: g.targets.NumRenderTargets,
            RTVFormats: g.targets.RTFormats,
            DSVFormat: g.depth_format,
            SampleDesc: g.samples,
            ..Default::default()
        };
        let name = super::library::name(
            "graphics",
            &root_blob,
            &[&code_bytes(&code(0)), &code_bytes(&code(1)), &gs],
            &state,
        );
        let made = if depth_bounds {
            graphics_depth_bounds_pipeline(&host.device, &desc, g.depth1(), &name)
        } else {
            super::library::pipeline(
                &name,
                // SAFETY: the description points at live data.
                |library, name| unsafe { library.LoadGraphicsPipeline(name, &desc) },
                // SAFETY: as above.
                || unsafe { host.device.CreateGraphicsPipelineState(&desc) },
            )
        };
        let pso = match made {
            Ok(pso) => pso,
            Err(e) => {
                tracing::warn!(error = %e, ?shaders, "host pipeline refused");
                host.log_messages();
                return None;
            }
        };
        tracing::info!(?shaders, depth_bounds, rectangles = !gs.is_empty(), "host pipeline");
        crate::host::name(&pso, || format!("{shaders:?}"));
        tracing::debug!(
            ?shaders,
            depth = ?g.depth,
            depth_bounds,
            dsv = g.depth_format.0,
            targets = ?&g.targets.RTFormats[..g.targets.NumRenderTargets.min(8) as usize],
            blend = ?g.blend.RenderTarget[0],
            cull = g.rasterizer.CullMode.0,
            "host graphics state"
        );
        Some(HostPipeline {
            compute: false,
            mesh: false,
            pso,
            root,
            stages: bound,
            name: format!("{shaders:?}"),
            slots: g.slots(),
            inputs: g.elements(),
            depth_bounds,
            rectangles: !gs.is_empty(),
        })
    }
}

/// A pipeline-state stream subobject: its type, then its description,
/// pointer-aligned.
#[repr(C, align(8))]
struct Sub<T>(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE, T);

/// A pipeline from a pipeline-state stream (`stream`: whole subobjects
/// pointing at live data), through the library.
fn stream_pipeline<S>(device: &ID3D12Device, stream: &mut S, name: &str) -> windows::core::Result<ID3D12PipelineState> {
    use windows::core::Interface;
    let desc = D3D12_PIPELINE_STATE_STREAM_DESC {
        SizeInBytes: size_of::<S>(),
        pPipelineStateSubobjectStream: std::ptr::from_mut(stream).cast(),
    };
    let device = device.cast::<ID3D12Device2>()?;
    super::library::pipeline(
        name,
        // SAFETY: the stream holds whole subobjects pointing at live data.
        |library, name| unsafe { library.LoadPipeline(name, &desc) },
        // SAFETY: as above.
        || unsafe { device.CreatePipelineState(&desc) },
    )
}

/// A mesh pipeline from the title's graphics state: the pipeline-state
/// stream form (no input layout or topology), with the depth bounds test
/// when `depth_bounds`.
fn mesh_pipeline(
    device: &ID3D12Device,
    root: &ID3D12RootSignature,
    ms: D3D12_SHADER_BYTECODE,
    ps: D3D12_SHADER_BYTECODE,
    g: &GraphicsState,
    depth_bounds: bool,
    name: &str,
) -> windows::core::Result<ID3D12PipelineState> {
    use windows::core::Interface;
    #[repr(C)]
    struct Stream<D> {
        root: Sub<*mut std::ffi::c_void>,
        ms: Sub<D3D12_SHADER_BYTECODE>,
        ps: Sub<D3D12_SHADER_BYTECODE>,
        blend: Sub<D3D12_BLEND_DESC>,
        mask: Sub<u32>,
        raster: Sub<D3D12_RASTERIZER_DESC>,
        depth: Sub<D>,
        targets: Sub<D3D12_RT_FORMAT_ARRAY>,
        dsv: Sub<DXGI_FORMAT>,
        samples: Sub<DXGI_SAMPLE_DESC>,
    }
    /// The stream with the depth-stencil subobject `depth`.
    fn stream<D>(
        root: &ID3D12RootSignature,
        (ms, ps): (D3D12_SHADER_BYTECODE, D3D12_SHADER_BYTECODE),
        g: &GraphicsState,
        depth: Sub<D>,
    ) -> Stream<D> {
        Stream {
            root: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_ROOT_SIGNATURE, root.as_raw()),
            ms: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_MS, ms),
            ps: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PS, ps),
            blend: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_BLEND, g.blend),
            mask: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_MASK, g.sample_mask),
            raster: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RASTERIZER, g.rasterizer),
            depth,
            targets: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RENDER_TARGET_FORMATS, g.targets),
            dsv: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL_FORMAT, g.depth_format),
            samples: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_DESC, g.samples),
        }
    }
    if depth_bounds {
        let depth = Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL1, g.depth1());
        stream_pipeline(device, &mut stream(root, (ms, ps), g, depth), name)
    } else {
        let depth = Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL, g.depth);
        stream_pipeline(device, &mut stream(root, (ms, ps), g, depth), name)
    }
}

/// A vertex and pixel pipeline from its plain description `d` with the
/// depth-stencil state `depth` (the depth bounds test, which the plain
/// description cannot hold): the pipeline-state stream form.
fn graphics_depth_bounds_pipeline(
    device: &ID3D12Device,
    d: &D3D12_GRAPHICS_PIPELINE_STATE_DESC,
    depth: D3D12_DEPTH_STENCIL_DESC1,
    name: &str,
) -> windows::core::Result<ID3D12PipelineState> {
    #[repr(C)]
    struct Stream {
        root: Sub<*mut std::ffi::c_void>,
        vs: Sub<D3D12_SHADER_BYTECODE>,
        ps: Sub<D3D12_SHADER_BYTECODE>,
        gs: Sub<D3D12_SHADER_BYTECODE>,
        blend: Sub<D3D12_BLEND_DESC>,
        mask: Sub<u32>,
        raster: Sub<D3D12_RASTERIZER_DESC>,
        depth: Sub<D3D12_DEPTH_STENCIL_DESC1>,
        input: Sub<D3D12_INPUT_LAYOUT_DESC>,
        strip_cut: Sub<D3D12_INDEX_BUFFER_STRIP_CUT_VALUE>,
        topology: Sub<D3D12_PRIMITIVE_TOPOLOGY_TYPE>,
        targets: Sub<D3D12_RT_FORMAT_ARRAY>,
        dsv: Sub<DXGI_FORMAT>,
        samples: Sub<DXGI_SAMPLE_DESC>,
    }
    let mut targets = D3D12_RT_FORMAT_ARRAY {
        RTFormats: d.RTVFormats,
        NumRenderTargets: d.NumRenderTargets,
    };
    targets.NumRenderTargets = targets.NumRenderTargets.min(8);
    let mut stream = Stream {
        // (the description's root signature, a borrowed pointer)
        root: Sub(
            D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_ROOT_SIGNATURE,
            // SAFETY: reads the pointer the description holds; the
            // description owns no reference and the root signature
            // outlives the call.
            unsafe { std::mem::transmute_copy(&d.pRootSignature) },
        ),
        vs: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_VS, d.VS),
        ps: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PS, d.PS),
        gs: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_GS, d.GS),
        blend: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_BLEND, d.BlendState),
        mask: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_MASK, d.SampleMask),
        raster: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RASTERIZER, d.RasterizerState),
        depth: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL1, depth),
        input: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_INPUT_LAYOUT, d.InputLayout),
        strip_cut: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_IB_STRIP_CUT_VALUE, d.IBStripCutValue),
        topology: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_PRIMITIVE_TOPOLOGY, d.PrimitiveTopologyType),
        targets: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_RENDER_TARGET_FORMATS, targets),
        dsv: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_DEPTH_STENCIL_FORMAT, d.DSVFormat),
        samples: Sub(D3D12_PIPELINE_STATE_SUBOBJECT_TYPE_SAMPLE_DESC, d.SampleDesc),
    };
    stream_pipeline(device, &mut stream, name)
}

/// The bytes of shader code (none for an absent stage).
fn code_bytes(code: &D3D12_SHADER_BYTECODE) -> Vec<u8> {
    if code.pShaderBytecode.is_null() {
        return Vec::new();
    }
    // SAFETY: a translation's own bytes, alive while its shader is.
    unsafe { std::slice::from_raw_parts(code.pShaderBytecode.cast::<u8>(), code.BytecodeLength) }.to_vec()
}

fn root_signature(
    device: &ID3D12Device,
    params: &[D3D12_ROOT_PARAMETER1],
    samplers: &[D3D12_STATIC_SAMPLER_DESC],
    compute: bool,
) -> windows::core::Result<(ID3D12RootSignature, Vec<u8>)> {
    let input = if compute {
        D3D12_ROOT_SIGNATURE_FLAG_NONE
    } else {
        D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT
    };
    let desc = D3D12_VERSIONED_ROOT_SIGNATURE_DESC {
        Version: windows::Win32::Graphics::Direct3D12::D3D_ROOT_SIGNATURE_VERSION_1_1,
        Anonymous: D3D12_VERSIONED_ROOT_SIGNATURE_DESC_0 {
            Desc_1_1: D3D12_ROOT_SIGNATURE_DESC1 {
                NumParameters: params.len() as u32,
                pParameters: params.as_ptr(),
                NumStaticSamplers: samplers.len() as u32,
                pStaticSamplers: samplers.as_ptr(),
                Flags: input
                    | D3D12_ROOT_SIGNATURE_FLAG_CBV_SRV_UAV_HEAP_DIRECTLY_INDEXED
                    | D3D12_ROOT_SIGNATURE_FLAG_SAMPLER_HEAP_DIRECTLY_INDEXED,
            },
        },
    };
    // SAFETY: the description points at live arrays; the blob is read in
    // full before it is released.
    unsafe {
        let mut blob: Option<ID3DBlob> = None;
        let mut error: Option<ID3DBlob> = None;
        if let Err(e) = D3D12SerializeVersionedRootSignature(&desc, &mut blob, Some(&mut error)) {
            if let Some(error) = error {
                let text = std::slice::from_raw_parts(
                    error.GetBufferPointer().cast::<u8>(),
                    error.GetBufferSize(),
                );
                tracing::warn!("root signature: {}", String::from_utf8_lossy(text));
            }
            return Err(e);
        }
        let blob = blob.ok_or_else(windows::core::Error::empty)?;
        let bytes =
            std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize());
        Ok((device.CreateRootSignature(0, bytes)?, bytes.to_vec()))
    }
}

/// The wave size a translated program declares (`WaveSize`, from its
/// pipeline state validation part), if it declares one.
fn wave_lanes(dxil: &[u8]) -> Option<u32> {
    let container = victory_shader::Container::parse(dxil).ok()?;
    let info = container.part(victory_shader::FourCC::PSV0)?.data;
    // The info's size, then minimum and maximum wave lanes at 16 and 20.
    let min = u32::from_le_bytes(info.get(20..24)?.try_into().ok()?);
    (min != 0).then_some(min)
}

#[cfg(test)]
mod tests {
    /// A container with one PSV0 part whose info declares `min..=max`
    /// wave lanes (as the emitter writes it).
    fn container(min: u32, max: u32) -> Vec<u8> {
        let mut info = vec![0u8; 52];
        info[0..4].copy_from_slice(&48u32.to_le_bytes());
        info[20..24].copy_from_slice(&min.to_le_bytes());
        info[24..28].copy_from_slice(&max.to_le_bytes());
        let part_at = 0x20 + 4;
        let total = part_at + 8 + info.len();
        let mut c = b"DXBC".to_vec();
        c.extend([0u8; 16]);
        c.extend(1u16.to_le_bytes());
        c.extend(0u16.to_le_bytes());
        c.extend((total as u32).to_le_bytes());
        c.extend(1u32.to_le_bytes());
        c.extend((part_at as u32).to_le_bytes());
        c.extend(b"PSV0");
        c.extend((info.len() as u32).to_le_bytes());
        c.extend(info);
        c
    }

    #[test]
    fn declared_wave_lanes_are_read_from_the_container() {
        assert_eq!(super::wave_lanes(&container(64, 64)), Some(64));
        assert_eq!(super::wave_lanes(&container(0, u32::MAX)), None);
        assert_eq!(super::wave_lanes(b"not a container"), None);
    }
    use super::{GraphicsState, graphics_depth_bounds_pipeline, root_signature};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::Ordering;
    use windows::Win32::Graphics::Direct3D12::*;
    use windows::Win32::Graphics::Dxgi::Common::*;

    /// Subobject 21 (`DEPTH_STENCIL1`) with the depth bounds test on and
    /// a depth test that always passes and writes nothing: only the bounds
    /// gate a draw. (With the depth test off, WARP drew at every depth
    /// where the RTX 5050 applied the bounds.)
    #[repr(C, align(8))]
    struct Sub21(u32, D3D12_DEPTH_STENCIL_DESC1);

    fn bounds_only() -> Sub21 {
        Sub21(
            21,
            D3D12_DEPTH_STENCIL_DESC1 {
                DepthEnable: true.into(),
                DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ZERO,
                DepthFunc: D3D12_COMPARISON_FUNC_ALWAYS,
                DepthBoundsTestEnable: true.into(),
                ..Default::default()
            },
        )
    }

    /// The test a `DEPTH_STENCIL1` subobject enables is kept, names a
    /// pipeline apart from one without it, and goes back into the stream
    /// description; a plain `DEPTH_STENCIL` subobject after it turns it off.
    #[test]
    fn a_depth_bounds_test_is_kept_from_the_stream() {
        let mut g = GraphicsState::default();
        let plain = g.describe();
        assert!(!plain.contains("depth-bounds"));
        let sub = bounds_only();
        // SAFETY: `sub` is a whole subobject 21.
        unsafe { g.read(21, (&raw const sub).cast()) };
        assert!(g.depth_bounds);
        assert!(g.describe().contains("depth-bounds"));
        assert_ne!(g.describe(), plain);
        assert!(g.depth1().DepthBoundsTestEnable.as_bool());
        assert!(g.depth1().DepthEnable.as_bool());
        #[repr(C, align(8))]
        struct Sub11(u32, D3D12_DEPTH_STENCIL_DESC);
        let sub = Sub11(11, D3D12_DEPTH_STENCIL_DESC::default());
        // SAFETY: `sub` is a whole subobject 11.
        unsafe { g.read(11, (&raw const sub).cast()) };
        assert!(!g.depth_bounds);
        assert!(!g.depth1().DepthBoundsTestEnable.as_bool());
    }

    const VS: &str = "float4 main(uint i : SV_VertexID) : SV_Position {
        float2 p = float2((i << 1) & 2, i & 2);
        return float4(p * 2.0 - 1.0, 0.5, 1.0);
    }";
    const PS: &str = "float4 main() : SV_Target { return 1; }";

    /// The PC `dxc.exe` (`VICTORY_PC_DXC`, else the newest Windows SDK's).
    fn dxc() -> Option<PathBuf> {
        let dxc = match std::env::var_os("VICTORY_PC_DXC") {
            Some(p) => PathBuf::from(p),
            None => victory_shader::emit::validator::Validator::default_path()?.with_file_name("dxc.exe"),
        };
        dxc.exists().then_some(dxc)
    }

    fn compile(dxc: &Path, dir: &Path, src: &str, profile: &str) -> Vec<u8> {
        let (hlsl, out) = (dir.join(format!("{profile}.hlsl")), dir.join(format!("{profile}.dxil")));
        std::fs::write(&hlsl, src).unwrap();
        let status = std::process::Command::new(dxc)
            .args(["-nologo", "-T", profile, "-E", "main", "-Fo"])
            .arg(&out)
            .arg(&hlsl)
            .status()
            .unwrap();
        assert!(status.success(), "dxc failed for {profile}");
        std::fs::read(out).unwrap()
    }

    fn code(bytes: &[u8]) -> D3D12_SHADER_BYTECODE {
        D3D12_SHADER_BYTECODE {
            pShaderBytecode: bytes.as_ptr().cast(),
            BytecodeLength: bytes.len(),
        }
    }

    /// A 4x4 texture in GPU memory, in `state`, cleared to `clear`.
    fn texture(
        device: &ID3D12Device,
        format: DXGI_FORMAT,
        flags: D3D12_RESOURCE_FLAGS,
        state: D3D12_RESOURCE_STATES,
        clear: &D3D12_CLEAR_VALUE,
    ) -> ID3D12Resource {
        let heap = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            ..Default::default()
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: 4,
            Height: 4,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Flags: flags,
            ..Default::default()
        };
        let mut out: Option<ID3D12Resource> = None;
        // SAFETY: valid heap and resource descriptions; the out pointer is ours.
        unsafe {
            device
                .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, state, Some(clear), &mut out)
                .unwrap();
        }
        out.unwrap()
    }

    /// A pipeline with `DepthBoundsTestEnable` loads back from the pipeline
    /// library, and a draw at depth 0.5 is culled or drawn as the bounds a
    /// list sets (`draw::depth_bounds`) leave 0.5 out or in. Skipped
    /// without a host device that runs the test, or without `dxc.exe`.
    #[test]
    fn depth_bounds_pipeline_loads_and_filters_draws() {
        let Some(host) = crate::host::host() else {
            eprintln!("no host device; skipped");
            return;
        };
        if !host.depth_bounds() {
            eprintln!("no host depth bounds test; skipped");
            return;
        }
        let Some(dxc) = dxc() else {
            eprintln!("no PC dxc.exe; skipped");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let vs = compile(&dxc, dir.path(), VS, "vs_6_5");
        let ps = compile(&dxc, dir.path(), PS, "ps_6_5");
        let Ok((root, root_blob)) = root_signature(&host.device, &[], &[], false) else {
            eprintln!("no root signature with directly indexed heaps; skipped");
            return;
        };

        let mut g = GraphicsState::default();
        let sub = bounds_only();
        // SAFETY: `sub` is a whole subobject 21.
        unsafe { g.read(21, (&raw const sub).cast()) };
        g.rasterizer.CullMode = D3D12_CULL_MODE_NONE;
        g.targets.NumRenderTargets = 1;
        g.targets.RTFormats[0] = DXGI_FORMAT_R8G8B8A8_UNORM;
        g.depth_format = DXGI_FORMAT_D32_FLOAT;
        let desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            // SAFETY: a borrowed pointer the description never releases.
            pRootSignature: unsafe { std::mem::transmute_copy(&root) },
            VS: code(&vs),
            PS: code(&ps),
            BlendState: g.blend,
            SampleMask: g.sample_mask,
            RasterizerState: g.rasterizer,
            DepthStencilState: g.depth,
            IBStripCutValue: g.strip_cut,
            PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            NumRenderTargets: 1,
            RTVFormats: g.targets.RTFormats,
            DSVFormat: g.depth_format,
            SampleDesc: g.samples,
            ..Default::default()
        };
        let name = super::super::library::name("graphics depth-bounds test", &root_blob, &[&vs, &ps], &g.describe());
        let pso = graphics_depth_bounds_pipeline(&host.device, &desc, g.depth1(), &name).expect("depth-bounds pipeline made");
        let loaded = super::super::library::LOADED.load(Ordering::Relaxed);
        graphics_depth_bounds_pipeline(&host.device, &desc, g.depth1(), &name).expect("depth-bounds pipeline made again");
        assert!(
            super::super::library::LOADED.load(Ordering::Relaxed) > loaded,
            "the depth-bounds pipeline was compiled again instead of loaded from the library"
        );

        let color = texture(
            &host.device,
            DXGI_FORMAT_R8G8B8A8_UNORM,
            D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
            D3D12_RESOURCE_STATE_RENDER_TARGET,
            &D3D12_CLEAR_VALUE {
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                Anonymous: D3D12_CLEAR_VALUE_0 { Color: [0.0; 4] },
            },
        );
        let depth = texture(
            &host.device,
            DXGI_FORMAT_D32_FLOAT,
            D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL,
            D3D12_RESOURCE_STATE_DEPTH_WRITE,
            &D3D12_CLEAR_VALUE {
                Format: DXGI_FORMAT_D32_FLOAT,
                Anonymous: D3D12_CLEAR_VALUE_0 {
                    DepthStencil: D3D12_DEPTH_STENCIL_VALUE { Depth: 0.5, Stencil: 0 },
                },
            },
        );
        let readback = {
            let heap = D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_READBACK,
                ..Default::default()
            };
            let desc = D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
                Width: 1024,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
                ..Default::default()
            };
            let mut out: Option<ID3D12Resource> = None;
            // SAFETY: a valid readback buffer description; the out pointer is ours.
            unsafe {
                host.device
                    .CreateCommittedResource(&heap, D3D12_HEAP_FLAG_NONE, &desc, D3D12_RESOURCE_STATE_COPY_DEST, None, &mut out)
                    .unwrap();
            }
            out.unwrap()
        };

        // The root signature indexes heaps directly: heaps go first, though
        // the shaders read none (one view and one sampler slot each).
        let heap = |kind| -> ID3D12DescriptorHeap {
            // SAFETY: a valid heap description.
            unsafe {
                host.device.CreateDescriptorHeap(&D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: kind,
                    NumDescriptors: 1,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    NodeMask: 0,
                })
            }
            .unwrap()
        };
        let heaps = [
            Some(heap(D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV)),
            Some(heap(D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER)),
        ];

        // Red of pixel (0, 0) after a full-screen draw at depth 0.5 with the
        // bounds `min..max`, the depth target cleared to 0.5.
        let run_draw = |min: f32, max: f32| -> u8 {
            let list = host.new_list().unwrap();
            crate::draw::depth_bounds(&list, min, max);
            let rtv = host.rtv(&color, None);
            let dsv = host.dsv(&depth, None);
            let viewport = D3D12_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: 4.0,
                Height: 4.0,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            let rect = windows::Win32::Foundation::RECT {
                left: 0,
                top: 0,
                right: 4,
                bottom: 4,
            };
            // SAFETY: an open list, live resources and views.
            unsafe {
                list.list.ClearRenderTargetView(rtv, &[0.0; 4], None);
                list.list.ClearDepthStencilView(dsv, D3D12_CLEAR_FLAG_DEPTH, 0.5, 0, None);
                list.list.OMSetRenderTargets(1, Some(&rtv), false, Some(&dsv));
                list.list.RSSetViewports(&[viewport]);
                list.list.RSSetScissorRects(&[rect]);
                list.list.SetDescriptorHeaps(&heaps);
                list.list.SetGraphicsRootSignature(&root);
                list.list.SetPipelineState(&pso);
                list.list
                    .IASetPrimitiveTopology(windows::Win32::Graphics::Direct3D::D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                list.list.DrawInstanced(3, 1, 0, 0);
                list.list.ResourceBarrier(&[crate::host::transition(
                    &color,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                )]);
                let to = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(&readback),
                    Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                        PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                            Offset: 0,
                            Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                                Width: 4,
                                Height: 4,
                                Depth: 1,
                                RowPitch: 256,
                            },
                        },
                    },
                };
                let from = D3D12_TEXTURE_COPY_LOCATION {
                    pResource: std::mem::transmute_copy(&color),
                    Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
                    Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
                };
                list.list.CopyTextureRegion(&to, 0, 0, 0, &from, None);
                list.list.ResourceBarrier(&[crate::host::transition(
                    &color,
                    D3D12_RESOURCE_STATE_COPY_SOURCE,
                    D3D12_RESOURCE_STATE_RENDER_TARGET,
                )]);
            }
            list.close();
            host.execute(&[&list]);
            let mut at = std::ptr::null_mut();
            // SAFETY: `execute` waited for the GPU; the buffer holds 1024
            // bytes and is mapped for the read only.
            unsafe {
                readback.Map(0, None, Some(&mut at)).unwrap();
                let red = *at.cast::<u8>();
                readback.Unmap(0, None);
                red
            }
        };
        assert_eq!(run_draw(0.6, 1.0), 0, "depth 0.5 outside [0.6, 1.0] must be culled");
        assert_eq!(run_draw(0.4, 0.6), 255, "depth 0.5 inside [0.4, 0.6] must be drawn");
    }
}

