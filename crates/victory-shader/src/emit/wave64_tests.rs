//! A console wave64 compute program on a host of 32-lane waves (ported
//! from fxceless/victory-fork, `wave64_tests.rs`, 578d459 and e9765a7):
//! when its cross-lane code stays inside 32-lane halves it is lowered for
//! waves of 32, each host wave taking one half of a console wave; when it
//! crosses halves it keeps `WaveSize(64)`. Validated by `dxil.dll` when
//! present and run on each SM 6.6 device that takes it (a test that needs
//! a card skips without one).

use super::host::{Dispatch, Host, Root, View};
use super::{HostShader, lower, validator};
use crate::dxbc::{Container, FourCC};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{io, ir, resource, structure};

const S_ENDPGM: u32 = 0xBF81_0000;

/// `v_mov_b32_dpp v1, v0 row_shr:1 bound_ctrl:0`: each lane takes the
/// thread index of the lane below it in its row of 16, the row's first
/// lane 0.
const ROW_SHR_V0_TO_V1: [u32; 2] = [0x7E02_02FA, 0xFF09_1100];

/// `v_lshlrev_b32 v2, 2, v0` and `buffer_store_dword v1, v2, s[8:11], 0
/// offen`: v1 to dword `thread index` of the raw UAV.
const STORE_V1_AT_THREAD: [u32; 3] = [
    0x3404_0082,
    0x38 << 26 | 0x1C << 18 | 1 << 12,
    0x80 << 24 | 2 << 16 | 1 << 8 | 2,
];

/// `v_mbcnt_lo_u32_b32 v1, -1, 0`: the lanes below this one, which a host
/// wave of 32 cannot count for console lanes 32..63.
const MBCNT_LO_TO_V1: [u32; 2] = [0x365 << 16 | 0x35 << 26 | 1, 193 | 128 << 9];

/// A wave64 compute program of `threads` threads (the thread index in v0)
/// with 4 root constants in s0..s3 and a raw UAV's descriptor in s[8:11],
/// then `body`.
fn native(body: &[u32], threads: u32) -> NativeProgram {
    let mut words = vec![
        0xBE85_0380, // s_mov_b32 s5, 0
        0x3D << 26 | 0x02 << 18 | 8 << 6 | 2,
        125 << 25, // s_load_dwordx4 s[8:11], s[4:5], 0
    ];
    words.extend_from_slice(body);
    words.push(S_ENDPGM);
    NativeProgram {
        stage: HwStage::Cs,
        wave64: true,
        vgprs: 16,
        layout: vec![
            RootParam {
                root: RootType::Constants,
                table_offset_dwords: 0,
                sgprs: 4,
                base_register: 0,
                user_data: 0,
                slot: 0,
            },
            RootParam {
                root: RootType::Table,
                table_offset_dwords: 0,
                sgprs: 1,
                base_register: 0,
                user_data: 4,
                slot: 1,
            },
        ],
        used: vec![UsedResource {
            kind: ResourceType::Uav,
            root: RootType::Table,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 0,
            dwords: 4,
            user_data: 4,
            slot: 1,
        }],
        io: StageIo::Cs(ComputeIo {
            rsrc2: 5 << 1, // 5 user SGPRs: s0..s4
            threads: [threads, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    }
}

/// `native` lowered for a host whose widest wave is `host_wave` lanes
/// (0: not known), and validated when `dxil.dll` is there.
fn lowered(native: &NativeProgram, host_wave: u32) -> HostShader {
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("resource");
    let io = io::raise(&p, native).expect("io::raise");
    let was = lower::TEST_HOST_WAVE.replace(Some(host_wave));
    let lowered = lower::lower(&lower::Parts {
        native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
    });
    lower::TEST_HOST_WAVE.set(was);
    let lowered = lowered.expect("lower");
    let shader = HostShader {
        stage: HwStage::Cs,
        dxil: lowered.dxil,
        user_data: lowered.user_data,
        samplers: lowered.samplers,
        static_samplers: Vec::new(),
        root_cbvs: lowered.root_cbvs,
        root_srvs: lowered.root_srvs,
        root_uavs: lowered.root_uavs,
    };
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        let mut copy = shader.dxil.clone();
        v.validate(&mut copy).expect("DXIL validation failed");
    }
    shader
}

/// The wave size the program declares (`WaveSize`, from its pipeline state
/// validation part: minimum wave lanes at byte 20), 0 for none.
fn declared_wave(shader: &HostShader) -> u32 {
    let c = Container::parse(&shader.dxil).expect("container");
    let info = c.part(FourCC::PSV0).expect("PSV0").data;
    u32::from_le_bytes(info[20..24].try_into().expect("PSV0 size"))
}

#[test]
fn a_row_shift_in_wave64_runs_in_host_waves_of_32() {
    let program = native(&[&ROW_SHR_V0_TO_V1[..], &STORE_V1_AT_THREAD].concat(), 128);
    assert_eq!(
        declared_wave(&lowered(&program, 0)),
        64,
        "host wave unknown"
    );
    assert_eq!(declared_wave(&lowered(&program, 64)), 64, "a 64-lane host");
    let shader = lowered(&program, 32);
    assert_eq!(declared_wave(&shader), 32, "a 32-lane host");
    // Each thread gets the index of the thread below it in its row of 16:
    // the console's lanes, which the host waves of 32 take half by half.
    let expected: Vec<u32> = (0..128)
        .map(|t| if t % 16 == 0 { 0 } else { t - 1 })
        .collect();
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut ran = 0;
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let Ok((root, pso)) = host.compute_pipeline(&shader) else {
            eprintln!("the {label} device does not take waves of 32; skipped");
            continue;
        };
        let ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
        let out = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud))],
                    views: &[View::RawUav(128)],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        assert_eq!(out[0], expected, "{label}");
        ran += 1;
    }
    eprintln!("ran on {ran} device(s)");
}

