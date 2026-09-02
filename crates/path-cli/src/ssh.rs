//! ssh transport for commands that act on a remote host.
//!
//! The transport is an in-process SSH client (`russh`); no `ssh` binary
//! is involved. The destination is `user@host`; the port is 22. The
//! caller passes the agent socket and the ssh directory; the module
//! reads no environment variable. The agent authenticates first, then
//! the default identity files in the ssh directory. `known_hosts` in
//! that directory verifies the host key: a changed key is an error,
//! and an unknown host is learned on first contact with a notice, as
//! `StrictHostKeyChecking=accept-new` does.
//!
//! [`Transport::run`] captures the output of an exec channel.
//!
//! A remote command is one string by the protocol: the exec request
//! carries it and the remote login shell parses it. [`RemoteCommand`]
//! is the only way to build that string. Its two forms are an argv, and
//! a constant `sh` script that reads its values as positional
//! parameters. Every value passes through shell quoting; no caller
//! interpolates into shell text.
//!
//! The remote login shell must be POSIX-compatible. A value that
//! contains a single quote renders double-quoted with backslash
//! escapes; csh and fish parse those escapes differently.

use anyhow::{Context, Result, anyhow, bail};
use russh::client;
use russh::keys::known_hosts::{check_known_hosts_path, learn_known_hosts_path};
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{ChannelMsg, Disconnect};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use std::time::Duration;

/// An ssh destination: `user@host`. The port is 22; `~/.ssh/config`
/// is not read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    user: String,
    host: String,
}

impl Destination {
    /// `user@host`, both non-empty. Each side starts with an
    /// alphanumeric character (so the value cannot be an option) and
    /// continues with `[A-Za-z0-9._-]`. Also usable as a clap
    /// `value_parser`, hence the `String` error type.
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        let word = |part: &str| {
            let mut chars = part.chars();
            chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
                && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        };
        match s.split_once('@') {
            Some((user, host)) if word(user) && word(host) => Ok(Self {
                user: user.to_string(),
                host: host.to_string(),
            }),
            _ => Err(format!(
                "expected an ssh destination of the form user@host (letters, \
                 digits, and `._-` on each side, alphanumeric first); IPv6 \
                 addresses and ports are not accepted, got {s:?}"
            )),
        }
    }
}

impl fmt::Display for Destination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.user, self.host)
    }
}

/// A command for the remote login shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteCommand {
    words: Vec<String>,
}

impl RemoteCommand {
    /// An argv: `program` followed by the values given to [`Self::arg`].
    #[allow(dead_code)]
    pub(crate) fn new(program: &str) -> Self {
        Self {
            words: vec![program.to_string()],
        }
    }

    /// A constant `sh` script. The values given to [`Self::arg`] reach
    /// it as `$1`, `$2`, and so on. The text must not contain a single
    /// quote, so shlex renders it as one single-quoted word (a test
    /// pins the rendering), which the login shell passes to `sh`
    /// verbatim.
    pub(crate) fn script(text: &'static str) -> Self {
        assert!(
            !text.contains('\''),
            "a remote script must not contain a single quote"
        );
        Self {
            words: vec!["sh".into(), "-c".into(), text.into(), "sh".into()],
        }
    }

    pub(crate) fn arg(mut self, value: impl Into<String>) -> Self {
        self.words.push(value.into());
        self
    }

    pub(crate) fn args(self, values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        values
            .into_iter()
            .fold(self, |command, value| command.arg(value))
    }

    /// The string the exec request carries: every word quoted for a
    /// POSIX shell.
    pub(crate) fn render(&self) -> Result<String> {
        let words: Vec<String> = self.words.iter().map(|w| quote(w)).collect::<Result<_>>()?;
        Ok(words.join(" "))
    }
}

