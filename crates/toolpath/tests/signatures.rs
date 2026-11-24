//! RFC.md "Verification Algorithm", proven with Ed25519 `ssh` keys made in the
//! test from fixed seeds (never a real key).

use std::collections::HashMap;

use serde_json::json;
use ssh_key::private::Ed25519Keypair;
use ssh_key::{HashAlg, LineEnding, PrivateKey, PublicKey};
use toolpath::v1::sign::{
    SSHSIG_NAMESPACE, VerifyError, digest, path_author_input, path_reviewer_input,
    step_author_input, verify_path_signatures, verify_step_signature,
};
use toolpath::v1::{Key, Path, Signature};

fn keypair(seed: u8) -> PrivateKey {
    PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
}

fn fingerprint(k: &PrivateKey) -> String {
    k.public_key().fingerprint(HashAlg::Sha256).to_string()
}

fn sign(k: &PrivateKey, canonical_input: &str) -> String {
    k.sign(SSHSIG_NAMESPACE, HashAlg::Sha512, &digest(canonical_input))
        .unwrap()
        .to_pem(LineEnding::LF)
        .unwrap()
}

fn sig(signer: &str, k: &PrivateKey, scope: &str, ts: Option<&str>, input: &str) -> Signature {
    Signature {
        signer: signer.into(),
        key: format!("ssh:{}", fingerprint(k)),
        scope: scope.into(),
        sig: sign(k, input),
        timestamp: ts.map(Into::into),
    }
}

/// A two-step path whose actors carry the two test keys.
fn path(alex: &PrivateKey, bob: &PrivateKey) -> Path {
    serde_json::from_value(json!({
        "path": { "id": "pr-42", "base": { "uri": "github:org/repo", "ref": "abc" }, "head": "step-002" },
        "steps": [
            { "step": { "id": "step-001", "actor": "human:alex", "timestamp": "2026-01-29T10:00:00Z" },
              "change": { "src/main.rs": { "raw": "@@ -1,1 +1,1 @@\n-hello\n+world" } },
              "meta": { "intent": "Fix greeting" } },
            { "step": { "id": "step-002", "parents": ["step-001"], "actor": "human:alex", "timestamp": "2026-01-29T10:05:00Z" },
              "change": { "src/main.rs": { "raw": "@@ -1,1 +1,1 @@\n-world\n+world!" } } }
        ],
        "meta": { "actors": {
            "human:alex": { "keys": [{ "type": "ssh", "fingerprint": fingerprint(alex) }] },
            "human:bob":  { "keys": [{ "type": "ssh", "fingerprint": fingerprint(bob) }] }
        } }
    }))
    .unwrap()
}

/// The resolver a caller supplies: fingerprint → public key, from its own store.
fn store(keys: &[&PrivateKey]) -> impl Fn(&Key) -> Option<PublicKey> {
    let map: HashMap<String, PublicKey> = keys
        .iter()
        .map(|k| (fingerprint(k), k.public_key().clone()))
        .collect();
    move |key: &Key| map.get(&key.fingerprint).cloned()
}

/// Author and reviewer signatures added to a path's meta (and step 1's author signature).
fn signed(alex: &PrivateKey, bob: &PrivateKey) -> Path {
    let mut p = path(alex, bob);
    let author = sig("human:alex", alex, "author", None, &path_author_input(&p));
    let review_at = "2026-01-29T16:00:00Z";
    let reviewer = sig(
        "human:bob",
        bob,
        "reviewer",
        Some(review_at),
        &path_reviewer_input(&p.path.id, &p.path.head, review_at),
    );
    p.meta.as_mut().unwrap().signatures = vec![author, reviewer];
    let step_sig = sig(
        "human:alex",
        alex,
        "author",
        None,
        &step_author_input(&p.steps[0]),
    );
    p.steps[0].meta.as_mut().unwrap().signatures = vec![step_sig];
    p
}

