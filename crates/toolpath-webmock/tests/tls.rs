//! The replay server over TLS with a per-run throwaway CA (what codex needs: it speaks https to
//! its ChatGPT backend). The CA lives in memory and in the test's temp dir; it is never added to
//! any trust store. The client trusts only that CA.

use std::sync::Arc;

use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair, KeyUsagePurpose};
use toolpath_webmock::{Config, Mode, Store, serve_tls};

/// (CA certificate PEM, a server config for 127.0.0.1 signed by it)
pub fn throwaway_tls() -> (String, Arc<rustls::ServerConfig>) {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params
        .distinguished_name
        .push(DnType::CommonName, "webmock throwaway CA");
    let ca = ca_params.self_signed(&ca_key).unwrap();

    let leaf_key = KeyPair::generate().unwrap();
    let leaf_params =
        CertificateParams::new(vec!["127.0.0.1".to_string(), "localhost".into()]).unwrap();
    let ca_pem = ca.pem();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let leaf = leaf_params.signed_by(&leaf_key, &issuer).unwrap();

    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![leaf.der().clone()],
            rustls::pki_types::PrivateKeyDer::try_from(leaf_key.serialize_der()).unwrap(),
        )
        .unwrap();
    (ca_pem, Arc::new(cfg))
}

#[tokio::test]
async fn the_committed_pathbase_cassette_replays_over_tls_for_a_client_that_trusts_only_the_throwaway_ca()
 {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("cassettes/pathbase");
    let store = Arc::new(Store::open(&dir).unwrap());
    let (ca_pem, tls) = throwaway_tls();
    let (addr, handle) = serve_tls(
        Config {
            agent: "pathbase".into(),
            mode: Mode::Replay,
            store,
            log: Arc::new(|l: &str| eprintln!("[tls-test] {l}")),
            masks: Vec::new(),
        },
        0,
        Some(tls),
    )
    .await
    .unwrap();
    let url = "/api/v1/u/anon/repos/pathstash/graphs";
    let body = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<toolpath_webmock::Cassette>(&t).ok())
        .find(|c| c.request.url == url)
        .unwrap();
    // trusting only the throwaway CA
    let client = reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_pem(ca_pem.as_bytes()).unwrap()])
        .build()
        .unwrap();
    let r = client
        .post(format!("https://{addr}{url}"))
        .header("content-type", "application/json")
        .body(body.request.body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status().as_u16(), body.response.status);
    // a client with the default roots must REFUSE the same server (the CA is not trusted anywhere)
    let plain = reqwest::Client::new()
        .post(format!("https://{addr}{url}"))
        .body("{}")
        .send()
        .await;
    assert!(
        plain.is_err(),
        "the throwaway CA must not be trusted by default"
    );
    handle.abort();
}
