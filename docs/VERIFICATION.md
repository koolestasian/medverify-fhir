# Kani proofs

Linux CI and the Windows gate use Kani 0.67.0. Install the pinned toolchain on Linux or WSL, then run the harnesses from the repository root.

```sh
cargo install --locked kani-verifier --version 0.67.0
cargo kani setup
for proof in encoding_b36_order_roundtrip ncci_ccmi_zero_never_bypasses \
             mue_mai2_split_and_overflow wiser_utn_validity_and_placement
do
  cargo kani -p medverify-core --harness "$proof" || exit
done
```

Kani's unwinding assertions are enabled. The bound includes library loops reached by each harness.

| Harness | Symbolic domain and property | Unwind bound |
| --- | --- | --- |
| `encoding_b36_order_roundtrip` | Every integer from `0` through `36^5 - 1` decodes and re-encodes to itself. A pair of arbitrary five-character uppercase ASCII base-36 codes checks that encoding preserves lexicographic order. The decoder emits canonical uppercase codes. | 8 |
| `ncci_ccmi_zero_never_bypasses` | An active CCMI=0 edit and all four combinations of modifier support on the two lines. The production outcome is always `ModifierProhibited`. | 1 |
| `mue_mai2_split_and_overflow` | Two arbitrary positive `u64` unit counts within one service day and code, any `u16` limit, and both liability flags. The MAI=2 finding is identical to one merged line with the same units and combined flag when the sum fits. A `u128` reference detects overflow of the `u64` sum. | 20 |
| `wiser_utn_validity_and_placement` | Every signed day offset checks the `[0, 120)` rule. Arbitrary field bytes at lengths 0 through 33 and an arbitrary 14-byte UTN check manual UB-04 placement, including each complete match in the first 18 bytes and the exact match at positions 19 through 32. The production length check rejects every other length. | 34 |
