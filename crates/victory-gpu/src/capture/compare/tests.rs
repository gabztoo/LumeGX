use super::*;
use crate::host;

#[test]
fn present_compare_requires_a_supported_color_spec() {
    assert_eq!(
        parse_spec(" 8x4-f28 "),
        Some((8, 4, DXGI_FORMAT_R8G8B8A8_UNORM))
    );
    // Formats the probe reads are compared too (not copied): HDR targets.
    assert_eq!(
        parse_spec("2560x1440-f10"),
        Some((2560, 1440, DXGI_FORMAT_R16G16B16A16_FLOAT))
    );
    for spec in ["8x4", "0x4-f28", "8x0-f28", "8x4-f0", "8x4-f71"] {
        assert!(parse_spec(spec).is_none(), "{spec}");
    }
}

fn texture(host: &host::Host, width: u64, state: D3D12_RESOURCE_STATES) -> Arc<HostTexture> {
    texture_in(host, width, DXGI_FORMAT_R8G8B8A8_UNORM, state)
}

fn texture_in(
    host: &host::Host,
    width: u64,
    format: DXGI_FORMAT,
    state: D3D12_RESOURCE_STATES,
) -> Arc<HostTexture> {
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
        Format: format,
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
    Arc::new(HostTexture {
        texture: texture.unwrap(),
        state: (state.0 as u32).into(),
    })
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
    selection.note(1, bright.clone());
    selection.note(2, black.clone());
    selection.note(3, other.clone());
    let chosen = selection.take();
    assert_eq!(chosen.iter().map(|(r, _)| *r).collect::<Vec<_>>(), [1, 2]);
    assert!(
        selection.take().is_empty(),
        "do not reuse the previous present's selection"
    );
    let (source, status) = source(&chosen, &bright.texture, &back.texture);
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

/// The scene target of another size and format than the output (an HDR
/// 2560x1440 target against a 4K RGBA8 output, here 4x4 against 8x4) is
/// not copied but probed in the same list as the output, and read at full
/// scale 1.
#[test]
#[ignore = "requires a hardware D3D12 device"]
fn present_compare_probes_a_scene_target_of_another_size() {
    let host = host::host().expect("D3D12 device for the present comparison test");
    let rt = D3D12_RESOURCE_STATE_RENDER_TARGET;
    let src = D3D12_RESOURCE_STATE_COPY_SOURCE;
    let float = DXGI_FORMAT_R16G16B16A16_FLOAT;
    let scene = texture_in(host, 4, float, rt);
    let unlit = texture_in(host, 4, float, rt);
    let output = texture(host, 8, rt);
    let selection = Selection::new((4, 4, float));
    selection.note(7, scene.clone());
    selection.note(8, unlit.clone());
    selection.note(9, output.clone());
    let chosen = selection.take();
    assert_eq!(chosen.len(), 2, "only targets of the given size and format");
    let (shown, status) = source(&chosen, &output.texture, &output.texture);
    assert_eq!(status, "not_copied");
    assert_eq!(shown, &output.texture);

    let list = host.new_list().expect("D3D12 command list");
    // SAFETY: live resources, RTVs in their own formats, and a transition to
    // the copy state before each probe.
    let probes: Vec<_> = unsafe {
        for (target, color) in [
            (&scene, [2.0, 0.25, 0.0, 1.0]),
            (&unlit, [0.0, 0.0, 0.0, 1.0]),
            (&output, [0.0, 0.0, 0.0, 1.0]),
        ] {
            list.list
                .ClearRenderTargetView(host.rtv(&target.texture, None), &color, None);
        }
        let targets = [&scene, &unlit, &output];
        let barriers: Vec<_> = targets
            .iter()
            .map(|t| host::transition(&t.texture, rt, src))
            .collect();
        list.list.ResourceBarrier(&barriers);
        targets
            .iter()
            .map(|t| super::super::probe_any(&host.device, &list.list, &t.texture))
            .collect()
    };
    list.close();
    host.execute(&[&list]);
    let values = |i: usize, format: DXGI_FORMAT| {
        super::super::probe_values(probes[i].as_ref().expect("probe"), format.0 as u32)
            .expect("values")
    };
    assert!(values(0, float).iter().all(|&p| p == [2.0, 0.25, 0.0]), "HDR values kept");
    assert!(values(1, float).iter().all(|&p| p == [0.0; 3]));
    assert!(values(2, DXGI_FORMAT_R8G8B8A8_UNORM).iter().all(|&p| p == [0.0; 3]));
    // The report reads the same buffers (logged; it must not panic).
    for (i, target) in chosen.iter().enumerate() {
        report_target(1, i, target, probes[i].as_ref());
    }
}
