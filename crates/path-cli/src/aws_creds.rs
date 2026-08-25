//! Where S3 credentials actually come from.
//!
//! `object_store` resolves static keys, EKS/IRSA web identity, ECS task
//! roles, and EC2 instance metadata — the *server* cases. It reads no
//! `~/.aws/credentials`, no `~/.aws/config`, no `AWS_PROFILE`, and no
//! SSO, because it deliberately avoids depending on the AWS SDK. That
//! leaves out how nearly every developer actually has S3 access on a
//! laptop.
//!
//! This module fills that in without taking on the SDK:
//!
//! - **Static-key profiles** are read directly. `~/.aws/credentials` is
//!   a trivial ini file and the keys are right there.
//! - **Everything else** — SSO, `role_arn` chains, `credential_process`
//!   — is delegated to `aws configure export-credentials`, which runs
//!   the AWS CLI's own resolver and hands back concrete keys. Anyone
//!   using SSO already has the CLI, since `aws sso login` is how they
//!   authenticate; and delegating means refresh, cache layout, and
//!   every future profile type stay the CLI's problem, not ours.
//!
//! An expired SSO session is the one failure with an obvious next step,
//! so it gets one: we offer to run `aws sso login` and retry, rather
//! than making the user go do it and start the command over. Offer, not
//! do — that command opens a browser and waits, which shouldn't happen
//! because someone typed `path resume`. With no terminal to ask (CI),
//! it fails with the exact command instead of blocking on a prompt
//! nobody will answer.
//!
//! Depending on `aws-config` instead would be 31 crates, and its whole
//! family currently requires rustc 1.94.1 while this repo pins 1.94.0
//! — an MSRV treadmill on a toolchain we pin exactly.

#![cfg(not(target_os = "emscripten"))]

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Concrete keys, plus where they came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

/// Which resolution step supplied the credentials.
///
/// Carried all the way to `path auth s3 status`, because the first
/// question when a share fails is always *which* credential was used —
/// and "the one you configured" and "whatever the AWS CLI resolved" are
/// very different debugging stories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Source {
    /// Stored by `path auth s3 login`.
    Stored,
    /// `AWS_ACCESS_KEY_ID` and friends.
    Environment,
    /// Static keys in `~/.aws/credentials`.
    Profile { name: String, file: PathBuf },
    /// Resolved by the AWS CLI (SSO, assume-role, credential_process).
    AwsCli { name: String },
    /// Nothing local; `object_store` will try instance metadata, ECS,
    /// and web identity on its own.
    InstanceChain,
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Stored => f.write_str("stored by `path auth s3 login`"),
            Source::Environment => f.write_str("AWS_ACCESS_KEY_ID (environment)"),
            Source::Profile { name, file } => {
                write!(f, "profile `{name}` in {}", file.display())
            }
            Source::AwsCli { name } => {
                write!(f, "profile `{name}` via the AWS CLI")
            }
            Source::InstanceChain => {
                f.write_str("none found locally — will try the EC2/ECS/EKS credential chain")
            }
        }
    }
}

/// The outcome of resolution: keys when we found them, always a source.
#[derive(Debug, Clone)]
pub(crate) struct Resolved {
    pub credentials: Option<Credentials>,
    pub source: Source,
    /// Region the profile declares, if any — `~/.aws/config` is the
    /// natural place for it and re-typing it would be silly.
    pub region: Option<String>,
}

/// Everything resolution reads, injected so tests need no `$HOME`
/// games and no real AWS CLI.
pub(crate) struct Env<'a> {
    pub home: Option<PathBuf>,
    pub var: &'a dyn Fn(&str) -> Option<String>,
    /// Runs `aws configure export-credentials --profile <name>`,
    /// returning its stdout.
    pub aws_cli: &'a dyn Fn(&str) -> Result<String>,
    /// Runs `aws sso login --profile <name>`. Interactive: it opens a
    /// browser and waits.
    pub sso_login: &'a dyn Fn(&str) -> Result<()>,
    /// Asks the user a yes/no question. `false` when there's nobody to
    /// ask — a CI run must never block on a prompt.
    pub confirm: &'a dyn Fn(&str) -> bool,
}

