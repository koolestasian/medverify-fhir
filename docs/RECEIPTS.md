# Signed NCCI receipts

Create a `medverify_core::receipt::ReceiptIndexes` from the loaded PTP and MUE indexes and reuse it across claims. `Receipt::issue_ncci` takes that pair, the claim lines, and an Ed25519 signing key. It evaluates the lines and returns a receipt plus a fresh 32-byte secret nonce. Publish `Receipt::to_json()` and retain the nonce with the original input in the operator's protected records. `Receipt::check_ncci` opens the commitments locally using those lines, the nonce, and the same engine build and indexes.

Give recipients the public key through a trusted channel independent of the receipt. Store the raw 32-byte Ed25519 public key as 64 lowercase hexadecimal characters with an optional final newline. `SigningKey::verifying_key().to_bytes()` returns the raw key bytes.

```sh
medverify verify-receipt receipt.json --public-key public.key
```

The verifier checks the schema and signature offline. Success authenticates what the signer recorded. Opening the commitments and reproducing the evaluation requires the operator's private records.

## Version 1 format

The JSON envelope has a `payload` object and a `signature` string of 128 lowercase hexadecimal characters. The Ed25519 message is the ASCII string `MedVerify receipt signature v1`, one zero byte, and the UTF-8 [RFC 8785](https://www.rfc-editor.org/rfc/rfc8785) canonical form of the payload. The verifier rejects duplicate and unknown fields at both levels.

| Payload field | Value |
| --- | --- |
| `schema_version` | Integer `1` |
| `engine_version` | Cargo package version as three decimal components |
| `index_digest` | SHA-256 of the loaded PTP and MUE files in that order, including their headers |
| `policy_digest` | SHA-256 of the bundled Washington WISeR and PAS reviewer policy tables in that order |
| `input_commitment` | HMAC-SHA-256 of the private input |
| `verdict_commitment` | HMAC-SHA-256 of the private PTP and MUE findings |
| `finding_codes` | Unique allowlisted PTP outcomes and MUE scopes in schema order |

The four digests and commitments use 64 lowercase hexadecimal characters. The bundle hash starts with `MedVerify index bundle v1` or `MedVerify policy bundle v1`, followed by a zero byte. Each file is preceded by its byte length as an unsigned 64-bit big-endian integer. `ReceiptIndexes::new` hashes the original little-endian file bytes from the loaded snapshots once. The policy digest identifies the engine's bundled tables. Version 1 issuance covers NCCI evaluations.

For each commitment, the nonce is the HMAC key. The message starts with `MedVerify NCCI input v1` or `MedVerify NCCI verdict v1` and a zero byte. The rest is the engine's compact JSON serialization of the original ordered claim lines or the pair of PTP and MUE finding arrays. Both retain exact 64-bit integers. Claim IDs, dates, procedure codes, free text, raw findings and the nonce stay in the operator's private records.
