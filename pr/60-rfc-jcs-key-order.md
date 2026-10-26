# docs(rfc): JCS key ordering is by UTF-16 code units, not UTF-8 bytes

Status: draft · Base: pr/33-signatures · RFC.md text only

## Why
RFC.md:719 said object keys are "sorted lexicographically by UTF-8 bytes". RFC 8785 (§3.2.3) sorts by UTF-16 code units.
The two orders agree except for keys with characters outside the BMP (surrogate pairs sort before U+E000-U+FFFF in UTF-16,
after them in UTF-8). The implementation already follows RFC 8785 (its reference vector `weird.json` requires it), so only
the text was false. pr/33 recorded this as a blocker; pr/45 listed it as still open.

## What changes
- RFC.md "Canonicalization Rules" rule 1 now states UTF-16 code unit order and says where it differs from byte order.

## Not changed
- The `.qual` blocker annotation for RFC.md:719 is the qualifier's, not hand-edited here: resolve it with the qualifier tool
  after this lands. The rest of the signatures findings in pr/33 (path signature covers ids, key material, witness/ci forms,
  SSHSIG namespace) are untouched.
- `cargo test` was not needed for a text-only change; the signature tests that pin `weird.json` are unaffected.
