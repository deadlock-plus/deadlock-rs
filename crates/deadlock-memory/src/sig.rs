//! Signature (AOB) scanning and RIP-relative address resolution.
//!
//! Platform-independent and pure: everything here operates on a byte slice, so it is
//! fully unit-testable without a game running.

use crate::error::{Error, Result};

/// A byte pattern with `??` wildcards, in the usual IDA-style hex-string form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pattern {
    bytes: Vec<u8>,
    /// `true` where the corresponding byte must match exactly.
    mask: Vec<bool>,
}

impl Pattern {
    /// Parse a pattern such as `"48 8B 0D ?? ?? ?? ?? 45 33 C9"`.
    ///
    /// Accepts `??` or `?` for wildcards. Whitespace between tokens is ignored.
    pub fn parse(pattern: &str) -> Result<Self> {
        let mut bytes = Vec::new();
        let mut mask = Vec::new();
        for tok in pattern.split_whitespace() {
            if tok == "??" || tok == "?" {
                bytes.push(0);
                mask.push(false);
            } else {
                let b = u8::from_str_radix(tok, 16)
                    .map_err(|_| Error::BadPattern(format!("bad token {tok:?} in {pattern:?}")))?;
                bytes.push(b);
                mask.push(true);
            }
        }
        if bytes.is_empty() {
            return Err(Error::BadPattern("empty pattern".into()));
        }
        Ok(Pattern { bytes, mask })
    }

    /// Number of bytes the pattern spans, wildcards included.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Always false; a parsed pattern has at least one token.
    pub fn is_empty(&self) -> bool {
        false
    }

    fn matches_at(&self, hay: &[u8], at: usize) -> bool {
        self.bytes
            .iter()
            .zip(&self.mask)
            .enumerate()
            .all(|(i, (b, m))| !*m || hay[at + i] == *b)
    }

    /// Index of the first match in `hay`, if any.
    pub fn find(&self, hay: &[u8]) -> Option<usize> {
        self.find_from(hay, 0)
    }

    /// Index of the first match at or after `start`.
    pub fn find_from(&self, hay: &[u8], start: usize) -> Option<usize> {
        let n = self.bytes.len();
        if hay.len() < n {
            return None;
        }
        let last = hay.len() - n;

        if start > last {
            return None;
        }

        // Anchor on the first fixed byte and let `memchr` find its occurrences.
        // Attaching runs three of these over the whole ~100 MiB client image, so a
        // byte-at-a-time scan here dominates attach latency.
        let anchor = self.mask.iter().position(|m| *m);
        match anchor {
            Some(a) => {
                let want = self.bytes[a];
                // Candidate anchors sit at `i + a`, so search from there and map back.
                let from = start + a;
                if from >= hay.len() {
                    return None;
                }
                for r in memchr::memchr_iter(want, &hay[from..]) {
                    let i = start + r;
                    // Hits arrive in order, so the first one past the end ends the search.
                    if i > last {
                        return None;
                    }
                    if self.matches_at(hay, i) {
                        return Some(i);
                    }
                }
                None
            }
            // All-wildcard pattern: matches immediately.
            None => (start <= last).then_some(start),
        }
    }

    /// Every match in `hay`, non-overlapping scan step of one byte.
    pub fn find_all(&self, hay: &[u8]) -> Vec<usize> {
        let mut out = Vec::new();
        let mut at = 0usize;
        while let Some(hit) = self.find_from(hay, at) {
            out.push(hit);
            at = hit + 1;
        }
        out
    }
}

/// A named signature plus the arithmetic needed to turn a match into a global address.
///
/// Mirrors the 0x30-byte descriptor table the original binary carries at RVA `0x138b0a0`:
/// `{ name, name_len, pattern, pattern_len, disp_off, instr_len }`.
#[derive(Clone, Copy, Debug)]
pub struct SigDesc {
    /// Human-readable name, used in diagnostics.
    pub name: &'static str,
    /// IDA-style byte pattern.
    pub pattern: &'static str,
    /// Offset, within the match, of the little-endian `i32` displacement.
    pub disp_off: usize,
    /// Total length of the RIP-relative instruction the pattern anchors on.
    pub instr_len: usize,
}

/// Resolve a RIP-relative reference from a matched signature.
///
/// ```text
/// disp32    = i32le(image[match_off + disp_off ..][..4])
/// global_va = module_base + match_off + instr_len + disp32
/// ```
///
/// `image` must be the module image as loaded (i.e. `module_base` maps to `image[0]`).
pub fn resolve_rip(
    desc: &SigDesc,
    image: &[u8],
    module_base: u64,
    match_off: usize,
) -> Result<u64> {
    // Saturating: `match_off` comes from a scan of the target's own image.
    let lo = match_off.saturating_add(desc.disp_off);
    let hi = lo.saturating_add(4);
    if hi > image.len() {
        // Not `SignatureNotFound`. The pattern *was* found; the image is too short to hold
        // the displacement it points at, which is a different problem with a different fix.
        return Err(Error::SignatureTruncated {
            name: desc.name,
            needs: hi,
            image_len: image.len(),
        });
    }
    let disp = i32::from_le_bytes([image[lo], image[lo + 1], image[lo + 2], image[lo + 3]]);
    let next = module_base
        .wrapping_add(match_off as u64)
        .wrapping_add(desc.instr_len as u64);
    Ok(next.wrapping_add(disp as i64 as u64))
}

