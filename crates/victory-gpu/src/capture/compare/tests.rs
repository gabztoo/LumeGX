use super::*;
use crate::host;

#[test]
fn present_compare_requires_a_supported_color_spec() {
    assert_eq!(
        parse_spec(" 8x4-f28 "),
        Some((8, 4, DXGI_FORMAT_R8G8B8A8_UNORM))
    );
    for spec in ["8x4", "0x4-f28", "8x0-f28", "8x4-f0", "8x4-f10"] {
        assert!(parse_spec(spec).is_none(), "{spec}");
    }
}

fn texture(host: &host::Host, width: u64, state: D3D12_RESOURCE_STATES) -> Arc<HostTexture> {
    let heap = D3D12_HEAP_PROPERTIES {
        Type: D3D12_HEAP_TYPE_DEFAULT,
        ..Default::default()
    };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Width: width,
        Height: 4,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
        ..Default::default()
    };
    let mut texture = None;
    // SAFETY: plain resource creation with valid descriptions.
    unsafe {
        host.device
            .CreateCommittedResource(
                &heap,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                state,
                None,
                &mut texture,
            )
            .expect("create synthetic texture");
    }
    Arc::new(HostTexture::new(texture.unwrap(), state.0 as u32, Vec::new()))
}

#[test]
#[ignore = "requires a hardware D3D12 device"]
fn present_compare_copies_the_last_matching_black_target() {
    let host = host::host().expect("D3D12 device for the present comparison test");
    let rt = D3D12_RESOURCE_STATE_RENDER_TARGET;
    let src = D3D12_RESOURCE_STATE_COPY_SOURCE;
    let dest = D3D12_RESOURCE_STATE_COPY_DEST;
    let bright = texture(host, 8, rt);
    let black = texture(host, 8, rt);
    let other = texture(host, 4, rt);
    let back = texture(host, 8, rt);
    let selection = Selection::new((8, 4, DXGI_FORMAT_R8G8B8A8_UNORM));
    selection.note(bright.clone());
    selection.note(black.clone());
    selection.note(other.clone());
    let choice = selection.take().expect("one matching target selected");
    assert!(Arc::ptr_eq(&choice, &black));
    assert!(
        selection.take().is_none(),
        "do not reuse the previous present's selection"
    );
    let (source, status) = source(Some(&choice), &bright.texture, &back.texture);
    assert_eq!(status, "copied");
    assert_eq!(source, &black.texture);

    let list = host.new_list().expect("D3D12 command list");
    // SAFETY: all resources are live, the RTVs use their own formats, and
    // each copy below has a preceding transition to its required state.
    let (first, selected, after) = unsafe {
        for (target, color) in [
            (&bright, [1.0, 0.25, 0.0, 1.0]),
            (&black, [0.0, 0.0, 0.0, 1.0]),
            (&back, [1.0, 0.25, 0.0, 1.0]),
        ] {
            list.list
                .ClearRenderTargetView(host.rtv(&target.texture, None), &color, None);
        }
        list.list.ResourceBarrier(&[
            host::transition(&bright.texture, rt, src),
            host::transition(source, rt, src),
            host::transition(&back.texture, rt, dest),
        ]);
        (
            super::super::probe(&host.device, &list.list, &bright.texture).expect("first probe"),
            super::super::probe(&host.device, &list.list, source).expect("selected probe"),
            copy_to_back(&host.device, &list.list, source, &back.texture, true)
                .expect("back probe"),
        )
    };
    list.close();
    host.execute(&[&list]);
    assert_eq!(super::super::probe_level(&first), Some(255));
    let selected = super::super::probe_colors(&selected).expect("selected colors");
    let after = super::super::probe_colors(&after).expect("back colors");
    assert!(selected.iter().all(|&p| p == [0, 0, 0]));
    assert_eq!(
        after, selected,
        "the back buffer must be black after the blit, not before"
    );
}
