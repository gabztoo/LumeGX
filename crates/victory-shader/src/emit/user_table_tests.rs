//! End-to-end test of a compute root signature past the 16 user SGPRs:
//! the synthetic program of `resource::tests` (root parameters loaded from
//! the user-data table, a root UAV built around one) lowered to DXIL,
//! validated by `dxil.dll` when present, and run on the host GPU and WARP.

use super::host::{Dispatch, Host, Root};
use super::{HostShader, lower, validator};
use crate::native::{
    ComputeIo, HwStage, NativeProgram, ResourceType, RootParam, RootType, StageIo, UsedResource,
};
use crate::{io, ir, resource, structure};

/// The root constant the test passes (user-data dword 16).
const BASE: u32 = 1000;

/// Halves written and read (wave32, 64 threads, written for this test):
/// thread i writes 0x8000 + i to LDS and to the root UAV at byte 2i (two
/// threads per dword, at once), then reads its half back zero- and
/// sign-extended from both and stores the four results as dwords from
/// byte 128 on.
const HALVES_WAVE32: &[u32] = &[
    0xBE82_0300, // s_mov_b32 s2, s0
    0x8703_FF01,
    0x0000_FFFF, // s_and_b32 s3, s1, 0xffff
    0xF404_0101,
    0xFA00_0008, // s_load_dwordx2 s[4:5], s[2:3], 0x8 (user data 18, 19)
    0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
    0xBE8C_0304, // s_mov_b32 s12, s4
    0x880D_FF05,
    0x0001_0000, // s_or_b32 s13, s5, 0x10000
    0xBE8E_03C1, // s_mov_b32 s14, -1
    0xBE8F_03FF,
    0x1101_4FAC, // s_mov_b32 s15, 0x11014fac
    0x3404_0081, // v_lshlrev_b32 v2, 1, v0
    0x340A_0082, // v_lshlrev_b32 v5, 2, v0
    0x4A02_00FF,
    0x0000_8000, // v_add_nc_u32 v1, 0x8000, v0
    0xD87C_0000,
    0x0000_0102, // ds_write_b16 v2, v1
    0xBF8C_C07F, // s_waitcnt lgkmcnt(0)
    0xD8F0_0000,
    0x0300_0002, // ds_read_u16 v3, v2
    0xD8EC_0000,
    0x0400_0002, // ds_read_i16 v4, v2
    0xE068_1000,
    0x8003_0102, // buffer_store_short v1, v2, s[12:15], 0 offen
    0xE028_1000,
    0x8003_0602, // buffer_load_ushort v6, v2, s[12:15], 0 offen
    0xE02C_1000,
    0x8003_0702, // buffer_load_sshort v7, v2, s[12:15], 0 offen
    0xBF8C_3F70, // s_waitcnt vmcnt(0) lgkmcnt(0)
    0xE070_1080,
    0x8003_0305, // buffer_store_dword v3, v5, s[12:15], 0 offen offset:128
    0xE070_1180,
    0x8003_0405, // buffer_store_dword v4, v5, s[12:15], 0 offen offset:384
    0xE070_1280,
    0x8003_0605, // buffer_store_dword v6, v5, s[12:15], 0 offen offset:640
    0xE070_1380,
    0x8003_0705, // buffer_store_dword v7, v5, s[12:15], 0 offen offset:896
    0xBF81_0000, // s_endpgm
];

fn build() -> HostShader {
    build_from(resource::tests::USER_TABLE_WAVE32, 0, 32)
}

