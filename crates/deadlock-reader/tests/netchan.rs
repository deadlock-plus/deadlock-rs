//! The engine net channel reads, driven over synthetic `engine2.dll` and
//! `networksystem.dll` images.

use std::sync::Arc;

use deadlock_memory::mem::MemoryReader;
use deadlock_memory::mock::MockMemory;
use deadlock_reader::netchan::{NetChanAnchors, NetChanError, NetChanOffsets, NetChanStats};

const ENGINE_BASE: u64 = 0x2_0000_0000;
const ENGINE_SIZE: usize = 0x4000;
const NET_BASE: u64 = 0x3_0000_0000;
const NET_SIZE: usize = 0x2000;

const G_SIG: &str =
    "4C 8B 05 ?? ?? ?? ?? 4D 85 C0 74 10 48 63 CA 48 8D 04 49 49 8B 84 C0 F0 00 00 00 C3 33 C0 C3";
const PING_GETTER: &str = "40 57 48 83 EC 30 8B 91 C8 8E 00 00 48 8B F9 85 D2 0F 84";
const FLOW_GETTER: &str = "83 FA 01 B8 00 00 00 00 41 B8 00 00 00 00 41 0F 45 C0 F3 0F 10 04 08 C3";

/// Offsets deliberately different from the shipping build, so a hard-coded constant fails.
const PING: u32 = 0x9104;
const LOSS_DOWN: u32 = 0x58;
const LOSS_UP: u32 = 0x7268;
const JITTER_IN: u32 = 0x70;
const JITTER_OUT: u32 = 0x7290;

const G_VAR: u64 = ENGINE_BASE + 0x3000;
const GLOBAL_OBJ: u64 = 0x5_0000_0000;
const CHAN: u64 = 0x6_0000_0000;

fn bytes(pattern: &str) -> Vec<u8> {
    pattern
        .split_whitespace()
        .map(|t| {
            if t == "??" {
                0
            } else {
                u8::from_str_radix(t, 16).unwrap()
            }
        })
        .collect()
}

fn engine_image(g_target: u64) -> Vec<u8> {
    let mut img = vec![0xCCu8; ENGINE_SIZE];
    let at = 0x200usize;
    let mut sig = bytes(G_SIG);
    let disp = (g_target as i64 - (ENGINE_BASE as i64 + at as i64 + 7)) as i32;
    sig[3..7].copy_from_slice(&disp.to_le_bytes());
    img[at..at + sig.len()].copy_from_slice(&sig);
    img
}

fn net_image(ping: u32, loss: (u32, u32), jitter: (u32, u32)) -> Vec<u8> {
    net_image_with(ping, &[loss, (0x5C, 0x727C), jitter])
}

fn net_image_with(ping: u32, flow: &[(u32, u32)]) -> Vec<u8> {
    let mut img = vec![0xCCu8; NET_SIZE];

    let mut p = bytes(PING_GETTER);
    // The `jz rel32` operand and some filler, then `mov eax,[rdi+disp32]; test eax,eax; js`.
    p.extend_from_slice(&[0x9C, 0x00, 0x00, 0x00, 0x90, 0x90, 0x90, 0x90]);
    p.extend_from_slice(&[0x8B, 0x87]);
    p.extend_from_slice(&ping.to_le_bytes());
    p.extend_from_slice(&[0x85, 0xC0, 0x78, 0x32]);
    img[0x100..0x100 + p.len()].copy_from_slice(&p);

    for (n, (down, up)) in flow.iter().enumerate() {
        let at = 0x300 + n * 0x20;
        let mut g = bytes(FLOW_GETTER);
        g[4..8].copy_from_slice(&down.to_le_bytes());
        g[10..14].copy_from_slice(&up.to_le_bytes());
        img[at..at + g.len()].copy_from_slice(&g);
    }
    img
}

fn memory(engine: &[u8], net: &[u8]) -> MockMemory {
    let mut m = MockMemory::new(77);
    m.add_module("engine2.dll", ENGINE_BASE, ENGINE_SIZE);
    m.add_module("networksystem.dll", NET_BASE, NET_SIZE);
    m.write(ENGINE_BASE, engine);
    m.write(NET_BASE, net);
    m
}

