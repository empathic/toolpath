//! RFC.md "Signature Algorithm" and "Verification Algorithm", as pure functions.
//!
//! - [`jcs`]: RFC 8785 canonical form.
//! - [`step_author_input`], [`path_author_input`], [`path_reviewer_input`]: the
//!   three canonical inputs "What Gets Signed" defines, and [`digest`] (SHA-256).
//! - [`verify_step_signature`] and [`verify_path_signatures`]: the RFC's
//!   verification pseudo-code for `ssh` keys (Ed25519 signatures in the SSHSIG
//!   format, namespace [`SSHSIG_NAMESPACE`]). `gpg` and `sigstore` keys are
//!   refused as [`VerifyError::UnsupportedKeyType`].
//!
//! No key material is ever read from the document: a [`Key`] carries a type and
//! a fingerprint, not the public key, so the caller resolves keys and this
//! module checks that the resolved key's fingerprint is the one the document
//! names.

use std::fmt;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use ssh_key::{HashAlg, PublicKey, SshSig};

use crate::types::{ActorDefinition, Key, Path, Signature, Step};

/// The SSHSIG namespace every Toolpath `ssh` signature is made in. The RFC
/// names the SSH signature format but no namespace; this is the one this
/// implementation fixes.
pub const SSHSIG_NAMESPACE: &str = "toolpath";

/// RFC 8785 canonical form of a JSON value: object keys in UTF-16 code-unit
/// order (§3.2.3), no whitespace, minimal string escaping (§3.2.2.2), numbers
/// as ECMAScript writes them (§3.2.2.3, via `ryu-js`).
///
/// Written here rather than taken from `serde_json_canonicalizer`, which turns
/// on serde_json's `float_roundtrip` for the whole build and so changes how
/// every other crate parses floats.
pub fn jcs(value: &Value) -> String {
    let mut out = String::new();
    write_value(value, &mut out);
    out
}

fn write_value(value: &Value, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => {
            // I-JSON: every number is an IEEE 754 double. serde_json never holds NaN or ±inf.
            let f = n.as_f64().expect("a serde_json number converts to f64");
            if f == 0.0 {
                out.push('0'); // -0 too
            } else {
                out.push_str(ryu_js::Buffer::new().format_finite(f));
            }
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(&map[key], out);
            }
            out.push('}');
        }
    }
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// SHA-256 of a canonical input.
pub fn digest(canonical_input: &str) -> [u8; 32] {
    Sha256::digest(canonical_input.as_bytes()).into()
}

/// Step Signature (Author): `JCS({"change": …, "step": …})`; `meta` is excluded.
pub fn step_author_input(step: &Step) -> String {
    jcs(&json!({ "change": step.change, "step": step.step }))
}

/// Path Signature (Author): `JCS({"path": …, "step_ids": [ids in steps order]})`.
pub fn path_author_input(path: &Path) -> String {
    let step_ids: Vec<&str> = path.steps.iter().map(|s| s.step.id.as_str()).collect();
    jcs(&json!({ "path": path.path, "step_ids": step_ids }))
}

/// Path Signature (Reviewer): `JCS({"head": …, "path_id": …, "reviewed_at": …})`.
pub fn path_reviewer_input(path_id: &str, head: &str, reviewed_at: &str) -> String {
    jcs(&json!({ "head": head, "path_id": path_id, "reviewed_at": reviewed_at }))
}

/// Why a signature did not verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// No signature with this scope in `meta.signatures`.
    MissingScope(String),
    /// The scope has no canonical form in the RFC (only `author` and `reviewer` do).
    UnsupportedScope(String),
    /// The signer is not defined in `meta.actors`.
    UnknownSigner(String),
    /// The signer's definition has no key matching the signature's `key`.
    NoMatchingKey { signer: String, key: String },
    /// A key type this implementation does not verify (`gpg`, `sigstore`).
    UnsupportedKeyType(String),
    /// The resolver had no public key for this key.
    KeyUnavailable(String),
    /// The resolved public key's fingerprint is not the one the document names.
    FingerprintMismatch { expected: String, actual: String },
    /// A reviewer signature without the `timestamp` its canonical form needs.
    MissingTimestamp,
    /// The `sig` is not a well-formed SSH signature.
    MalformedSignature(String),
    /// The signature does not verify over this canonical input.
    BadSignature(String),
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingScope(s) => write!(f, "no signature with scope {s:?}"),
            Self::UnsupportedScope(s) => {
                write!(f, "the RFC defines no canonical form for scope {s:?}")
            }
            Self::UnknownSigner(s) => write!(f, "signer {s:?} is not in meta.actors"),
            Self::NoMatchingKey { signer, key } => write!(f, "{signer:?} has no key {key:?}"),
            Self::UnsupportedKeyType(t) => write!(f, "key type {t:?} is not verified"),
            Self::KeyUnavailable(k) => write!(f, "no public key for {k:?}"),
            Self::FingerprintMismatch { expected, actual } => {
                write!(
                    f,
                    "key fingerprint is {actual}, the document names {expected}"
                )
            }
            Self::MissingTimestamp => write!(f, "reviewer signature has no timestamp"),
            Self::MalformedSignature(e) => write!(f, "malformed ssh signature: {e}"),
            Self::BadSignature(e) => write!(f, "signature does not verify: {e}"),
        }
    }
}