pub(crate) trait Transport {
    /// Run `command` on `dest` without a terminal. stdin is fed from
    /// `input` or closed; stdout and stderr are captured. The call
    /// returns when the remote command exits; a command that exits
    /// before it reads all of `input` yields its status and stderr.
    /// When the command outlives `timeout`, the connection is dropped
    /// and the call errors with the stderr received so far; sshd hangs
    /// up the remote session, and a detached remote process survives.
    fn run(
        &self,
        dest: &Destination,
        command: &RemoteCommand,
        input: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<Output>;
}

/// Bound on the TCP connect, the handshake, and authentication
/// together. Separate from the caller's per-command timeout, which
/// bounds the command alone.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// Bound on the disconnect, so a stalled peer does not hold the caller
/// after its command is done.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Keepalives on every session, so a peer that stops answering is
/// noticed within a minute.
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const KEEPALIVE_MAX: usize = 3;

/// The ssh port; a [`Destination`] carries none.
const PORT: u16 = 22;

/// The in-process SSH client. Each call opens one connection.
pub(crate) struct Russh {
    runtime: tokio::runtime::Runtime,
    agent_socket: Option<PathBuf>,
    ssh_dir: PathBuf,
}

impl Russh {
    /// `agent_socket` is the ssh agent's socket, when there is one.
    /// `ssh_dir` holds the identity files and `known_hosts`.
    pub(crate) fn new(agent_socket: Option<PathBuf>, ssh_dir: PathBuf) -> Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("start the async runtime")?;
        Ok(Self {
            runtime,
            agent_socket,
            ssh_dir,
        })
    }
}

impl Transport for Russh {
    fn run(
        &self,
        dest: &Destination,
        command: &RemoteCommand,
        input: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<Output> {
        let command = command.render()?;
        self.runtime.block_on(async {
            let session = connect(dest, self.agent_socket.as_deref(), &self.ssh_dir).await?;
            let result = exec_captured(&session, &command, input, timeout).await;
            session.close().await;
            result
        })
    }
}

/// Verifies the server's host key against `known_hosts`.
struct HostKeyCheck {
    host: String,
    known_hosts: PathBuf,
}

impl client::Handler for HostKeyCheck {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = server_key else {
            bail!(
                "{}:{PORT} presented a host certificate; only plain host keys are accepted",
                self.host
            );
        };
        match check_known_hosts_path(&self.host, PORT, key, &self.known_hosts) {
            Ok(true) => Ok(true),
            Ok(false) => {
                learn_known_hosts_path(&self.host, PORT, key, &self.known_hosts).with_context(
                    || {
                        format!(
                            "record the host key of {}:{PORT} in {}",
                            self.host,
                            self.known_hosts.display()
                        )
                    },
                )?;
                eprintln!(
                    "Learned the host key of {}:{PORT} ({} {})",
                    self.host,
                    key.algorithm(),
                    key.fingerprint(HashAlg::Sha256)
                );
                Ok(true)
            }
            Err(russh::keys::Error::KeyChanged { line }) => bail!(
                "the host key of {}:{PORT} does not match {} line {line}; \
                 refusing to connect",
                self.host,
                self.known_hosts.display()
            ),
            Err(e) => Err(e).with_context(|| format!("read {}", self.known_hosts.display())),
        }
    }
}

struct Session {
    handle: client::Handle<HostKeyCheck>,
}

impl Session {
    /// Sends the DISCONNECT and waits for the session task to end,
    /// within [`CLOSE_TIMEOUT`]. `disconnect` only queues the message;
    /// the task writes it, and the runtime polls the task only while
    /// this future runs.
    async fn close(self) {
        let handle = self.handle;
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, async {
            let _ = handle
                .disconnect(Disconnect::ByApplication, "", "English")
                .await;
            let _ = handle.await;
        })
        .await;
    }
}