#[test]
fn a_lane_count_across_halves_keeps_wave64() {
    let program = native(&[&MBCNT_LO_TO_V1[..], &STORE_V1_AT_THREAD].concat(), 64);
    assert_eq!(declared_wave(&lowered(&program, 32)), 64);
}

#[test]
fn lds_in_a_group_over_32_keeps_wave64() {
    // `ds_write_b32 v2, v1` (LDS a group of 64 may share within its one
    // console wave without barriers).
    let ds_write = [0xD834_0000, 0x0000_0102];
    let mut program = native(
        &[&ROW_SHR_V0_TO_V1[..], &ds_write, &STORE_V1_AT_THREAD].concat(),
        64,
    );
    if let StageIo::Cs(cs) = &mut program.io {
        cs.rsrc2 |= 1 << 15; // 512 bytes of LDS (LDS_SIZE, in 512-byte units)
    }
    let was = lower::TEST_SPLIT_LDS.replace(Some(false));
    let wave = declared_wave(&lowered(&program, 32));
    lower::TEST_SPLIT_LDS.set(was);
    assert_eq!(wave, 64);
}

/// 64 threads in one console wave with LDS: each writes its index to
/// `lds[t]` and reads `lds[t ^ 32]`, the other half's, without a barrier
/// (the console runs the wave in step). With `VICTORY_WAVE_SPLIT_LDS` the
/// two host waves meet before each LDS access and get the console's
/// result; without it the program keeps `WaveSize(64)`.
#[test]
fn lds_shared_across_halves_is_ordered_by_meetings_when_split() {
    let body = [
        0x3404_0082, // v_lshlrev_b32 v2, 2, v0
        0x3A06_00A0, // v_xor_b32 v3, 32, v0
        0x3406_0682, // v_lshlrev_b32 v3, 2, v3
        0xD834_0000,
        0x0000_0002, // ds_write_b32 v2, v0
        0xD8D8_0000,
        0x0100_0003, // ds_read_b32 v1, v3
        0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
    ];
    let mut program = native(&[&body[..], &STORE_V1_AT_THREAD].concat(), 64);
    if let StageIo::Cs(cs) = &mut program.io {
        cs.rsrc2 |= 1 << 15; // 512 bytes of LDS
    }
    let with = |on: bool| {
        let was = lower::TEST_SPLIT_LDS.replace(Some(on));
        let shader = lowered(&program, 32);
        lower::TEST_SPLIT_LDS.set(was);
        shader
    };
    assert_eq!(declared_wave(&with(false)), 64, "split off");
    let shader = with(true);
    assert_eq!(declared_wave(&shader), 32, "split on");
    let expected: Vec<u32> = (0..64).map(|t| t ^ 32).collect();
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let Ok((root, pso)) = host.compute_pipeline(&shader) else {
            eprintln!("the {label} device does not take waves of 32; skipped");
            continue;
        };
        let ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
        let out = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud))],
                    views: &[View::RawUav(64)],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        assert_eq!(out[0], expected, "{label}");
        eprintln!("ran on the {label} device");
    }
}