fn standard() -> MockMemory {
    memory(
        &engine_image(G_VAR),
        &net_image(PING, (LOSS_DOWN, LOSS_UP), (JITTER_IN, JITTER_OUT)),
    )
}

fn put_chan(m: &mut MockMemory, ping: i32, loss_down: f32, loss_up: f32, jin: f32, jout: f32) {
    let mut chan = vec![0u8; 0x9200];
    let mut set = |off: u32, b: [u8; 4]| chan[off as usize..off as usize + 4].copy_from_slice(&b);
    set(PING, ping.to_le_bytes());
    set(LOSS_DOWN, loss_down.to_le_bytes());
    set(LOSS_UP, loss_up.to_le_bytes());
    set(JITTER_IN, jin.to_le_bytes());
    set(JITTER_OUT, jout.to_le_bytes());
    m.write(CHAN, &chan);
    let mut obj = vec![0u8; 0x100];
    obj[0xF0..0xF8].copy_from_slice(&CHAN.to_le_bytes());
    m.write(GLOBAL_OBJ, &obj);
    m.write_u64(G_VAR, GLOBAL_OBJ);
}

#[test]
fn the_global_signature_resolves_its_disp32() {
    let m = standard();
    let a = NetChanAnchors::resolve(&m).unwrap();
    assert_eq!(a.global(), G_VAR);
}

#[test]
fn field_offsets_come_from_the_getters() {
    let m = standard();
    let a = NetChanAnchors::resolve(&m).unwrap();
    assert_eq!(
        a.offsets(),
        NetChanOffsets {
            ping: PING,
            loss_down: LOSS_DOWN,
            loss_up: LOSS_UP,
            jitter_in: JITTER_IN,
            jitter_out: JITTER_OUT,
        }
    );
}

#[test]
fn reads_the_stats_at_the_derived_offsets() {
    let mut m = standard();
    put_chan(&mut m, 23, 0.18, 0.09, 3.5, 2.25);
    let a = NetChanAnchors::resolve(&m).unwrap();
    assert_eq!(
        a.read(&m).unwrap(),
        NetChanStats {
            ping_ms: 23,
            loss_down: 0.18,
            loss_up: 0.09,
            jitter_in_ms: 3.5,
            jitter_out_ms: 2.25,
        }
    );
}

#[test]
fn a_null_global_or_channel_is_unavailable() {
    let mut m = standard();
    let a = NetChanAnchors::resolve(&m).unwrap();

    m.write_u64(G_VAR, 0);
    assert_eq!(a.read(&m), Err(NetChanError::Unavailable));

    m.write_u64(G_VAR, GLOBAL_OBJ);
    m.write(GLOBAL_OBJ, &[0u8; 0x100]);
    assert_eq!(a.read(&m), Err(NetChanError::Unavailable));
}

/// Ping, loss down, loss up, jitter in, jitter out.
type Chan = (i32, f32, f32, f32, f32);

#[test]
fn out_of_range_values_are_rejected() {
    let cases: [(&str, Chan); 8] = [
        ("ping", (-1, 0.0, 0.0, 0.0, 0.0)),
        ("ping", (60_000, 0.0, 0.0, 0.0, 0.0)),
        ("loss_down", (20, 1.5, 0.0, 0.0, 0.0)),
        ("loss_down", (20, f32::NAN, 0.0, 0.0, 0.0)),
        ("loss_up", (20, 0.0, -0.1, 0.0, 0.0)),
        ("jitter_in", (20, 0.0, 0.0, -2.0, 0.0)),
        ("jitter_out", (20, 0.0, 0.0, 0.0, f32::INFINITY)),
        ("jitter_out", (20, 0.0, 0.0, 0.0, 1.0e9)),
    ];
    for (field, (p, ld, lu, ji, jo)) in cases {
        let mut m = standard();
        put_chan(&mut m, p, ld, lu, ji, jo);
        let a = NetChanAnchors::resolve(&m).unwrap();
        match a.read(&m) {
            Err(NetChanError::Implausible { field: f, .. }) => assert_eq!(f, field),
            other => panic!("{field}: expected Implausible, got {other:?}"),
        }
    }
}

