# Compare the selected target with the presented image

In a Windows build with `diagnostics`, set `VICTORY_PRESENT_COMPARE` to
`<width>x<height>-f<DXGI format number>`, for example `1280x720-f28` for an
RGBA8 UNORM target. For each host present, the last matching target observed
while recording render-target or UAV bindings is selected. Selection is by the
host texture descriptor, not a resource address or shader identity. It does
not prove the resource was written. The original source is used if there is no
match or the selected target cannot be copied to the back buffer.

This option changes what is shown. The blit is a one-to-one D3D12 copy, not a
shader: it requires single-sampled 2D textures with one mip and one layer, the
same dimensions and a compatible RGBA8 or BGRA8 format family. No scaling,
color conversion, HDR tone mapping or additional images are provided. If the
existing PNG capture is enabled as well, it captures the effective source.

The `present comparison` log entry identifies the present attempt that reached
DXGI (`present_id`), `buffer_index`, the selection outcome and the exact
`dxgi_hresult` returned by `Present`. The source and the actual back buffer
are sampled on the GPU after the copy and before `Present`. The line includes
both peak RGB byte values and whether their 4×4 grids matched, ignoring alpha.
An unavailable readback is `None`, not black. Matching samples are not proof
that every pixel matches, and even a successful DXGI result does not prove the
image was displayed. This mode writes no files of its own.

The synthetic hardware test can be run without a title:

```text
cargo test -p victory-gpu --features diagnostics present_compare_copies_the_last_matching_black_target -- --ignored --test-threads=1
```

It uses two same-format, same-size GPU textures (bright, then black) and an
initially bright stand-in for the back buffer; it exercises the same selector,
copy and readback path as the presenter without opening a window.