/// Connect, verify the host key, and authenticate, within
/// [`CONNECT_TIMEOUT`].
async fn connect(
    dest: &Destination,
    agent_socket: Option<&Path>,
    ssh_dir: &Path,
) -> Result<Session> {
    let connected = async {
        let client_config = Arc::new(client::Config {
            keepalive_interval: Some(KEEPALIVE_INTERVAL),
            keepalive_max: KEEPALIVE_MAX,
            ..Default::default()
        });
        let handler = HostKeyCheck {
            host: dest.host.clone(),
            known_hosts: ssh_dir.join("known_hosts"),
        };
        let mut handle = client::connect(client_config, (dest.host.as_str(), PORT), handler)
            .await
            .with_context(|| format!("connect to {}:{PORT}", dest.host))?;
        authenticate(&mut handle, &dest.user, &dest.host, agent_socket, ssh_dir).await?;
        Ok::<_, anyhow::Error>(Session { handle })
    };
    match tokio::time::timeout(CONNECT_TIMEOUT, connected).await {
        Ok(result) => result,
        Err(_) => bail!(
            "connecting to {dest} did not finish within {}s",
            CONNECT_TIMEOUT.as_secs()
        ),
    }
}

/// The identities of the agent at `agent_socket` first, then the
/// identity files in `ssh_dir`. A failure on one identity is recorded
/// and the next one is tried; the error lists what was tried.
async fn authenticate(
    handle: &mut client::Handle<HostKeyCheck>,
    user: &str,
    host: &str,
    agent_socket: Option<&Path>,
    ssh_dir: &Path,
) -> Result<()> {
    let rsa_hash = handle
        .best_supported_rsa_hash()
        .await
        .context("negotiate signature algorithms")?
        .flatten();
    let mut tried: Vec<String> = Vec::new();

    #[cfg(unix)]
    match agent_socket {
        Some(socket) => match russh::keys::agent::client::AgentClient::connect_uds(socket).await {
            Ok(mut agent) => match agent.request_identities().await {
                Ok(identities) => {
                    for identity in identities {
                        let russh::keys::agent::AgentIdentity::PublicKey { key, comment } =
                            identity
                        else {
                            continue;
                        };
                        let result = match handle
                            .authenticate_publickey_with(user, key, rsa_hash, &mut agent)
                            .await
                        {
                            Ok(result) => result,
                            Err(e) => {
                                tried.push(format!("agent key {comment:?} ({e})"));
                                continue;
                            }
                        };
                        if result.success() {
                            return Ok(());
                        }
                        tried.push(format!("agent key {comment:?}"));
                    }
                }
                Err(e) => tried.push(format!("agent identities ({e})")),
            },
            Err(e) => tried.push(format!("agent at {} ({e})", socket.display())),
        },
        None => tried.push("agent (no socket)".to_string()),
    }
    #[cfg(not(unix))]
    let _ = agent_socket;

    for path in identity_file_candidates(ssh_dir) {
        if !path.is_file() {
            continue;
        }
        let key = match russh::keys::load_secret_key(&path, None) {
            Ok(key) => key,
            Err(russh::keys::Error::KeyIsEncrypted) => {
                tried.push(format!(
                    "{} (encrypted; add it to the agent)",
                    path.display()
                ));
                continue;
            }
            Err(e) => {
                tried.push(format!("{} ({e})", path.display()));
                continue;
            }
        };
        let result = match handle
            .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), rsa_hash))
            .await
        {
            Ok(result) => result,
            Err(e) => {
                tried.push(format!("{} ({e})", path.display()));
                continue;
            }
        };
        if result.success() {
            return Ok(());
        }
        tried.push(path.display().to_string());
    }

    let tried = if tried.is_empty() {
        "nothing: no agent and no identity file".to_string()
    } else {
        tried.join(", ")
    };
    bail!("authentication as {user}@{host} failed; tried {tried}")
}

/// The OpenSSH default identity files in `ssh_dir`.
fn identity_file_candidates(ssh_dir: &Path) -> Vec<PathBuf> {
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|name| ssh_dir.join(name))
        .collect()
}