/// Lowers `words` with the user-data table's layout, `lds_blocks` of 512
/// bytes of LDS and groups of `threads` (more than a wave: no wave size).
fn build_from(words: &[u32], lds_blocks: u32, threads: u32) -> HostShader {
    let param = |root, user_data, sgprs, slot| RootParam {
        root,
        table_offset_dwords: 0,
        sgprs,
        base_register: 0,
        user_data,
        slot,
    };
    let native = NativeProgram {
        stage: HwStage::Cs,
        wave64: false,
        vgprs: 8,
        layout: vec![
            param(RootType::Constants, 16, 1, 0),
            param(RootType::Descriptor, 18, 2, 1),
        ],
        used: vec![UsedResource {
            kind: ResourceType::Uav,
            root: RootType::Descriptor,
            table_offset_dwords: 0,
            lower_bound: 0,
            stride: 1,
            dwords: 4,
            user_data: 18,
            slot: 1,
        }],
        // Two user SGPRs: the table's address.
        io: StageIo::Cs(ComputeIo {
            rsrc2: 2 << 1 | lds_blocks << 15,
            threads: [threads, 1, 1],
        }),
        code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
    };
    let p = ir::build(&native.code, native.wave64, native.vgprs).expect("ir::build");
    let st = structure::structure(&p).expect("structure");
    let res = resource::track(&p, &native.layout, native.stage, Some(&st)).expect("track");
    let io = io::raise(&p, &native).expect("io::raise");
    let lowered = lower::lower(&lower::Parts {
        native: &native,
        p: &p,
        st: &st,
        io: &io,
        res: &res,
        inputs: Vec::new(),
        outputs: Vec::new(),
        prims: Vec::new(),
        mesh_stride: 0,
    })
    .expect("lower");
    assert_eq!(lowered.root_uavs, [1], "the title's root parameter 1");
    let shader = HostShader {
        stage: HwStage::Cs,
        dxil: lowered.dxil,
        user_data: lowered.user_data,
        samplers: lowered.samplers,
        static_samplers: Vec::new(),
        root_cbvs: lowered.root_cbvs,
        root_srvs: lowered.root_srvs,
        root_uavs: lowered.root_uavs,
        heap_usage: lowered.heap_usage,
    };
    if let Ok(mut v) = validator::Validator::load(validator::Validator::default_path().as_deref()) {
        v.validate(&mut shader.dxil.clone())
            .expect("DXIL validation failed");
    } else {
        eprintln!("no dxil.dll; validation skipped");
    }
    shader
}

#[test]
fn root_parameters_from_the_user_data_table_reach_the_host() {
    let shader = build();
    // The root constant is user data, read from the host's root constants.
    let constant = shader
        .user_data
        .iter()
        .find(|u| u.index == 16)
        .expect("user-data dword 16");
    assert_eq!((constant.slot, constant.root), (0, RootType::Constants));
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let (root, pso) = host
            .compute_pipeline(&shader)
            .unwrap_or_else(|e| panic!("{label}: compute_pipeline failed: {e}"));
        let mut ud = vec![0u32; shader.user_data_dwords() as usize];
        ud[16] = BASE;
        let out = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud)), (1, Root::Uav(32))],
                    views: &[],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        let want: Vec<u32> = (0..32).map(|lane| BASE + lane).collect();
        assert_eq!(out[0], want, "{label}");
    }
}

/// Byte and half accesses change only their bits of a dword other lanes
/// write at the same time, and read back zero- or sign-extended.
#[test]
fn halves_written_by_neighbouring_lanes_keep_each_other() {
    let shader = build_from(HALVES_WAVE32, 1, 64);
    let _one = super::valu_tests::ONE_AT_A_TIME
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for (label, warp) in [("hardware", false), ("warp", true)] {
        let Ok(host) = Host::new(warp) else {
            eprintln!("no SM 6.6 {label} device; skipped");
            continue;
        };
        let (root, pso) = host
            .compute_pipeline(&shader)
            .unwrap_or_else(|e| panic!("{label}: compute_pipeline failed: {e}"));
        let ud = vec![0u32; shader.user_data_dwords().max(1) as usize];
        let out = host
            .dispatch(
                &root,
                &pso,
                &Dispatch {
                    roots: vec![(0, Root::Constants(ud)), (1, Root::Uav(288))],
                    views: &[],
                    samplers: &[],
                    groups: [1, 1, 1],
                },
            )
            .unwrap_or_else(|e| panic!("{label}: dispatch failed: {e}"));
        let w = &out[0];
        let half = |l: u32| 0x8000 + l;
        let pairs: Vec<u32> = (0..32).map(|k| half(2 * k) | half(2 * k + 1) << 16).collect();
        let zero: Vec<u32> = (0..64).map(half).collect();
        let signed: Vec<u32> = (0..64).map(|l| half(l) | 0xFFFF_0000).collect();
        assert_eq!(w[..32], pairs[..], "{label}: stored halves");
        assert_eq!(w[32..96], zero[..], "{label}: ds_read_u16");
        assert_eq!(w[96..160], signed[..], "{label}: ds_read_i16");
        assert_eq!(w[160..224], zero[..], "{label}: buffer_load_ushort");
        assert_eq!(w[224..288], signed[..], "{label}: buffer_load_sshort");
    }
}