impl Env<'_> {
    fn credentials_file(&self) -> Option<PathBuf> {
        let explicit = self
            .var("AWS_SHARED_CREDENTIALS_FILE")
            .filter(|v| !v.trim().is_empty());
        match explicit {
            Some(p) => Some(PathBuf::from(p)),
            None => self.home.as_ref().map(|h| h.join(".aws/credentials")),
        }
    }

    fn config_file(&self) -> Option<PathBuf> {
        let explicit = self.var("AWS_CONFIG_FILE").filter(|v| !v.trim().is_empty());
        match explicit {
            Some(p) => Some(PathBuf::from(p)),
            None => self.home.as_ref().map(|h| h.join(".aws/config")),
        }
    }

    fn var(&self, key: &str) -> Option<String> {
        (self.var)(key)
    }
}

/// Resolve S3 credentials, following AWS's own precedence with our
/// stored settings layered on top.
///
/// 1. `path auth s3 login` — an explicit local choice beats ambient
///    state, and it's the only way to configure a non-AWS endpoint.
/// 2. `--profile` / `AWS_PROFILE` — naming a profile is also explicit.
/// 3. `AWS_ACCESS_KEY_ID` — what CI sets.
/// 4. The `[default]` profile.
/// 5. Nothing: leave it to `object_store`'s instance chain.
pub(crate) fn resolve(
    stored: Option<Credentials>,
    profile_flag: Option<&str>,
    env: &Env<'_>,
) -> Result<Resolved> {
    if let Some(credentials) = stored {
        return Ok(Resolved {
            credentials: Some(credentials),
            source: Source::Stored,
            region: None,
        });
    }

    let named = profile_flag
        .map(str::to_string)
        .or_else(|| env.var("AWS_PROFILE"))
        .filter(|p| !p.trim().is_empty());

    if let Some(name) = &named {
        return from_profile(name, env).with_context(|| format!("profile `{name}`"));
    }

    if let (Some(key), Some(secret)) = (
        env.var("AWS_ACCESS_KEY_ID")
            .filter(|v| !v.trim().is_empty()),
        env.var("AWS_SECRET_ACCESS_KEY")
            .filter(|v| !v.trim().is_empty()),
    ) {
        return Ok(Resolved {
            credentials: Some(Credentials {
                access_key_id: key,
                secret_access_key: secret,
                session_token: env.var("AWS_SESSION_TOKEN").filter(|v| !v.is_empty()),
            }),
            source: Source::Environment,
            region: env
                .var("AWS_REGION")
                .or_else(|| env.var("AWS_DEFAULT_REGION")),
        });
    }

    // A `[default]` profile is only used when it exists; its absence is
    // not an error, it just means we fall through to the chain.
    if profile_exists("default", env) {
        return from_profile("default", env).context("profile `default`");
    }

    Ok(Resolved {
        credentials: None,
        source: Source::InstanceChain,
        region: None,
    })
}

fn profile_exists(name: &str, env: &Env<'_>) -> bool {
    let in_credentials = env
        .credentials_file()
        .and_then(|p| read_ini(&p).ok())
        .is_some_and(|ini| ini.contains_key(name));
    let in_config = env
        .config_file()
        .and_then(|p| read_ini(&p).ok())
        .is_some_and(|ini| ini.contains_key(&config_section(name)) || ini.contains_key(name));
    in_credentials || in_config
}