#[test]
fn boundary_values_are_accepted() {
    let mut m = standard();
    put_chan(&mut m, 0, 0.0, 1.0, 0.0, 0.0);
    let a = NetChanAnchors::resolve(&m).unwrap();
    assert!(a.read(&m).is_ok());
}

#[test]
fn a_missing_signature_is_unresolved() {
    let m = memory(&vec![0xCCu8; ENGINE_SIZE], &net_image(PING, (1, 2), (3, 4)));
    assert!(matches!(
        NetChanAnchors::resolve(&m),
        Err(NetChanError::Unresolved(_))
    ));

    let m = memory(&engine_image(G_VAR), &vec![0xCCu8; NET_SIZE]);
    assert!(matches!(
        NetChanAnchors::resolve(&m),
        Err(NetChanError::Unresolved(_))
    ));
}

#[test]
fn the_flow_getters_must_be_exactly_three() {
    for flow in [&[(1, 2), (3, 4)][..], &[(1, 2), (3, 4), (5, 6), (7, 8)][..]] {
        let m = memory(&engine_image(G_VAR), &net_image_with(PING, flow));
        assert!(matches!(
            NetChanAnchors::resolve(&m),
            Err(NetChanError::Unresolved(_))
        ));
    }
}

#[test]
fn an_impossible_offset_is_unresolved() {
    let m = memory(
        &engine_image(G_VAR),
        &net_image(PING, (0x20_0000, LOSS_UP), (JITTER_IN, JITTER_OUT)),
    );
    assert!(matches!(
        NetChanAnchors::resolve(&m),
        Err(NetChanError::Unresolved(_))
    ));
}

#[test]
fn a_missing_module_is_unresolved() {
    let m = MockMemory::new(1);
    assert!(matches!(
        NetChanAnchors::resolve(&m),
        Err(NetChanError::Unresolved(_))
    ));
}

#[test]
fn an_unreadable_channel_is_a_memory_error() {
    let mut m = standard();
    m.write_u64(G_VAR, 0x7_0000_0000);
    let a = NetChanAnchors::resolve(&m).unwrap();
    assert!(matches!(a.read(&m), Err(NetChanError::Memory(_))));
}

#[test]
fn the_reader_exposes_the_stats() {
    use deadlock_reader::Reader;

    let mut m = standard();
    put_chan(&mut m, 31, 0.0, 0.0, 1.0, 1.0);
    add_client(&mut m, 0x1_0000_0000);
    let mem: Arc<dyn MemoryReader> = Arc::new(m);
    let reader = Reader::with_memory(mem, "client.dll").unwrap();
    assert_eq!(reader.net_chan_stats().unwrap().ping_ms, 31);
    assert_eq!(reader.net_chan_stats().unwrap().ping_ms, 31);
}

fn add_client(m: &mut MockMemory, base: u64) {
    use deadlock_memory::sig::Pattern;
    use deadlock_reader::globals::SIGNATURES;

    const SIZE: usize = 0x4000;
    let mut image = vec![0xCCu8; SIZE];
    image[0] = 0x4D;
    image[1] = 0x5A;
    let mut at = 0x100usize;
    for (n, d) in SIGNATURES.iter().enumerate() {
        let pat = Pattern::parse(d.pattern).unwrap();
        let mut b = vec![0u8; pat.len()];
        for (i, tok) in d.pattern.split_whitespace().enumerate() {
            if tok != "??" && tok != "?" {
                b[i] = u8::from_str_radix(tok, 16).unwrap();
            }
        }
        let want = 0x3000i64 + n as i64 * 8;
        let disp = (want - (at + d.instr_len) as i64) as i32;
        b[d.disp_off..d.disp_off + 4].copy_from_slice(&disp.to_le_bytes());
        image[at..at + b.len()].copy_from_slice(&b);
        at += 0x200;
    }
    m.add_module("client.dll", base, SIZE);
    m.write(base, &image);
}