/// Find `desc` in `image` and resolve it, rejecting results outside the module.
pub fn scan_and_resolve(
    desc: &SigDesc,
    image: &[u8],
    module_base: u64,
    module_size: usize,
) -> Result<u64> {
    let pat = Pattern::parse(desc.pattern)?;
    let hit = pat.find(image).ok_or(Error::SignatureNotFound(desc.name))?;
    let va = resolve_rip(desc, image, module_base, hit)?;
    let end = module_base.wrapping_add(module_size as u64);
    if va < module_base || va >= end {
        return Err(Error::OutsideModule {
            name: desc.name,
            va,
        });
    }
    Ok(va)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_wildcards() {
        let p = Pattern::parse("48 8B 0D ?? ?? ?? ?? 45").unwrap();
        assert_eq!(p.len(), 8);
        assert_eq!(
            p.mask,
            vec![true, true, true, false, false, false, false, true]
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(Pattern::parse("48 ZZ").is_err());
        assert!(Pattern::parse("").is_err());
    }

    #[test]
    fn finds_with_wildcards() {
        let hay = [
            0x00, 0x11, 0x48, 0x8B, 0x0D, 0xDE, 0xAD, 0xBE, 0xEF, 0x45, 0x99,
        ];
        let p = Pattern::parse("48 8B 0D ?? ?? ?? ?? 45").unwrap();
        assert_eq!(p.find(&hay), Some(2));
    }

    /// The anchor is the first *fixed* byte, which need not be the pattern's first byte.
    /// Mapping a `memchr` hit on that byte back to the pattern start is exactly where an
    /// off-by-one would hide, and a wrong offset still finds *a* match.
    #[test]
    fn leading_wildcards_do_not_shift_the_match() {
        let pat = Pattern::parse("?? ?? BE EF").unwrap();
        let hay = [0x00u8, 0xBE, 0xEF, 0x11, 0x22, 0xBE, 0xEF, 0x99];

        assert_eq!(pat.find(&hay), Some(3));
        assert_eq!(pat.find_from(&hay, 3), Some(3));
        assert_eq!(pat.find_from(&hay, 4), None, "nothing left to match");
    }

    /// `find_from` has to skip hits before `start` without losing later ones.
    #[test]
    fn find_from_respects_its_start() {
        let pat = Pattern::parse("AB CD").unwrap();
        let hay = [0xABu8, 0xCD, 0x00, 0xAB, 0xCD, 0x00];

        assert_eq!(pat.find_from(&hay, 0), Some(0));
        assert_eq!(pat.find_from(&hay, 1), Some(3));
        assert_eq!(pat.find_from(&hay, 3), Some(3));
        assert_eq!(pat.find_from(&hay, 4), None);
        assert_eq!(pat.find_from(&hay, 99), None);
    }

    #[test]
    fn find_all_reports_every_hit() {
        let hay = [0xAA, 0xBB, 0xAA, 0xBB, 0xAA, 0xBB];
        let p = Pattern::parse("AA BB").unwrap();
        assert_eq!(p.find_all(&hay), vec![0, 2, 4]);
    }

    /// Hand-built `mov rcx, [rip+0x10]` at image offset 4, base 0x1000.
    /// Instruction spans [4, 11); next instruction is at 0x1000 + 11 = 0x100B.
    /// Target should therefore be 0x100B + 0x10 = 0x101B.
    #[test]
    fn resolves_rip_forward() {
        let mut image = vec![0u8; 64];
        image[4..7].copy_from_slice(&[0x48, 0x8B, 0x0D]);
        image[7..11].copy_from_slice(&0x10i32.to_le_bytes());
        let desc = SigDesc {
            name: "test",
            pattern: "48 8B 0D ?? ?? ?? ??",
            disp_off: 3,
            instr_len: 7,
        };
        assert_eq!(resolve_rip(&desc, &image, 0x1000, 4).unwrap(), 0x101B);
    }

    /// Negative displacements must sign-extend, not wrap through u32.
    #[test]
    fn resolves_rip_backward() {
        let mut image = vec![0u8; 64];
        image[20..23].copy_from_slice(&[0x48, 0x8B, 0x0D]);
        image[23..27].copy_from_slice(&(-0x20i32).to_le_bytes());
        let desc = SigDesc {
            name: "test",
            pattern: "48 8B 0D ?? ?? ?? ??",
            disp_off: 3,
            instr_len: 7,
        };
        assert_eq!(resolve_rip(&desc, &image, 0x1000, 20).unwrap(), 0xFFB);
    }

    #[test]
    fn rejects_resolution_outside_module() {
        let mut image = vec![0u8; 64];
        image[0..3].copy_from_slice(&[0x48, 0x8B, 0x0D]);
        image[3..7].copy_from_slice(&0x7000_0000i32.to_le_bytes());
        let desc = SigDesc {
            name: "test",
            pattern: "48 8B 0D ?? ?? ?? ??",
            disp_off: 3,
            instr_len: 7,
        };
        let err = scan_and_resolve(&desc, &image, 0x1000, 64).unwrap_err();
        assert!(matches!(err, Error::OutsideModule { .. }));
    }
}