impl std::error::Error for VerifyError {}

/// `verify_step_signature`: canonical form → SHA-256 → SSHSIG verify with `public_key`.
pub fn verify_step_signature(
    step: &Step,
    signature: &Signature,
    public_key: &PublicKey,
) -> Result<(), VerifyError> {
    verify_digest(&digest(&step_author_input(step)), signature, public_key)
}

/// `verify_path_signatures`: every scope in `required_scopes` must have a
/// signature in the path's `meta.signatures` that verifies over that scope's
/// canonical form, with the signer's key from `meta.actors`. With
/// `verify_steps`, every step's own `author` signatures must verify too (the
/// signer looked up in the step's `meta.actors`, then the path's).
///
/// `resolve` maps a document [`Key`] to its public key; its fingerprint must
/// equal `Key.fingerprint`.
pub fn verify_path_signatures(
    path: &Path,
    required_scopes: &[&str],
    verify_steps: bool,
    resolve: &dyn Fn(&Key) -> Option<PublicKey>,
) -> Result<(), VerifyError> {
    let meta = path.meta.as_ref();
    let signatures: &[Signature] = meta.map(|m| m.signatures.as_slice()).unwrap_or(&[]);
    let actors = meta.and_then(|m| m.actors.as_ref());
    for scope in required_scopes {
        let candidates: Vec<&Signature> = signatures.iter().filter(|s| s.scope == *scope).collect();
        if candidates.is_empty() {
            return Err(VerifyError::MissingScope((*scope).to_string()));
        }
        let mut last = None;
        let mut ok = false;
        for sig in candidates {
            let input = match *scope {
                "author" => path_author_input(path),
                "reviewer" => match &sig.timestamp {
                    Some(at) => path_reviewer_input(&path.path.id, &path.path.head, at),
                    None => {
                        last = Some(VerifyError::MissingTimestamp);
                        continue;
                    }
                },
                other => return Err(VerifyError::UnsupportedScope(other.to_string())),
            };
            let key = match signer_key(sig, actors.and_then(|a| a.get(&sig.signer)), resolve) {
                Ok(k) => k,
                Err(e) => {
                    last = Some(e);
                    continue;
                }
            };
            match verify_digest(&digest(&input), sig, &key) {
                Ok(()) => {
                    ok = true;
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        if !ok {
            return Err(last.expect("at least one candidate was tried"));
        }
    }
    if verify_steps {
        for step in &path.steps {
            let step_meta = step.meta.as_ref();
            for sig in step_meta
                .map(|m| m.signatures.as_slice())
                .unwrap_or(&[])
                .iter()
                .filter(|s| s.scope == "author")
            {
                let def = step_meta
                    .and_then(|m| m.actors.as_ref())
                    .and_then(|a| a.get(&sig.signer))
                    .or_else(|| actors.and_then(|a| a.get(&sig.signer)));
                let key = signer_key(sig, def, resolve)?;
                verify_step_signature(step, sig, &key)?;
            }
        }
    }
    Ok(())
}

/// The document's identifier for a key: `type:fingerprint`, spaces removed.
pub fn key_id(key: &Key) -> String {
    format!("{}:{}", key.key_type, key.fingerprint.replace(' ', ""))
}

fn signer_key(
    sig: &Signature,
    def: Option<&ActorDefinition>,
    resolve: &dyn Fn(&Key) -> Option<PublicKey>,
) -> Result<PublicKey, VerifyError> {
    let def = def.ok_or_else(|| VerifyError::UnknownSigner(sig.signer.clone()))?;
    let key = def
        .keys
        .iter()
        .find(|k| key_id(k) == sig.key.replace(' ', ""))
        .ok_or_else(|| VerifyError::NoMatchingKey {
            signer: sig.signer.clone(),
            key: sig.key.clone(),
        })?;
    if key.key_type != "ssh" {
        return Err(VerifyError::UnsupportedKeyType(key.key_type.clone()));
    }
    let public = resolve(key).ok_or_else(|| VerifyError::KeyUnavailable(sig.key.clone()))?;
    let actual = public.fingerprint(HashAlg::Sha256).to_string();
    if actual != key.fingerprint {
        return Err(VerifyError::FingerprintMismatch {
            expected: key.fingerprint.clone(),
            actual,
        });
    }
    Ok(public)
}

fn verify_digest(
    digest: &[u8; 32],
    signature: &Signature,
    key: &PublicKey,
) -> Result<(), VerifyError> {
    let sshsig = SshSig::from_pem(signature.sig.as_bytes())
        .map_err(|e| VerifyError::MalformedSignature(e.to_string()))?;
    key.verify(SSHSIG_NAMESPACE, digest, &sshsig)
        .map_err(|e| VerifyError::BadSignature(e.to_string()))
}
