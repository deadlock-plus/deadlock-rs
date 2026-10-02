//! The signature table must be internally consistent.

use deadlock_memory::sig::Pattern;

/// Every shipped signature must be internally consistent: the displacement it reads has
/// to lie inside the bytes the pattern actually matches, otherwise the resolver would be
/// reading whatever follows the match.
#[test]
fn shipped_signatures_are_self_consistent() {
    for d in deadlock_reader::globals::SIGNATURES {
        let pat = Pattern::parse(d.pattern).unwrap_or_else(|e| panic!("{}: {e}", d.name));
        assert!(
            d.disp_off + 4 <= pat.len(),
            "{}: disp32 at +{} escapes the {}-byte pattern",
            d.name,
            d.disp_off,
            pat.len()
        );
        assert!(
            d.instr_len >= d.disp_off + 4,
            "{}: instruction length {} cannot end before its own displacement",
            d.name,
            d.instr_len
        );
    }
}