/// One exec channel: feed `input`, collect stdout and stderr, and take
/// the exit status. `timeout` bounds the whole call, from the channel
/// open to the channel's close. The close ends the call, so a remote
/// that exits without reading all of `input` does not leave the feed
/// waiting on window space until `timeout`. A refused exec request
/// errors at once; sshd keeps the channel open after a refusal.
async fn exec_captured(
    session: &Session,
    command: &str,
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<Output> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut status = None;
    let work = async {
        let channel = session
            .handle
            .channel_open_session()
            .await
            .context("open a session channel")?;
        channel
            .exec(true, command)
            .await
            .context("send the exec request")?;
        let (mut reader, writer) = channel.split();
        let feed = async {
            if let Some(bytes) = input {
                writer.data(bytes).await.context("send stdin")?;
            }
            writer.eof().await.context("close stdin")
        };
        let collect = async {
            while let Some(msg) = reader.wait().await {
                match msg {
                    ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                    ChannelMsg::ExtendedData { data, ext: 1 } => stderr.extend_from_slice(&data),
                    ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                    ChannelMsg::ExitSignal { signal_name, .. } => {
                        status = Some(255);
                        stderr.extend_from_slice(
                            format!("\nterminated by signal {signal_name:?}").as_bytes(),
                        );
                    }
                    ChannelMsg::Failure => bail!("the remote refused the exec request"),
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
            Ok::<_, anyhow::Error>(())
        };
        let (collected, fed) = {
            tokio::pin!(feed);
            tokio::pin!(collect);
            let mut fed: Option<Result<()>> = None;
            loop {
                tokio::select! {
                    result = &mut feed, if fed.is_none() => fed = Some(result),
                    result = &mut collect => break (result, fed),
                }
            }
        };
        if collected.is_err() {
            let _ = writer.close().await;
        }
        collected.map(|()| fed)
    };
    let fed = match tokio::time::timeout(timeout, work).await {
        Ok(result) => result?,
        Err(_) => bail!(
            "remote command did not finish within {}s; \
             the connection may be fine while the command hangs{}",
            timeout.as_secs(),
            stderr_note(&stderr)
        ),
    };
    if status.is_none() && session.handle.is_closed() {
        bail!(
            "the connection closed before the remote reported an exit status{}",
            stderr_note(&stderr)
        );
    }
    if status.is_none()
        && let Some(Err(e)) = fed
    {
        return Err(e);
    }
    Ok(Output {
        status: exit_status(status.unwrap_or(255)),
        stdout,
        stderr,
    })
}

/// The last 1000 characters of `stderr` behind a `(stderr)` marker,
/// or nothing when it is empty.
fn stderr_note(stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim_end();
    if stderr.is_empty() {
        String::new()
    } else {
        format!("\n(stderr) {}", tail(stderr, 1000))
    }
}

#[cfg(unix)]
fn exit_status(code: u32) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw((code as i32 & 0xff) << 8)
}

#[cfg(windows)]
fn exit_status(code: u32) -> std::process::ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(code)
}

/// The last `n` characters of `s`.
fn tail(s: &str, n: usize) -> &str {
    let start = s
        .char_indices()
        .rev()
        .nth(n.saturating_sub(1))
        .map_or(0, |(i, _)| i);
    &s[start..]
}

/// POSIX shell quoting for one word of a remote command.
fn quote(s: &str) -> Result<String> {
    shlex::try_quote(s)
        .map(|c| c.into_owned())
        .map_err(|_| anyhow!("cannot quote a value that contains a NUL byte: {s:?}"))
}

/// Error unless `output` reports success. The message names `what`,
/// the destination, the exit status, and the remote stderr.
pub(crate) fn fail_unless_success(output: &Output, what: &str, dest: &Destination) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim_end();
    if stderr.is_empty() {
        bail!("{what} on {dest} failed ({})", output.status);
    }
    bail!("{what} on {dest} failed ({}):\n{stderr}", output.status);
}