#[test]
fn signed_step_and_path_verify() {
    let (alex, bob) = (keypair(1), keypair(2));
    let p = signed(&alex, &bob);
    verify_step_signature(
        &p.steps[0],
        &p.steps[0].meta.as_ref().unwrap().signatures[0],
        alex.public_key(),
    )
    .unwrap();
    verify_path_signatures(&p, &["author", "reviewer"], true, &store(&[&alex, &bob])).unwrap();
}

#[test]
fn step_meta_is_not_signed() {
    let (alex, bob) = (keypair(1), keypair(2));
    let mut p = signed(&alex, &bob);
    p.steps[0].meta.as_mut().unwrap().intent = Some("changed after signing".into());
    let s = p.steps[0].meta.as_ref().unwrap().signatures[0].clone();
    verify_step_signature(&p.steps[0], &s, alex.public_key()).unwrap();
}

#[test]
fn tampered_change_fails() {
    let (alex, bob) = (keypair(1), keypair(2));
    let mut p = signed(&alex, &bob);
    let s = p.steps[0].meta.as_ref().unwrap().signatures[0].clone();
    p.steps[0].change.get_mut("src/main.rs").unwrap().raw = Some("@@ tampered".into());
    assert!(matches!(
        verify_step_signature(&p.steps[0], &s, alex.public_key()),
        Err(VerifyError::BadSignature(_))
    ));
}

#[test]
fn tampered_step_identity_fails() {
    let (alex, bob) = (keypair(1), keypair(2));
    let mut p = signed(&alex, &bob);
    p.steps[0].step.actor = "agent:someone-else".into();
    assert!(matches!(
        verify_path_signatures(&p, &["author"], true, &store(&[&alex, &bob])),
        Err(VerifyError::BadSignature(_))
    ));
}

#[test]
fn moved_head_fails_both_path_signatures() {
    let (alex, bob) = (keypair(1), keypair(2));
    let mut p = signed(&alex, &bob);
    p.path.head = "step-001".into();
    for scope in ["author", "reviewer"] {
        assert!(
            matches!(
                verify_path_signatures(&p, &[scope], false, &store(&[&alex, &bob])),
                Err(VerifyError::BadSignature(_))
            ),
            "{scope}"
        );
    }
}

#[test]
fn reordered_steps_fail_the_author_signature() {
    let (alex, bob) = (keypair(1), keypair(2));
    let mut p = signed(&alex, &bob);
    p.steps.reverse();
    assert!(matches!(
        verify_path_signatures(&p, &["author"], false, &store(&[&alex, &bob])),
        Err(VerifyError::BadSignature(_))
    ));
}

#[test]
fn missing_required_scope_fails() {
    let (alex, bob) = (keypair(1), keypair(2));
    let mut p = signed(&alex, &bob);
    p.meta
        .as_mut()
        .unwrap()
        .signatures
        .retain(|s| s.scope != "reviewer");
    assert_eq!(
        verify_path_signatures(&p, &["author", "reviewer"], false, &store(&[&alex, &bob])),
        Err(VerifyError::MissingScope("reviewer".into()))
    );
}

#[test]
fn signer_not_in_actors_fails() {
    let (alex, bob) = (keypair(1), keypair(2));
    let mut p = signed(&alex, &bob);
    p.meta
        .as_mut()
        .unwrap()
        .actors
        .as_mut()
        .unwrap()
        .remove("human:bob");
    assert_eq!(
        verify_path_signatures(&p, &["reviewer"], false, &store(&[&alex, &bob])),
        Err(VerifyError::UnknownSigner("human:bob".into()))
    );
}

