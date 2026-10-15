use toolpath_webmock::is_secret_header;

#[test]
fn provider_credential_headers_are_secret_by_name() {
    for h in [
        "authorization",
        "Authorization",
        "x-api-key",
        "x-goog-api-key",
        "api-key",
        "x-github-token",
        "x-opencode-key",
        "x-anything-secret",
        "cookie",
        "set-cookie",
        "chatgpt-account-id",
    ] {
        assert!(is_secret_header(h), "{h} must never reach a cassette");
    }
}

#[test]
fn ordinary_headers_are_kept() {
    for h in [
        "content-type",
        "accept",
        "x-opencode-session",
        "user-agent",
        "openai-beta",
    ] {
        assert!(!is_secret_header(h), "{h} is not a credential");
    }
}