/// Parse `<tag>=<value>` lines from stdout: exactly one line per tag,
/// in order. Any other shape (a login banner, a notice, a partial
/// run) errors with stdout, then stderr behind a `(stderr)` marker.
pub(crate) fn parse_facts<const N: usize>(output: &Output, tags: [&str; N]) -> Result<[String; N]> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    let values: Vec<&str> = lines
        .iter()
        .zip(tags)
        .filter_map(|(line, tag)| line.strip_prefix(tag)?.strip_prefix('='))
        .collect();
    if lines.len() != N || values.len() != N {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let mut shown = stdout.trim_end().to_string();
        if !stderr.trim().is_empty() {
            shown.push_str("\n(stderr) ");
            shown.push_str(stderr.trim_end());
        }
        bail!(
            "unexpected output from the remote (a login banner or notice?); output was:\n{shown}"
        );
    }
    Ok(std::array::from_fn(|i| values[i].to_string()))
}

/// Scripted transport for tests: `reply` queues one `run` result;
/// every call is recorded with its rendered command.
#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub(crate) struct Call {
        pub(crate) dest: String,
        pub(crate) command: String,
        pub(crate) input: Option<Vec<u8>>,
    }

    #[derive(Default)]
    pub(crate) struct FakeSsh {
        replies: Mutex<VecDeque<Output>>,
        calls: Mutex<Vec<Call>>,
    }

    pub(crate) fn output(status: u32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: exit_status(status),
            stdout: stdout.as_bytes().to_vec(),
            stderr: stderr.as_bytes().to_vec(),
        }
    }

    impl FakeSsh {
        pub(crate) fn new() -> Self {
            Self::default()
        }

        pub(crate) fn reply(&self, status: u32, stdout: &str) -> &Self {
            self.reply_with_stderr(status, stdout, "")
        }

        pub(crate) fn reply_with_stderr(&self, status: u32, stdout: &str, stderr: &str) -> &Self {
            self.replies
                .lock()
                .unwrap()
                .push_back(output(status, stdout, stderr));
            self
        }

        pub(crate) fn calls(&self) -> Vec<Call> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Transport for FakeSsh {
        fn run(
            &self,
            dest: &Destination,
            command: &RemoteCommand,
            input: Option<&[u8]>,
            _timeout: Duration,
        ) -> Result<Output> {
            let command = command.render()?;
            self.calls.lock().unwrap().push(Call {
                dest: dest.to_string(),
                command: command.clone(),
                input: input.map(|b| b.to_vec()),
            });
            let reply = self.replies.lock().unwrap().pop_front();
            Ok(reply.unwrap_or_else(|| panic!("FakeSsh: no scripted reply for {command:?}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NASTY: &str = "$(rm -rf ~); `x`; $HOME 'quoted' \"double\" \\ * ? ; & | > <";

    fn dest() -> Destination {
        Destination::parse("user@host").unwrap()
    }

    #[cfg(unix)]
    fn sh(command: &str) -> String {
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    #[test]
    fn destination_accepts_user_at_host() {
        for s in [
            "user@host",
            "exedev@vm.exe.xyz",
            "a-b_c.d@e-f_g.h",
            "u1@10.0.0.1",
        ] {
            assert_eq!(Destination::parse(s).unwrap().to_string(), s);
        }
    }

    #[test]
    fn destination_rejects_missing_parts_option_shaped_and_shell_characters() {
        for s in [
            "",
            "host",
            "user@",
            "@host",
            "-oProxyCommand=x",
            "-",
            "-u@host",
            "user@-host",
            "user@host;ls",
            "a@b@c",
            "a b",
            "user@host$",
            "u@[::1]",
            "ssh://u@h",
        ] {
            assert!(Destination::parse(s).is_err(), "{s:?} must be rejected");
        }
    }

    #[test]
    fn destination_splits_user_and_host() {
        let d = Destination::parse("exedev@vm.exe.xyz").unwrap();
        assert_eq!(d.user, "exedev");
        assert_eq!(d.host, "vm.exe.xyz");
    }

    #[test]
    fn quote_leaves_plain_words_and_quotes_the_rest() {
        assert_eq!(quote("/a/b").unwrap(), "/a/b");
        assert_eq!(quote("=path-abc").unwrap(), "'=path-abc'");
        assert_eq!(quote("").unwrap(), "''");
        assert_eq!(quote("a b").unwrap(), "'a b'");
        assert_eq!(quote("a'b").unwrap(), "\"a'b\"");
        assert!(quote("a\0b").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn quote_round_trips_through_sh() {
        assert_eq!(sh(&format!("printf %s {}", quote(NASTY).unwrap())), NASTY);
    }

    #[test]
    fn remote_command_renders_program_then_quoted_args() {
        let cmd = RemoteCommand::new("tmux")
            .arg("new-session")
            .arg("-c")
            .arg("/it's here")
            .arg("");
        assert_eq!(
            cmd.render().unwrap(),
            "tmux new-session -c \"/it's here\" ''"
        );
    }

    #[test]
    fn script_renders_sh_dash_c_then_positional_args() {
        let cmd = RemoteCommand::script("cd \"$1\" && pwd -P").arg("/a b");
        assert_eq!(
            cmd.render().unwrap(),
            "sh -c 'cd \"$1\" && pwd -P' sh '/a b'"
        );
    }

    #[test]
    #[should_panic(expected = "single quote")]
    fn script_rejects_a_single_quote() {
        RemoteCommand::script("printf '%s'");
    }

    #[test]
    fn render_rejects_a_nul_byte() {
        assert!(RemoteCommand::new("x").arg("a\0b").render().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn remote_command_round_trips_hostile_args_through_sh() {
        let argv = RemoteCommand::new("printf")
            .arg("%s\\n")
            .arg(NASTY)
            .arg("$HOME");
        assert_eq!(sh(&argv.render().unwrap()), format!("{NASTY}\n$HOME\n"));

        let script = RemoteCommand::script("printf \"%s\\n\" \"$1\" \"$2\"")
            .arg(NASTY)
            .arg("$HOME");
        assert_eq!(sh(&script.render().unwrap()), format!("{NASTY}\n$HOME\n"));
    }

    #[test]
    fn exit_status_maps_the_remote_code() {
        assert!(exit_status(0).success());
        assert_eq!(exit_status(3).code(), Some(3));
        assert_eq!(exit_status(255).code(), Some(255));
    }

    #[test]
    fn tail_keeps_the_last_n_characters() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("ab", 3), "ab");
        assert_eq!(tail("", 3), "");
        assert_eq!(tail("héllo", 2), "lo");
    }

    /// Needs a reachable host with agent or key auth:
    /// `PATH_TEST_SSH_DEST=user@host cargo test -p path-cli -- --ignored live_`.
    #[test]
    #[ignore = "connects to $PATH_TEST_SSH_DEST"]
    fn live_run_captures_feeds_stdin_times_out_and_maps_the_status() {
        let Ok(dest) = std::env::var("PATH_TEST_SSH_DEST") else {
            return;
        };
        let dest = Destination::parse(&dest).unwrap();
        let ssh = Russh::new(
            std::env::var_os("SSH_AUTH_SOCK").map(PathBuf::from),
            crate::config::home_dir().unwrap().join(".ssh"),
        )
        .unwrap();

        let script =
            RemoteCommand::script("printf \"TP_A=%s\\n\" \"$1\"; printf err >&2; cat").arg("x y");
        let out = ssh
            .run(&dest, &script, Some(b"fed"), Duration::from_secs(30))
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "TP_A=x y\nfed");
        assert_eq!(String::from_utf8_lossy(&out.stderr), "err");

        let out = ssh
            .run(
                &dest,
                &RemoteCommand::new("sh").arg("-c").arg("exit 3"),
                None,
                Duration::from_secs(30),
            )
            .unwrap();
        assert_eq!(out.status.code(), Some(3));

        // stdin larger than the initial channel window (2 MiB on
        // OpenSSH) and a remote that exits without reading it.
        let unread = vec![b'x'; 4 << 20];
        let out = ssh
            .run(
                &dest,
                &RemoteCommand::script("echo unread >&2; exit 4"),
                Some(&unread),
                Duration::from_secs(30),
            )
            .unwrap();
        assert_eq!(out.status.code(), Some(4));
        assert_eq!(String::from_utf8_lossy(&out.stderr), "unread\n");

        let err = ssh
            .run(
                &dest,
                &RemoteCommand::script("echo hanging on a lock >&2; sleep 30"),
                None,
                Duration::from_millis(1500),
            )
            .unwrap_err();
        assert!(err.to_string().contains("did not finish within"), "{err:#}");
        assert!(
            err.to_string().ends_with("(stderr) hanging on a lock"),
            "{err:#}"
        );
    }

    mod with_fake_output {
        use super::super::fake::{Call, FakeSsh, output};
        use super::*;

        #[test]
        fn parse_facts_reads_values_in_order() {
            let out = output(0, "A=1\nB=\nC=/x y\n", "");
            assert_eq!(
                parse_facts(&out, ["A", "B", "C"]).unwrap(),
                ["1", "", "/x y"]
            );
        }

        #[test]
        fn parse_facts_rejects_banner_missing_and_extra_lines() {
            let banner = output(0, "Welcome!\nA=1\n", "");
            let err = parse_facts(&banner, ["A"]).unwrap_err().to_string();
            assert!(err.contains("login banner"), "{err}");
            assert!(err.contains("Welcome!"), "{err}");
            assert!(parse_facts(&output(0, "A=1\n", ""), ["A", "B"]).is_err());
            assert!(parse_facts(&output(0, "A=1\nB=2\n", ""), ["A"]).is_err());
            assert!(parse_facts(&output(0, "B=2\nA=1\n", ""), ["A", "B"]).is_err());
            assert!(parse_facts(&output(0, "", ""), ["A"]).is_err());
            let noisy = output(0, "Welcome!\n", "warning: x\n");
            let err = parse_facts(&noisy, ["A"]).unwrap_err().to_string();
            assert!(err.ends_with("Welcome!\n(stderr) warning: x"), "{err}");
        }

        #[test]
        fn fail_unless_success_names_what_destination_status_and_stderr() {
            let dest = Destination::parse("u@h").unwrap();
            assert!(fail_unless_success(&output(0, "", ""), "x", &dest).is_ok());
            let err =
                fail_unless_success(&output(255, "", "Connection refused\n"), "preflight", &dest)
                    .unwrap_err()
                    .to_string();
            assert!(err.starts_with("preflight on u@h failed ("), "{err}");
            assert!(err.ends_with("Connection refused"), "{err}");
            let quiet = fail_unless_success(&output(1, "", ""), "launch", &dest)
                .unwrap_err()
                .to_string();
            assert!(quiet.ends_with(')'), "no trailing colon: {quiet}");
        }

        #[test]
        fn fake_records_rendered_commands_and_replays_replies() {
            let fake = FakeSsh::new();
            fake.reply(0, "A=1\n");
            let cmd = RemoteCommand::script("printf A=1").arg("x y");
            let out = fake
                .run(&dest(), &cmd, Some(b"body"), Duration::from_secs(1))
                .unwrap();
            assert_eq!(out.stdout, b"A=1\n");
            assert_eq!(
                fake.calls(),
                [Call {
                    dest: "user@host".into(),
                    command: "sh -c 'printf A=1' sh 'x y'".into(),
                    input: Some(b"body".to_vec()),
                }]
            );
        }
    }
}
