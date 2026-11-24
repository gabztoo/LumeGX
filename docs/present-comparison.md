# Compare selected targets with the presented image

In a Windows build with `diagnostics`, set `VICTORY_PRESENT_COMPARE` to
`<width>x<height>-f<DXGI format number>`, for example `2560x1440-f28` for an
RGBA8 UNORM target or `2560x1440-f10` for an RGBA16 float one. For each host
present, the distinct matching targets observed while recording render-target
or UAV bindings since the previous present (the last eight) are selected.
Selection is by the host texture descriptor, not a resource address or shader
identity. It does not prove the resource was written.

Every selected target other than the presented one is probed in the same
command list as the presented image: a 4×4 grid of pixels (mip 0, slice 0) is
read back after the frame's submissions, before `Present`. Each logs a
`present comparison target` line with the title resource (the same address
the draw traces and `VICTORY_DUMP_TARGETS` file names use), its size and
format, `peak` (the largest sampled channel, full scale 1; floats as stored)
and `lit` (samples with a channel above 5/255). Any color format the target
dump reads can be probed; depth and multisampled targets cannot.

This option can change what is shown. The last selected target that can be
copied one-to-one into the back buffer replaces the presented image: the copy
is a D3D12 copy, not a shader, and requires single-sampled 2D textures with
one mip and one layer, the same dimensions and a compatible RGBA8 or BGRA8
format family. No scaling, color conversion, HDR tone mapping or additional
images are provided. If none can be copied (`not_copied`), or none was
observed (`not_observed`), the original source is shown. If the PNG capture is
enabled as well, it captures the effective source.

The `present comparison` line identifies the present attempt that reached
DXGI (`present_id`), `buffer_index`, the selection outcome and the exact
`dxgi_hresult` returned by `Present`. The source and the actual back buffer
are sampled on the GPU after the copy and before `Present`. The line includes
both peak RGB byte values and whether their 4×4 grids matched, ignoring alpha.
An unavailable readback is `None`, not black. Matching samples are not proof
that every pixel matches, and even a successful DXGI result does not prove the
image was displayed. This mode writes no files of its own.

## Finding the scene's texture by producer and consumer

With `VICTORY_LOG=victory_gpu::draw=debug` and the frame listed in
`VICTORY_CAPTURE_FRAMES`, every draw, mesh dispatch and compute dispatch of
that frame is logged (`draw …` lines): render targets for draws, and for each
the views its shaders read through tables, as `<resource> <w>x<h> f<host
format> flags … view f<format the view names> title f<title's format>`
(`uav …` for unordered access, which is how a dispatch writes). The frame's
`present planes` line names the presented resource. The last pass whose
targets or `uav` views include it is the output pass; the textures it reads
are the candidates for the scene, and the passes writing those are traced the
same way. Set `VICTORY_PRESENT_COMPARE` to a candidate's size and format to
probe it against the output in the same present, and add `VICTORY_CAPTURE` and
`VICTORY_DUMP_TARGETS=1` to save that frame's targets and presented image,
named by the same resource addresses. Never pair resources from different
runs: addresses are per process.

The synthetic hardware tests can be run without a title:

```text
cargo test -p victory-gpu --features diagnostics present_compare -- --ignored --test-threads=1
```

One uses two same-format, same-size GPU textures (bright, then black) and an
initially bright stand-in for the back buffer; it exercises the same selector,
copy and readback path as the presenter without opening a window. The other
probes an RGBA16 float target of another size than the output in the same
list and checks its values are read at full scale 1.
