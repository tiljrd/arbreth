use arbitrary::{Arbitrary, Unstructured};
use serde::Serialize;

/// ArbOS version selector drawn from the live set.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ArbosVersion(pub u64);

impl<'a> Arbitrary<'a> for ArbosVersion {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        // v6 is arb1's launch version; included so the fuzz sweep exercises
        // pre-v8 l1BlockNumber++, pre-v9 drop_tip, and pre-v10 batch-poster
        // spending in addition to the post-Nitro path. Add v7/v8/v9 once
        // their per-chain genesis caches are captured.
        const CANDIDATES: [u64; 12] = [6, 10, 11, 20, 30, 31, 32, 40, 41, 50, 51, 60];
        let max_idx = CANDIDATES.len() - 1;
        let idx = u.int_in_range(0..=max_idx)?;
        Ok(ArbosVersion(CANDIDATES[idx]))
    }
}