#[test]
fn wrong_scope_canonical_form_fails() {
    // A reviewer signature made over the author canonical form does not verify as reviewer.
    let (alex, bob) = (keypair(1), keypair(2));
    let mut p = signed(&alex, &bob);
    let wrong = sig(
        "human:bob",
        &bob,
        "reviewer",
        Some("2026-01-29T16:00:00Z"),
        &path_author_input(&p),
    );
    p.meta
        .as_mut()
        .unwrap()
        .signatures
        .retain(|s| s.scope != "reviewer");
    p.meta.as_mut().unwrap().signatures.push(wrong);
    assert!(matches!(
        verify_path_signatures(&p, &["reviewer"], false, &store(&[&alex, &bob])),
        Err(VerifyError::BadSignature(_))
    ));
}

#[test]
fn wrong_key_and_resolver_lies_fail() {
    let (alex, bob, mallory) = (keypair(1), keypair(2), keypair(3));
    let p = signed(&alex, &bob);
    // the signature does not verify under another key
    let s = p.steps[0].meta.as_ref().unwrap().signatures[0].clone();
    assert!(matches!(
        verify_step_signature(&p.steps[0], &s, mallory.public_key()),
        Err(VerifyError::BadSignature(_))
    ));
    // a resolver that hands back a different key is caught by the fingerprint check
    let liar = |_: &Key| Some(mallory.public_key().clone());
    assert!(matches!(
        verify_path_signatures(&p, &["author"], false, &liar),
        Err(VerifyError::FingerprintMismatch { .. })
    ));
    // no key at all
    assert!(matches!(
        verify_path_signatures(&p, &["author"], false, &|_: &Key| None),
        Err(VerifyError::KeyUnavailable(_))
    ));
}

#[test]
fn reviewer_needs_its_timestamp() {
    let (alex, bob) = (keypair(1), keypair(2));
    let mut p = signed(&alex, &bob);
    for s in &mut p.meta.as_mut().unwrap().signatures {
        if s.scope == "reviewer" {
            s.timestamp = None;
        }
    }
    assert_eq!(
        verify_path_signatures(&p, &["reviewer"], false, &store(&[&alex, &bob])),
        Err(VerifyError::MissingTimestamp)
    );
}

#[test]
fn gpg_and_sigstore_are_not_verified() {
    let (alex, bob) = (keypair(1), keypair(2));
    let mut p = signed(&alex, &bob);
    for (scope_key, ty) in [("gpg", "gpg"), ("sigstore", "sigstore")] {
        let actors = p.meta.as_mut().unwrap().actors.as_mut().unwrap();
        actors.get_mut("human:alex").unwrap().keys = vec![Key {
            key_type: ty.into(),
            fingerprint: "ABCD1234".into(),
            href: None,
        }];
        let s = &mut p.meta.as_mut().unwrap().signatures[0];
        s.key = format!("{scope_key}:ABCD1234");
        assert_eq!(
            verify_path_signatures(&p, &["author"], false, &store(&[&alex])),
            Err(VerifyError::UnsupportedKeyType(ty.into()))
        );
    }
}

#[test]
fn scopes_without_a_canonical_form_are_refused() {
    let (alex, bob) = (keypair(1), keypair(2));
    let mut p = signed(&alex, &bob);
    let ci = sig("human:alex", &alex, "ci", None, &path_author_input(&p));
    p.meta.as_mut().unwrap().signatures.push(ci);
    assert_eq!(
        verify_path_signatures(&p, &["ci"], false, &store(&[&alex])),
        Err(VerifyError::UnsupportedScope("ci".into()))
    );
}

#[test]
fn tampered_step_signature_is_caught_with_verify_steps() {
    let (alex, bob) = (keypair(1), keypair(2));
    let mut p = signed(&alex, &bob);
    // The path author signature covers step ids, not step contents; the step's
    // own author signature is what catches a changed diff.
    p.steps[0].change.get_mut("src/main.rs").unwrap().raw = Some("@@ tampered".into());
    verify_path_signatures(&p, &["author", "reviewer"], false, &store(&[&alex, &bob])).unwrap();
    assert!(matches!(
        verify_path_signatures(&p, &["author", "reviewer"], true, &store(&[&alex, &bob])),
        Err(VerifyError::BadSignature(_))
    ));
}
