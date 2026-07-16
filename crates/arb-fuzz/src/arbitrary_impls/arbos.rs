use arbitrary::{Arbitrary, Unstructured};
use serde::Serialize;

/// ArbOS version selector drawn from the live set.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ArbosVersion(pub u64);

impl<'a> Arbitrary<'a> for ArbosVersion {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        // The arb1-era band v6..v9 exercises pre-v8 l1BlockNumber++, pre-v9
        // drop_tip, and pre-v10 batch-poster spending; every entry must have a
        // matching chain412346_v{N}.json genesis cache or the dual-exec sweep
        // falls back to a builder genesis and risks false-positive state-root
        // diffs.
        const CANDIDATES: [u64; 15] = [6, 7, 8, 9, 10, 11, 20, 30, 31, 32, 40, 41, 50, 51, 60];
        let max_idx = CANDIDATES.len() - 1;
        let idx = u.int_in_range(0..=max_idx)?;
        Ok(ArbosVersion(CANDIDATES[idx]))
    }
}
