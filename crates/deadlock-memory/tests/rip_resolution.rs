//! Ground-truth tests for RIP-relative resolution.
//!
//! The byte sequences below are real instructions whose targets were confirmed
//! independently by disassembly; the arithmetic is plain RIP-relative addressing. They
//! exercise the same arithmetic the reader applies to `client.dll`, so a regression here
//! means the signature resolver would silently point at the wrong global.

use deadlock_memory::sig::{Pattern, SigDesc, resolve_rip};

/// `lea reg, [rip + disp32]` - 3-byte opcode, 4-byte displacement.
const LEA: SigDesc = SigDesc {
    name: "lea_rip",
    pattern: "48 8D ?? ?? ?? ?? ??",
    disp_off: 3,
    instr_len: 7,
};

const IMAGE_BASE: u64 = 0x1_4000_0000;

/// Place `bytes` at `rva` inside a synthetic image and resolve.
fn resolve_at(bytes: &[u8], rva: usize) -> u64 {
    let mut image = vec![0u8; rva + bytes.len() + 16];
    image[rva..rva + bytes.len()].copy_from_slice(bytes);
    resolve_rip(&LEA, &image, IMAGE_BASE, rva).unwrap()
}

/// `0051951f  48 8d 0d 7a 1b e7 00   lea rcx, [rip + 0xe71b7a]`
///
/// This is the instruction that loads the signature descriptor table; the target was
/// confirmed to be RVA `0x138b0a0`, where the three-entry table lives.
#[test]
fn positive_displacement_matches_disassembly() {
    let bytes = [0x48, 0x8d, 0x0d, 0x7a, 0x1b, 0xe7, 0x00];
    assert_eq!(resolve_at(&bytes, 0x51951f), IMAGE_BASE + 0x138b0a0);
}

/// `0059e7f9  48 8d 05 00 c2 a7 ff   lea rax, [rip - 0x583e00]`
///
/// A backwards reference, so this fails loudly if the displacement is ever treated as
/// unsigned. Confirmed target: RVA `0x1aa00`.
#[test]
fn negative_displacement_sign_extends() {
    let bytes = [0x48, 0x8d, 0x05, 0x00, 0xc2, 0xa7, 0xff];
    assert_eq!(resolve_at(&bytes, 0x59e7f9), IMAGE_BASE + 0x1aa00);
}

/// The pattern engine must locate an instruction embedded in surrounding noise, then
/// resolve it correctly from that offset.
#[test]
fn scan_then_resolve_end_to_end() {
    let instr = [0x48u8, 0x8d, 0x0d, 0x7a, 0x1b, 0xe7, 0x00];
    let rva: usize = 0x51951f;
    let mut image = vec![0xCCu8; rva + 64];
    image[rva..rva + instr.len()].copy_from_slice(&instr);

    let pat = Pattern::parse(LEA.pattern).unwrap();
    let hit = pat.find(&image).expect("pattern should match");
    assert_eq!(hit, rva);
    assert_eq!(
        resolve_rip(&LEA, &image, IMAGE_BASE, hit).unwrap(),
        IMAGE_BASE + 0x138b0a0
    );
}