/// Read one profile: static keys if it has them, otherwise the AWS CLI.
fn from_profile(name: &str, env: &Env<'_>) -> Result<Resolved> {
    let credentials_path = env.credentials_file();
    let region = profile_region(name, env);

    if let Some(path) = &credentials_path
        && let Ok(ini) = read_ini(path)
        && let Some(section) = ini.get(name)
        && let (Some(key), Some(secret)) = (
            section.get("aws_access_key_id"),
            section.get("aws_secret_access_key"),
        )
    {
        return Ok(Resolved {
            credentials: Some(Credentials {
                access_key_id: key.clone(),
                secret_access_key: secret.clone(),
                session_token: section.get("aws_session_token").cloned(),
            }),
            source: Source::Profile {
                name: name.to_string(),
                file: path.clone(),
            },
            region,
        });
    }

    if !profile_exists(name, env) {
        bail!(
            "no such profile. Check `aws configure list-profiles`, or run \
             `path auth s3 login` to store keys directly."
        );
    }

    // The profile exists but carries no static keys: SSO, an assume-role
    // chain, or credential_process. The AWS CLI already knows how to
    // resolve all of those, including refresh.
    let raw = match (env.aws_cli)(name) {
        Ok(raw) => raw,
        // An expired SSO session is the one failure with an obvious
        // next step, and making the user go run it themselves and start
        // over is a pointless round trip — so offer to run it here.
        // Offer, not do: `aws sso login` opens a browser and waits, and
        // that shouldn't happen because someone typed `path resume`.
        Err(e) if is_expired_sso(&e) => {
            let cmd = format!("aws sso login --profile {name}");
            if !(env.confirm)(&format!(
                "The SSO session for profile `{name}` has expired. Run `{cmd}` now?"
            )) {
                bail!("the SSO session has expired.\n\nRun `{cmd}`, then try again.");
            }
            (env.sso_login)(name)?;
            // Exactly one retry. If a fresh login still doesn't yield
            // credentials, looping won't help and the real error is
            // whatever comes back now.
            (env.aws_cli)(name).context("after `aws sso login`")?
        }
        Err(e) => return Err(e),
    };
    let creds = parse_export_credentials(&raw)?;
    Ok(Resolved {
        credentials: Some(creds),
        source: Source::AwsCli {
            name: name.to_string(),
        },
        region,
    })
}

fn profile_region(name: &str, env: &Env<'_>) -> Option<String> {
    let ini = read_ini(&env.config_file()?).ok()?;
    // `~/.aws/config` spells non-default profiles `[profile name]`.
    let section = ini.get(&config_section(name)).or_else(|| ini.get(name))?;
    section.get("region").cloned()
}

fn config_section(name: &str) -> String {
    if name == "default" {
        name.to_string()
    } else {
        format!("profile {name}")
    }
}

/// `aws configure export-credentials --format process` output.
fn parse_export_credentials(raw: &str) -> Result<Credentials> {
    let v: serde_json::Value =
        serde_json::from_str(raw.trim()).context("the AWS CLI returned output that isn't JSON")?;
    let field = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let (Some(access_key_id), Some(secret_access_key)) =
        (field("AccessKeyId"), field("SecretAccessKey"))
    else {
        bail!("the AWS CLI returned no credentials");
    };
    Ok(Credentials {
        access_key_id,
        secret_access_key,
        session_token: field("SessionToken"),
    })
}

/// True when the AWS CLI failed because an SSO session needs renewing.
///
/// Matched on the message because the CLI reports it as a plain
/// non-zero exit, and its wording varies by version — "Token has
/// expired and refresh failed", "does not exist", and a direct
/// instruction to run `aws sso login` have all been observed. Requiring
/// `sso` alongside keeps unrelated failures out.
fn is_expired_sso(err: &anyhow::Error) -> bool {
    let msg = err.to_string().to_ascii_lowercase();
    msg.contains("sso")
        && (msg.contains("expired")
            || msg.contains("does not exist")
            || msg.contains("refresh failed")
            || msg.contains("sso login"))
}

/// Run `aws sso login`, inheriting stdio so its device code and browser
/// prompt reach the user. Kept behind [`Env::sso_login`] so tests don't.
pub(crate) fn run_sso_login(profile: &str) -> Result<()> {
    eprintln!("Running `aws sso login --profile {profile}`…");
    let status = std::process::Command::new("aws")
        .args(["sso", "login", "--profile", profile])
        .status()
        .context("running `aws sso login`")?;
    if !status.success() {
        bail!("`aws sso login --profile {profile}` did not complete");
    }
    Ok(())
}

/// Ask a yes/no question, defaulting to yes.
///
/// Returns `false` without asking when either stream isn't a terminal:
/// a CI run must fail with instructions rather than block forever on a
/// prompt nobody will answer. The question goes to stderr so stdout
/// stays clean for piping.
pub(crate) fn confirm_on_tty(question: &str) -> bool {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return false;
    }
    eprint!("{question} [Y/n] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    let answer = line.trim().to_ascii_lowercase();
    answer.is_empty() || answer == "y" || answer == "yes"
}

