import json, collections, sys
from pathlib import Path

# Schema and generated output both live in this crate; override the root with argv[1].
READER = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[1]

src = json.load(open(READER / 'schema-fields.json'))
rows = src['fields']

merged = {}
conflicts = []
for r in rows:
    c, f = r['class'], r['field']
    if not c or not f:
        continue
    fb = int(r['fallback'], 16) if r['fallback'] else None
    key = (c, f)
    if key in merged:
        prev = merged[key]
        if prev['fallback'] is not None and fb is not None and prev['fallback'] != fb:
            conflicts.append((key, prev['fallback'], fb))
        if prev['fallback'] is None and fb is not None:
            prev['fallback'] = fb
        prev['sites'].append(r['call_site'])
        prev['dynamic'] = prev['dynamic'] and r['resolution'] == 'dynamic'
    else:
        merged[key] = {'fallback': fb, 'sites': [r['call_site']],
                       'dynamic': r['resolution'] == 'dynamic'}

print('unique pairs:', len(merged))
print('conflicts:', conflicts)

by_class = collections.OrderedDict()
for (c, f), v in sorted(merged.items()):
    by_class.setdefault(c, []).append((f, v))

out = []
out.append('''//! Class/field table recovered from Statlocker Companion.
//!
//! Generated from `schema-fields.json` (88 call sites of the schema-offset helper at
//! RVA `0x545150`, 80 statically resolved). Do not edit by hand.
//!
//! `fallback` is the offset the original binary hardcodes for use *only* when the
//! runtime schema lookup misses. These constants are specific to the Deadlock build
//! current when the binary was compiled (2025-08-01) and will drift. Always prefer a
//! resolved schema offset; see [`crate::Reader::offset_of`].

/// One `(class, field)` pair the original reader looks up, with its hardcoded fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldSpec {
    /// Schema class name, e.g. `"C_CitadelPlayerPawn"`.
    pub class: &'static str,
    /// Schema field name, e.g. `"m_iHealth"`.
    pub field: &'static str,
    /// Offset baked into the original binary, used only when the schema lookup misses.
    pub fallback: Option<u32>,
    /// True when the original resolved the class name at runtime rather than from a
    /// literal, so the pairing here is inferred from context rather than proven.
    pub inferred: bool,
}

/// Every `(class, field)` pair the original reader is known to request.
///
/// MSVC/PE layout only. Use [`crate::abi::Abi::fallback_offset`] to pick by target ABI.
pub static WIN64_FIELDS: &[FieldSpec] = &[''')

for cls, fields in by_class.items():
    out.append('    // ---- %s ----' % cls)
    for f, v in fields:
        fb = 'None' if v['fallback'] is None else 'Some(0x%x)' % v['fallback']
        out.append('    FieldSpec { class: %-38s field: %-30s fallback: %-12s inferred: %-5s },'
                   % ('"%s",' % cls, '"%s",' % f, fb + ',', str(v['dynamic']).lower()))
out.append('];')
out.append('''
/// Look up the hardcoded fallback offset for a `(class, field)` pair.
///
/// Returns `None` when the pair is unknown *or* known but without a fallback (the
/// original skips those reads entirely rather than guessing).
pub fn win64_fallback_offset(class: &str, field: &str) -> Option<u32> {
    WIN64_FIELDS
        .iter()
        .find(|s| s.class == class && s.field == field)
        .and_then(|s| s.fallback)
}

/// Look up the full spec for a `(class, field)` pair.
pub fn spec(class: &str, field: &str) -> Option<&'static FieldSpec> {
    WIN64_FIELDS.iter().find(|s| s.class == class && s.field == field)
}

/// All distinct class names referenced by [`WIN64_FIELDS`].
pub fn classes() -> impl Iterator<Item = &'static str> {
    let mut seen: Vec<&'static str> = WIN64_FIELDS.iter().map(|s| s.class).collect();
    seen.sort_unstable();
    seen.dedup();
    seen.into_iter()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_and_unique() {
        let mut pairs: Vec<_> = WIN64_FIELDS.iter().map(|s| (s.class, s.field)).collect();
        let before = pairs.len();
        pairs.sort_unstable();
        pairs.dedup();
        assert_eq!(before, pairs.len(), "duplicate (class, field) rows in WIN64_FIELDS");
    }

    #[test]
    fn known_anchor_offsets() {
        // The three entry points the reader cannot work without.
        assert_eq!(win64_fallback_offset("C_CitadelGameRulesProxy", "m_pGameRules"), Some(0x5f0));
        assert_eq!(win64_fallback_offset("CCitadelPlayerController", "m_PlayerDataGlobal"), Some(0x8f0));
        assert_eq!(win64_fallback_offset("CCitadelPlayerController", "m_steamID"), Some(0x778));
    }

    #[test]
    fn scoreboard_fields_present() {
        for f in [
            "m_nHeroID", "m_iLevel", "m_iGoldNetWorth", "m_iPlayerKills", "m_iDeaths",
            "m_iPlayerAssists", "m_iLastHits", "m_iDenies", "m_iHeroDamage",
            "m_iObjectiveDamage", "m_iHeroHealing",
        ] {
            assert!(spec("PlayerDataGlobal_t", f).is_some(), "missing PlayerDataGlobal_t::{f}");
        }
    }
}''')

open(READER / 'src' / 'fields.rs', 'w', newline='\n').write('\n'.join(out) + '\n')
print('wrote src/fields.rs')