/// Run the real AWS CLI. Kept behind [`Env::aws_cli`] so tests don't.
pub(crate) fn run_aws_cli(profile: &str) -> Result<String> {
    let out = std::process::Command::new("aws")
        .args(["configure", "export-credentials", "--profile", profile])
        .args(["--format", "process"])
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => anyhow::anyhow!(
                "this profile needs the AWS CLI to resolve (SSO, assume-role, or \
                 credential_process), but `aws` isn't on PATH. Install it, or run \
                 `path auth s3 login` to store keys directly."
            ),
            _ => anyhow::anyhow!("running `aws configure export-credentials`: {e}"),
        })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!(
            "the AWS CLI could not resolve this profile: {}",
            stderr.trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ── Minimal ini ─────────────────────────────────────────────────────────

/// Parse the subset of ini that AWS config files actually use:
/// `[section]` headers, `key = value` pairs, `#`/`;` comments.
///
/// Nested sub-sections (the `[services]` style) are ignored rather than
/// mis-parsed — nothing we read lives in one.
fn read_ini(path: &Path) -> Result<HashMap<String, HashMap<String, String>>> {
    let text = std::fs::read_to_string(path)?;
    Ok(parse_ini(&text))
}

fn parse_ini(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        // Indented lines continue a nested sub-section; skip them.
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let name = name.trim().to_string();
            out.entry(name.clone()).or_default();
            current = Some(name);
            continue;
        }
        if let (Some(section), Some((key, value))) = (current.as_ref(), line.split_once('=')) {
            out.entry(section.clone())
                .or_default()
                .insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An `Env` over an in-memory fake `~/.aws` and a stub AWS CLI, so
    /// nothing here touches the developer's real credentials.
    struct Fake {
        dir: tempfile::TempDir,
        vars: HashMap<String, String>,
        cli_result: std::cell::RefCell<Result<String, String>>,
        cli_calls: std::cell::RefCell<Vec<String>>,
        /// What `aws_cli` returns on the *second* call, once a login has
        /// happened. `None` keeps returning `cli_result`.
        cli_after_login: std::cell::RefCell<Option<String>>,
        login_calls: std::cell::RefCell<Vec<String>>,
        login_fails: std::cell::RefCell<bool>,
        answer: std::cell::RefCell<bool>,
        questions: std::cell::RefCell<Vec<String>>,
    }

    impl Fake {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(dir.path().join(".aws")).unwrap();
            Fake {
                dir,
                vars: HashMap::new(),
                cli_result: std::cell::RefCell::new(Err("aws CLI not stubbed".into())),
                cli_calls: std::cell::RefCell::new(Vec::new()),
                cli_after_login: std::cell::RefCell::new(None),
                login_calls: std::cell::RefCell::new(Vec::new()),
                login_fails: std::cell::RefCell::new(false),
                answer: std::cell::RefCell::new(true),
                questions: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn credentials(self, body: &str) -> Self {
            std::fs::write(self.dir.path().join(".aws/credentials"), body).unwrap();
            self
        }
        fn config(self, body: &str) -> Self {
            std::fs::write(self.dir.path().join(".aws/config"), body).unwrap();
            self
        }
        fn var(mut self, k: &str, v: &str) -> Self {
            self.vars.insert(k.to_string(), v.to_string());
            self
        }
        fn cli(self, out: &str) -> Self {
            *self.cli_result.borrow_mut() = Ok(out.to_string());
            self
        }
        fn cli_err(self, msg: &str) -> Self {
            *self.cli_result.borrow_mut() = Err(msg.to_string());
            self
        }
        fn cli_after_login(self, out: &str) -> Self {
            *self.cli_after_login.borrow_mut() = Some(out.to_string());
            self
        }
        fn login_fails(self) -> Self {
            *self.login_fails.borrow_mut() = true;
            self
        }
        fn declines(self) -> Self {
            *self.answer.borrow_mut() = false;
            self
        }
        fn resolve(&self, stored: Option<Credentials>, profile: Option<&str>) -> Result<Resolved> {
            let var = |k: &str| self.vars.get(k).cloned();
            let cli = |name: &str| {
                self.cli_calls.borrow_mut().push(name.to_string());
                // After a login, return the post-login result if one was
                // staged — that's what a successful refresh looks like.
                if !self.login_calls.borrow().is_empty()
                    && let Some(out) = self.cli_after_login.borrow().clone()
                {
                    return Ok(out);
                }
                self.cli_result
                    .borrow()
                    .clone()
                    .map_err(|e| anyhow::anyhow!(e))
            };
            let login = |name: &str| {
                self.login_calls.borrow_mut().push(name.to_string());
                if *self.login_fails.borrow() {
                    anyhow::bail!("`aws sso login --profile {name}` did not complete");
                }
                Ok(())
            };
            let confirm = |q: &str| {
                self.questions.borrow_mut().push(q.to_string());
                *self.answer.borrow()
            };
            resolve(
                stored,
                profile,
                &Env {
                    home: Some(self.dir.path().to_path_buf()),
                    var: &var,
                    aws_cli: &cli,
                    sso_login: &login,
                    confirm: &confirm,
                },
            )
        }
    }

    const STATIC_PROFILE: &str = "\
[default]
aws_access_key_id = AKIADEFAULT
aws_secret_access_key = defaultsecret

[work]
aws_access_key_id = AKIAWORK
aws_secret_access_key = worksecret
aws_session_token = worktoken
";

    #[test]
    fn a_default_profile_is_used_with_no_configuration_at_all() {
        // The whole point: someone who has run `aws configure` gets S3
        // access without telling us anything.
        let f = Fake::new().credentials(STATIC_PROFILE);
        let r = f.resolve(None, None).unwrap();
        let c = r.credentials.unwrap();
        assert_eq!(c.access_key_id, "AKIADEFAULT");
        assert!(matches!(r.source, Source::Profile { .. }));
    }

    #[test]
    fn an_explicit_profile_beats_the_default() {
        let f = Fake::new().credentials(STATIC_PROFILE);
        let r = f.resolve(None, Some("work")).unwrap();
        let c = r.credentials.unwrap();
        assert_eq!(c.access_key_id, "AKIAWORK");
        assert_eq!(c.session_token.as_deref(), Some("worktoken"));
    }

    #[test]
    fn aws_profile_env_selects_the_profile() {
        let f = Fake::new()
            .credentials(STATIC_PROFILE)
            .var("AWS_PROFILE", "work");
        let r = f.resolve(None, None).unwrap();
        assert_eq!(r.credentials.unwrap().access_key_id, "AKIAWORK");
    }

    #[test]
    fn the_profile_flag_beats_aws_profile() {
        let f = Fake::new()
            .credentials(STATIC_PROFILE)
            .var("AWS_PROFILE", "work");
        let r = f.resolve(None, Some("default")).unwrap();
        assert_eq!(r.credentials.unwrap().access_key_id, "AKIADEFAULT");
    }

    #[test]
    fn stored_settings_beat_everything_ambient() {
        // Running `path auth s3 login` is an explicit local choice, and
        // it's the only way to reach a non-AWS endpoint.
        let f = Fake::new()
            .credentials(STATIC_PROFILE)
            .var("AWS_ACCESS_KEY_ID", "AKIAENV")
            .var("AWS_SECRET_ACCESS_KEY", "envsecret");
        let stored = Credentials {
            access_key_id: "AKIASTORED".into(),
            secret_access_key: "storedsecret".into(),
            session_token: None,
        };
        let r = f.resolve(Some(stored), None).unwrap();
        assert_eq!(r.credentials.unwrap().access_key_id, "AKIASTORED");
        assert_eq!(r.source, Source::Stored);
    }

    #[test]
    fn env_keys_beat_the_default_profile() {
        // AWS's own precedence, and what CI sets.
        let f = Fake::new()
            .credentials(STATIC_PROFILE)
            .var("AWS_ACCESS_KEY_ID", "AKIAENV")
            .var("AWS_SECRET_ACCESS_KEY", "envsecret");
        let r = f.resolve(None, None).unwrap();
        assert_eq!(r.credentials.unwrap().access_key_id, "AKIAENV");
        assert_eq!(r.source, Source::Environment);
    }

    #[test]
    fn nothing_configured_defers_to_the_instance_chain() {
        // On a server this is the correct answer, not a failure.
        let f = Fake::new();
        let r = f.resolve(None, None).unwrap();
        assert!(r.credentials.is_none());
        assert_eq!(r.source, Source::InstanceChain);
    }

    #[test]
    fn an_sso_profile_is_resolved_through_the_aws_cli() {
        // No static keys in the file — exactly what `aws sso login`
        // leaves behind. The CLI knows how to turn it into keys.
        let f = Fake::new()
            .config("[profile sso-work]\nsso_session = corp\nsso_account_id = 1234\n")
            .cli(r#"{"Version":1,"AccessKeyId":"ASIASSO","SecretAccessKey":"ssosecret","SessionToken":"ssotoken"}"#);
        let r = f.resolve(None, Some("sso-work")).unwrap();
        let c = r.credentials.unwrap();
        assert_eq!(c.access_key_id, "ASIASSO");
        assert_eq!(c.session_token.as_deref(), Some("ssotoken"));
        assert_eq!(
            r.source,
            Source::AwsCli {
                name: "sso-work".into()
            }
        );
        assert_eq!(*f.cli_calls.borrow(), vec!["sso-work".to_string()]);
    }

    #[test]
    fn a_static_profile_never_shells_out() {
        let f = Fake::new().credentials(STATIC_PROFILE);
        f.resolve(None, Some("work")).unwrap();
        assert!(
            f.cli_calls.borrow().is_empty(),
            "static keys are right there; spawning the AWS CLI would be silly"
        );
    }

    #[test]
    fn an_unknown_profile_says_so_rather_than_shelling_out() {
        let f = Fake::new().credentials(STATIC_PROFILE);
        let err = f.resolve(None, Some("nope")).unwrap_err().to_string();
        assert!(err.contains("nope"), "{err}");
        assert!(f.cli_calls.borrow().is_empty());
    }

    #[test]
    fn region_comes_from_the_profile_when_we_have_none() {
        // `~/.aws/config` spells non-default profiles `[profile name]`.
        let f = Fake::new()
            .credentials(STATIC_PROFILE)
            .config("[default]\nregion = us-east-2\n\n[profile work]\nregion = eu-west-1\n");
        assert_eq!(
            f.resolve(None, Some("work")).unwrap().region.as_deref(),
            Some("eu-west-1")
        );
        assert_eq!(
            f.resolve(None, None).unwrap().region.as_deref(),
            Some("us-east-2")
        );
    }

    #[test]
    fn credentials_file_location_honors_the_aws_env_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("elsewhere");
        std::fs::write(&path, STATIC_PROFILE).unwrap();
        let f = Fake::new().var("AWS_SHARED_CREDENTIALS_FILE", path.to_str().unwrap());
        assert_eq!(
            f.resolve(None, None)
                .unwrap()
                .credentials
                .unwrap()
                .access_key_id,
            "AKIADEFAULT"
        );
    }

    // ── expired SSO ──────────────────────────────────────────────────

    /// What the AWS CLI actually prints when an SSO session lapses.
    const EXPIRED: &str = "the AWS CLI could not resolve this profile: \
Error loading SSO Token: Token for https://corp.awsapps.com/start does not exist";

    const FRESH: &str =
        r#"{"Version":1,"AccessKeyId":"ASIAFRESH","SecretAccessKey":"s","SessionToken":"t"}"#;

    fn sso_profile() -> Fake {
        Fake::new().config("[profile sso-work]\nsso_session = corp\n")
    }

    #[test]
    fn an_expired_sso_session_offers_to_log_in_and_retries_once() {
        let f = sso_profile().cli_err(EXPIRED).cli_after_login(FRESH);
        let r = f.resolve(None, Some("sso-work")).unwrap();

        assert_eq!(r.credentials.unwrap().access_key_id, "ASIAFRESH");
        assert_eq!(*f.login_calls.borrow(), vec!["sso-work".to_string()]);
        // Asked before opening a browser, and named the command.
        let q = f.questions.borrow().join("");
        assert!(q.contains("aws sso login --profile sso-work"), "{q}");
        // One retry, not a loop.
        assert_eq!(f.cli_calls.borrow().len(), 2);
    }

    #[test]
    fn declining_the_offer_prints_the_command_and_logs_in_to_nothing() {
        let f = sso_profile().cli_err(EXPIRED).declines();
        let err = format!("{:#}", f.resolve(None, Some("sso-work")).unwrap_err());

        assert!(err.contains("aws sso login --profile sso-work"), "{err}");
        assert!(
            f.login_calls.borrow().is_empty(),
            "declining must not open a browser"
        );
    }

    #[test]
    fn a_failed_login_surfaces_rather_than_retrying_blindly() {
        let f = sso_profile().cli_err(EXPIRED).login_fails();
        let err = format!("{:#}", f.resolve(None, Some("sso-work")).unwrap_err());
        assert!(err.contains("did not complete"), "{err}");
        assert_eq!(
            f.cli_calls.borrow().len(),
            1,
            "no retry after a failed login"
        );
    }

    #[test]
    fn a_login_that_still_yields_nothing_reports_the_second_failure() {
        // Logged in, still broken: the real error is whatever comes back
        // now, and looping wouldn't help.
        let f = sso_profile().cli_err(EXPIRED);
        let err = format!("{:#}", f.resolve(None, Some("sso-work")).unwrap_err());
        assert!(err.contains("after `aws sso login`"), "{err}");
        assert_eq!(f.cli_calls.borrow().len(), 2);
    }

    #[test]
    fn an_unrelated_cli_failure_never_offers_a_login() {
        // Only an expired *SSO session* has this obvious next step.
        let f = sso_profile().cli_err(
            "the AWS CLI could not resolve this profile: \
Unable to locate credentials for role arn:aws:iam::1:role/nope",
        );
        let err = format!("{:#}", f.resolve(None, Some("sso-work")).unwrap_err());
        assert!(err.contains("Unable to locate credentials"), "{err}");
        assert!(f.login_calls.borrow().is_empty());
        assert!(f.questions.borrow().is_empty(), "nothing to ask about");
    }

    #[test]
    fn a_working_sso_profile_is_not_asked_about() {
        let f = sso_profile().cli(FRESH);
        f.resolve(None, Some("sso-work")).unwrap();
        assert!(f.questions.borrow().is_empty());
        assert!(f.login_calls.borrow().is_empty());
    }

    #[test]
    fn expired_sso_detection_covers_the_wordings_the_cli_uses() {
        for msg in [
            "Error loading SSO Token: Token for https://x/start does not exist",
            "The SSO session associated with this profile has expired",
            "Error when retrieving token from sso: Token has expired and refresh failed",
            "To refresh this SSO session run aws sso login with the corresponding profile",
        ] {
            assert!(is_expired_sso(&anyhow::anyhow!("{msg}")), "missed: {msg}");
        }
        // And doesn't fire on failures with a different fix.
        for msg in [
            "Unable to locate credentials",
            "An error occurred (AccessDenied) when calling AssumeRole",
            "Could not connect to the endpoint URL",
        ] {
            assert!(
                !is_expired_sso(&anyhow::anyhow!("{msg}")),
                "false positive: {msg}"
            );
        }
    }

    // ── ini parsing ──────────────────────────────────────────────────

    #[test]
    fn ini_ignores_comments_and_nested_subsections() {
        let ini = parse_ini(
            "\
# a comment
; another
[default]
aws_access_key_id = AKIA   ; trailing content is part of the value
region=us-east-1

[services x]
  s3 =
    endpoint_url = http://nested
",
        );
        assert_eq!(ini["default"]["region"], "us-east-1");
        // Indented sub-section bodies must not leak into the section.
        assert!(!ini["services x"].contains_key("endpoint_url"));
    }

    #[test]
    fn ini_keys_are_case_insensitive() {
        let ini = parse_ini("[default]\nAWS_ACCESS_KEY_ID = AKIA\n");
        assert_eq!(ini["default"]["aws_access_key_id"], "AKIA");
    }

    #[test]
    fn export_credentials_output_without_keys_is_an_error() {
        let err = parse_export_credentials(r#"{"Version":1}"#)
            .unwrap_err()
            .to_string();
        assert!(err.contains("no credentials"), "{err}");
    }

    #[test]
    fn export_credentials_non_json_is_an_error() {
        let err = parse_export_credentials("Unable to locate credentials")
            .unwrap_err()
            .to_string();
        assert!(err.contains("isn't JSON"), "{err}");
    }
}
