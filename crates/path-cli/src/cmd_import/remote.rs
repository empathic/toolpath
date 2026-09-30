//! `p import claude --remote`: read a Claude session off an ssh host
//! into the local cache, the way a local import reads `~/.claude`.
//!
//! The remote runs no `path`. Two read-only calls find the session:
//! the home probe, and a listing of the remote slug directory that
//! carries each file's stem, size, and first `sessionId`. The local
//! side resolves the session chain from that listing
//! ([`toolpath_claude::build_succession_map`], then
//! [`toolpath_claude::resolve_chain_with_map`]), fetches each
//! segment with one `cat`, lands the segments in a temporary
//! directory shaped like `~/.claude`, and derives from there. The
//! derived document is rooted at the local project directory and
//! records the destination and the remote directory under
//! `path.meta.remote` (the `extra` map, which flattens into `meta`).
//!
//! The scripts (`probe_home.sh` and `list_segments.sh`, next to this
//! module) print `TP_<NAME>=<value>` lines; [`crate::ssh::parse_facts`]
//! and [`crate::ssh::parse_tagged_lines`] reject any other output, and every
//! file stem in the listing passes [`require_plain_file_name`] before
//! it becomes a path component, so neither a login banner nor a
//! crafted listing can name a file outside the landing directory.

use anyhow::{Context, Result, bail};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::claude_session::{parse_posix_dir, parse_uuid_arg, swap_home};
use crate::config::Config;
use crate::derive::{DerivedDoc, derive_claude_session_with};
use crate::providers;
use crate::ssh::{
    DEAD_PEER_TIMEOUT, Destination, RemoteCommand, Transport, fail_unless_success, parse_facts,
    parse_tagged_lines, require_absolute_path, transfer_timeout,
};

/// The `path.meta` key that records where a pulled session came
/// from.
const REMOTE_META_KEY: &str = "remote";

/// The value under [`REMOTE_META_KEY`]: where a pulled session came
/// from.
#[derive(serde::Serialize)]
struct RemoteOrigin<'a> {
    destination: String,
    project_dir: &'a str,
}

/// The `p import claude` flags that read the session from another
/// host.
#[derive(clap::Args, Debug, Default)]
#[command(next_help_heading = "Remote host")]
pub struct RemoteImportArgs {
    /// Read the session from this ssh destination (`user@host` or
    /// `user@host:port`) instead of this machine. Needs --session, the
    /// session ID on the remote. The session is looked up under the
    /// remote project directory (see -C), fetched, and derived here;
    /// the document is rooted at the local project directory
    /// (--project, default: the current directory).
    #[arg(long = "remote", value_name = "DEST", value_parser = Destination::parse, requires = "session", conflicts_with = "all")]
    pub(super) dest: Option<Destination>,

    /// The remote project directory. Absolute POSIX path in normalized
    /// form. Default: the local project directory with the local home
    /// swapped for the remote home. Only with --remote.
    #[arg(short = 'C', long, value_name = "DIR", requires = "dest", value_parser = parse_posix_dir)]
    pub(super) cwd: Option<String>,
}

/// One pull: where the session is and the local context the document
/// is rooted in.
pub(super) struct RemotePull<'a> {
    pub(super) dest: &'a Destination,
    /// The `-C` value.
    pub(super) remote_dir: Option<&'a str>,
    /// The session ID on the remote: a UUID naming a file stem, or any
    /// member of its chain.
    pub(super) session: &'a str,
    /// The local project directory the document is rooted at.
    pub(super) local_project: &'a Path,
    pub(super) local_home: &'a Path,
}

/// One `.jsonl` file in the remote slug directory.
#[derive(Debug)]
struct Segment {
    stem: String,
    bytes: u64,
    /// The first `sessionId` in the file.
    first_session_id: Option<String>,
}

impl Segment {
    /// The value of one `TP_SEGMENT` line: `<stem>\t<bytes>\t<first
    /// sessionId>`, as `list_segments.sh` prints it; the third field is
    /// empty when the file carries no `sessionId`.
    fn parse(line: &str, dest: &Destination) -> Result<Self> {
        let [stem, bytes, first_session_id]: [&str; 3] = line
            .split('\t')
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| anyhow::anyhow!("bad listing line from {dest}: {line:?}"))?;
        require_plain_file_name(stem, &format!("a file name listed by {dest}"))?;
        let bytes = bytes
            .parse()
            .with_context(|| format!("bad size for {stem} from {dest}: {bytes:?}"))?;
        Ok(Self {
            stem: stem.to_string(),
            bytes,
            first_session_id: (!first_session_id.is_empty()).then(|| first_session_id.to_string()),
        })
    }
}

/// `p import claude --remote`: the session comes off the ssh host,
/// rooted at `project` (default: the current directory).
pub(super) fn derive_from_args(
    project: Option<String>,
    session: &str,
    dest: &Destination,
    remote_dir: Option<&str>,
    config: &Config,
) -> Result<DerivedDoc> {
    let local_project = match project {
        Some(p) => {
            std::fs::canonicalize(&p).with_context(|| format!("resolve project path {p}"))?
        }
        None => std::env::current_dir()?,
    };
    let home = config
        .home_dir()
        .context("cannot determine the home directory")?;
    let transport = providers::ssh_client(config)?;
    derive(
        &RemotePull {
            dest,
            remote_dir,
            session,
            local_project: &local_project,
            local_home: home,
        },
        &transport,
    )
}

/// Probe, list, fetch the chain, derive.
pub(super) fn derive(request: &RemotePull, transport: &dyn Transport) -> Result<DerivedDoc> {
    let RemotePull {
        dest,
        remote_dir,
        session,
        local_project,
        local_home,
    } = *request;
    let session = parse_uuid_arg(session).context("--session")?;
    let session = session.as_str();
    if !local_project.is_absolute() {
        bail!(
            "the local project directory must be absolute (got {})",
            local_project.display()
        );
    }

    let home = probe_home(transport, dest)?;
    let remote_dir = match remote_dir {
        Some(dir) => dir.to_string(),
        None => swap_home(local_project, local_home, &home)?,
    };
    let remote_layout = toolpath_claude::PathResolver::new().with_home(&home);
    let slug_dir = remote_path_string(
        remote_layout.project_dir(&remote_dir),
        "the remote slug directory",
    )?;
    let segments = list_segments(transport, dest, &slug_dir)?;
    if segments.is_empty() {
        bail!("no Claude sessions under {slug_dir} on {dest}; pass -C <remote-dir>");
    }

    let succession = toolpath_claude::build_succession_map(
        segments
            .iter()
            .map(|s| (s.stem.as_str(), s.first_session_id.as_deref())),
    );
    let chain = toolpath_claude::resolve_chain_with_map(&succession, session);
    let members: Vec<&Segment> = chain
        .iter()
        .map(|stem| {
            segments.iter().find(|s| &s.stem == stem).with_context(|| {
                let mut message = if stem == session {
                    format!("session {session} not found under {slug_dir} on {dest}")
                } else {
                    format!(
                        "segment {stem} of the chain of {session} is not under {slug_dir} on {dest}"
                    )
                };
                message.push_str("; sessions there:");
                for s in &segments {
                    let _ = write!(message, "\n  {} ({} bytes)", s.stem, s.bytes);
                }
                message
            })
        })
        .collect::<Result<_>>()?;
    let head = &chain[0];

    let total: u64 = members.iter().map(|s| s.bytes).sum();
    eprintln!(
        "Pulling session {session} from {dest}:{slug_dir} ({} segment{}, {total} bytes)",
        members.len(),
        if members.len() == 1 { "" } else { "s" }
    );
    let landing = tempfile::tempdir().context("create the landing directory")?;
    let resolver = toolpath_claude::PathResolver::new().with_claude_dir(landing.path());
    for segment in &members {
        let remote_file = remote_path_string(
            remote_layout.conversation_file(&remote_dir, &segment.stem),
            "the remote segment",
        )?;
        let bytes = fetch_segment(transport, dest, &remote_file, segment.bytes)?;
        let file = resolver
            .conversation_file(&remote_dir, &segment.stem)
            .context("build the landing file path")?;
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&file, bytes).with_context(|| format!("write {}", file.display()))?;
    }

    let manager = toolpath_claude::ClaudeConvo::with_resolver(resolver);
    let mut derived = derive_claude_session_with(&manager, &remote_dir, head)?;
    reroot(
        &mut derived,
        dest,
        &remote_dir,
        &local_project.to_string_lossy(),
    );
    Ok(derived)
}

/// Root the document at the local project directory and record the
/// remote origin. The manifest record carries no path or stamp: sync
/// skips the session while no local session carries its ID, and a
/// local session with that ID (`path resume` of this document creates
/// one) wins on the next sync.
fn reroot(derived: &mut DerivedDoc, dest: &Destination, remote_dir: &str, local_dir: &str) {
    if let Some(provenance) = derived.provenance.as_mut() {
        provenance.path = None;
        provenance.modified = None;
        provenance.size = None;
    }
    for path in &mut derived.doc.paths {
        let toolpath::v1::PathOrRef::Path(path) = path else {
            continue;
        };
        path.path.base = Some(toolpath::v1::Base {
            uri: format!("file://{local_dir}"),
            ref_str: None,
            branch: None,
        });
        let origin = RemoteOrigin {
            destination: dest.to_string(),
            project_dir: remote_dir,
        };
        path.meta.get_or_insert_with(Default::default).extra.insert(
            REMOTE_META_KEY.to_string(),
            serde_json::to_value(origin).expect("a remote origin serializes"),
        );
    }
}

/// A path the remote resolver built, as the string the remote takes.
/// `what` names the path in the error.
fn remote_path_string(path: toolpath_claude::Result<PathBuf>, what: &str) -> Result<String> {
    path.with_context(|| format!("build {what} path"))?
        .into_os_string()
        .into_string()
        .map_err(|_| anyhow::anyhow!("{what} path is not valid UTF-8"))
}

/// Error unless `name` is one plain file name: a non-empty run of
/// `[A-Za-z0-9._-]` that is not `.` or `..`. `what` names the value
/// in the error.
fn require_plain_file_name(name: &str, what: &str) -> Result<()> {
    let plain = !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !plain {
        bail!("{what} must be one plain file name (letters, digits, and `._-`; got {name:?})");
    }
    Ok(())
}

/// The remote home.
fn probe_home(transport: &dyn Transport, dest: &Destination) -> Result<String> {
    let command = RemoteCommand::from_script(include_str!("probe_home.sh"), [] as [&str; 0]);
    let output = transport.run(dest, &command, DEAD_PEER_TIMEOUT)?;
    fail_unless_success(&output, "home probe", dest)?;
    let [home] = parse_facts(&output, ["TP_HOME"])?;
    require_absolute_path(&home, "remote $HOME", dest)
}

/// Every `.jsonl` file in the remote slug directory.
fn list_segments(
    transport: &dyn Transport,
    dest: &Destination,
    slug_dir: &str,
) -> Result<Vec<Segment>> {
    let command = RemoteCommand::from_script(include_str!("list_segments.sh"), [slug_dir]);
    let output = transport.run(dest, &command, DEAD_PEER_TIMEOUT)?;
    fail_unless_success(&output, "session listing", dest)?;
    parse_tagged_lines(&output, "TP_SEGMENT")?
        .iter()
        .map(|line| Segment::parse(line, dest))
        .collect()
}

/// One fetch call: the bytes of `file` on stdout. `bytes` is its
/// listed size, which bounds the wait.
fn fetch_segment(
    transport: &dyn Transport,
    dest: &Destination,
    file: &str,
    bytes: u64,
) -> Result<Vec<u8>> {
    let command = RemoteCommand::new(["cat", file]);
    let output = transport.run(dest, &command, transfer_timeout(bytes))?;
    fail_unless_success(&output, &format!("fetching {file}"), dest)?;
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh::fake::{Call, FakeSsh};

    const HOME: &str = "/home/remote";
    const DIR: &str = "/home/remote/work";
    const SLUG_DIR: &str = "/home/remote/.claude/projects/-home-remote-work";

    /// Session A, one user turn; the file the pull fetches first.
    const A: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    /// Session B continues A: its first entry carries A's ID.
    const B: &str = "bbbbbbbb-0000-4000-8000-000000000002";
    /// An unrelated single-segment session.
    const C: &str = "cccccccc-0000-4000-8000-000000000003";

    /// One user turn and one reply, both carrying A's ID.
    fn segment_a() -> String {
        format!(
            "{{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":null,\"sessionId\":\"{A}\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"cwd\":\"{DIR}\",\"message\":{{\"role\":\"user\",\"content\":\"first\"}}}}\n\
             {{\"type\":\"assistant\",\"uuid\":\"u2\",\"parentUuid\":\"u1\",\"sessionId\":\"{A}\",\"timestamp\":\"2026-01-01T00:00:01Z\",\"cwd\":\"{DIR}\",\"message\":{{\"role\":\"assistant\",\"content\":\"one\"}}}}\n"
        )
    }

    /// The bridge line first (a copy of A's last entry, carrying A's
    /// ID), then B's own entries.
    fn segment_b() -> String {
        format!(
            "{{\"type\":\"assistant\",\"uuid\":\"u2\",\"parentUuid\":\"u1\",\"sessionId\":\"{A}\",\"timestamp\":\"2026-01-01T00:00:01Z\",\"cwd\":\"{DIR}\",\"message\":{{\"role\":\"assistant\",\"content\":\"one\"}}}}\n\
             {{\"type\":\"user\",\"uuid\":\"u3\",\"parentUuid\":\"u2\",\"sessionId\":\"{B}\",\"timestamp\":\"2026-01-01T00:00:02Z\",\"cwd\":\"{DIR}\",\"message\":{{\"role\":\"user\",\"content\":\"second\"}}}}\n\
             {{\"type\":\"assistant\",\"uuid\":\"u4\",\"parentUuid\":\"u3\",\"sessionId\":\"{B}\",\"timestamp\":\"2026-01-01T00:00:03Z\",\"cwd\":\"{DIR}\",\"message\":{{\"role\":\"assistant\",\"content\":\"two\"}}}}\n"
        )
    }

    fn dest() -> Destination {
        Destination::parse("user@host").unwrap()
    }

    fn reply_home(fake: &FakeSsh) {
        fake.reply(0, &format!("TP_HOME={HOME}\n"));
    }

    /// The listing for A, B (continuing A), and C.
    fn reply_listing(fake: &FakeSsh) {
        fake.reply(
            0,
            &format!(
                "TP_SEGMENT={A}\t{}\t{A}\nTP_SEGMENT={B}\t{}\t{A}\nTP_SEGMENT={C}\t10\t{C}\n",
                segment_a().len(),
                segment_b().len()
            ),
        );
    }

    fn request<'a>(dest: &'a Destination, session: &'a str) -> RemotePull<'a> {
        RemotePull {
            dest,
            remote_dir: Some(DIR),
            session,
            local_project: Path::new("/home/local/work"),
            local_home: Path::new("/home/local"),
        }
    }

    fn fetched_files(fake: &FakeSsh) -> Vec<String> {
        fake.calls()
            .iter()
            .filter_map(|call| match call {
                Call::Run { command, .. } if command.starts_with("cat ") => {
                    Some(command["cat ".len()..].to_string())
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_named_session_fetches_its_chain_and_reroots_the_document() {
        let fake = FakeSsh::new();
        let d = dest();
        reply_home(&fake);
        reply_listing(&fake);
        fake.reply(0, &segment_a());
        fake.reply(0, &segment_b());
        let derived = derive(&request(&d, A), &fake).unwrap();

        assert_eq!(
            fetched_files(&fake),
            [
                format!("{SLUG_DIR}/{A}.jsonl"),
                format!("{SLUG_DIR}/{B}.jsonl")
            ]
        );
        let calls = fake.calls();
        assert_eq!(calls.len(), 4);
        for call in &calls {
            let Call::Run { input, .. } = call else {
                panic!("every call is a run");
            };
            assert!(input.is_none(), "every call is read-only");
        }
        let Call::Run { command, .. } = &calls[1] else {
            unreachable!()
        };
        assert!(command.contains(SLUG_DIR), "{command}");

        let path = derived.doc.single_path().unwrap();
        assert_eq!(path.steps.len(), 4, "both segments derive");
        assert_eq!(
            path.path.base.as_ref().unwrap().uri,
            "file:///home/local/work"
        );
        let remote = &path.meta.as_ref().unwrap().extra[REMOTE_META_KEY];
        assert_eq!(remote["destination"], "user@host");
        assert_eq!(remote["project_dir"], DIR);
        let provenance = derived.provenance.as_ref().unwrap();
        assert_eq!(provenance.id, A);
        assert!(provenance.path.is_none());
        assert!(provenance.modified.is_none());
        assert!(provenance.size.is_none());
        assert!(
            derived.cache_id.starts_with("claude-"),
            "{}",
            derived.cache_id
        );
    }

    #[test]
    fn a_successor_id_resolves_to_the_whole_chain() {
        let fake = FakeSsh::new();
        let d = dest();
        reply_home(&fake);
        reply_listing(&fake);
        fake.reply(0, &segment_a());
        fake.reply(0, &segment_b());
        let derived = derive(&request(&d, B), &fake).unwrap();
        assert_eq!(fetched_files(&fake).len(), 2);
        assert!(
            derived.cache_id.starts_with("claude-"),
            "{}",
            derived.cache_id
        );
    }

    #[test]
    fn an_unknown_session_errors_before_any_fetch() {
        let fake = FakeSsh::new();
        let d = dest();
        reply_home(&fake);
        reply_listing(&fake);
        let err = derive(&request(&d, "dddddddd-0000-4000-8000-000000000004"), &fake).unwrap_err();
        assert!(err.to_string().contains("not found under"), "{err:#}");
        assert!(err.to_string().contains(A), "{err:#}");
        assert_eq!(fake.calls().len(), 2);
    }

    /// B continues a segment the listing lacks (deleted by Claude
    /// Code's cleanup, or in another slug directory). The error names
    /// that segment, not B.
    #[test]
    fn an_unlisted_predecessor_errors_before_any_fetch() {
        let fake = FakeSsh::new();
        let d = dest();
        reply_home(&fake);
        let x = "xxxxxxxx-0000-4000-8000-000000000009";
        fake.reply(
            0,
            &format!("TP_SEGMENT={A}\t10\t{A}\nTP_SEGMENT={B}\t10\t{x}\n"),
        );
        let err = derive(&request(&d, B), &fake).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains(&format!("segment {x} of the chain of {B}")),
            "{text}"
        );
        assert!(text.contains("is not under"), "{text}");
        assert_eq!(fake.calls().len(), 2);
    }

    #[test]
    fn an_empty_listing_names_the_directory() {
        let fake = FakeSsh::new();
        let d = dest();
        reply_home(&fake);
        fake.reply(0, "");
        let err = derive(&request(&d, A), &fake).unwrap_err();
        assert!(err.to_string().contains(SLUG_DIR), "{err:#}");
    }

    /// A rotation artifact (`<stem>.orphaned-<ts>-<hash>`) opens with
    /// the original session's ID, like a successor. It is not one: the
    /// chain is A then B, and the artifact is neither fetched nor
    /// derived.
    #[test]
    fn an_orphaned_segment_is_not_part_of_the_chain() {
        let fake = FakeSsh::new();
        let d = dest();
        reply_home(&fake);
        fake.reply(
            0,
            &format!(
                "TP_SEGMENT={A}\t{}\t{A}\nTP_SEGMENT={B}\t{}\t{A}\nTP_SEGMENT={B}.orphaned-1787626221622-ac84712d\t10\t{A}\n",
                segment_a().len(),
                segment_b().len()
            ),
        );
        fake.reply(0, &segment_a());
        fake.reply(0, &segment_b());
        let derived = derive(&request(&d, A), &fake).unwrap();
        assert_eq!(
            fetched_files(&fake),
            [
                format!("{SLUG_DIR}/{A}.jsonl"),
                format!("{SLUG_DIR}/{B}.jsonl")
            ]
        );
        let path = derived.doc.single_path().unwrap();
        assert_eq!(path.steps.len(), 4, "both segments derive");
    }

    #[test]
    fn a_session_that_is_not_a_uuid_errors_before_any_remote_call() {
        let fake = FakeSsh::new();
        let d = dest();
        for bad in ["", ".", "..", "a/b", "a b", "a\nb", "my-session"] {
            let err = derive(&request(&d, bad), &fake).unwrap_err();
            let text = format!("{err:#}");
            assert!(text.contains("--session"), "{bad:?}: {text}");
            assert!(text.contains("must be a UUID"), "{bad:?}: {text}");
        }
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn a_session_id_is_normalized_before_the_lookup() {
        let fake = FakeSsh::new();
        let d = dest();
        reply_home(&fake);
        reply_listing(&fake);
        fake.reply(0, &segment_a());
        fake.reply(0, &segment_b());
        let upper = A.to_ascii_uppercase();
        derive(&request(&d, &upper), &fake).unwrap();
        assert_eq!(fetched_files(&fake)[0], format!("{SLUG_DIR}/{A}.jsonl"));
    }

    /// The listing script prints the first `sessionId` the reader
    /// would find, for every shape of file the reader accepts.
    #[test]
    fn the_listing_script_agrees_with_the_reader() {
        let dir = tempfile::tempdir().unwrap();
        let entry = |sid: &str, content: &str| {
            format!(
                "{{\"type\":\"user\",\"uuid\":\"u\",\"parentUuid\":null,\"sessionId\":\"{sid}\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"cwd\":\"{DIR}\",\"message\":{{\"role\":\"user\",\"content\":\"{content}\"}}}}\n"
            )
        };
        let no_sid = "{\"type\":\"summary\",\"summary\":\"topic\",\"leafUuid\":\"u\"}\n";
        let fixtures: [(&str, String); 7] = [
            ("plain", entry(A, "first")),
            (
                "bridge-first",
                format!("{}{}", entry(A, "bridge"), entry(B, "own")),
            ),
            ("summary-then-entry", format!("{no_sid}{}", entry(C, "own"))),
            ("no-session-id", no_sid.to_string()),
            ("empty", String::new()),
            (
                "escaped-key-in-body",
                format!(
                    "{{\"type\":\"user\",\"uuid\":\"u\",\"parentUuid\":null,\"message\":{{\"role\":\"user\",\"content\":\"say \\\"sessionId\\\": \\\"{B}\\\"\"}},\"sessionId\":\"{A}\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"cwd\":\"{DIR}\"}}\n"
                ),
            ),
            (
                "past-the-tenth-line",
                format!("{}{}", no_sid.repeat(10), entry(A, "late")),
            ),
        ];
        for (stem, text) in &fixtures {
            std::fs::write(dir.path().join(format!("{stem}.jsonl")), text).unwrap();
        }
        std::fs::write(dir.path().join("not-a-segment.txt"), "x").unwrap();

        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(include_str!("list_segments.sh"))
            .arg("sh")
            .arg(dir.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let listed: Vec<Segment> = parse_tagged_lines(&output, "TP_SEGMENT")
            .unwrap()
            .iter()
            .map(|line| Segment::parse(line, &dest()).unwrap())
            .collect();

        assert_eq!(listed.len(), fixtures.len());
        for segment in &listed {
            let file = dir.path().join(format!("{}.jsonl", segment.stem));
            assert_eq!(segment.bytes, std::fs::metadata(&file).unwrap().len());
            assert_eq!(
                segment.first_session_id,
                toolpath_claude::ConversationReader::read_first_session_id(&file),
                "{}",
                segment.stem
            );
        }
        assert_eq!(
            listed
                .iter()
                .filter(|s| s.first_session_id.is_some())
                .count(),
            4,
            "the fixtures cover both outcomes"
        );
    }

    #[test]
    fn a_missing_slug_directory_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(include_str!("list_segments.sh"))
            .arg("sh")
            .arg(dir.path().join("absent"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
    }

    /// The remote controls the listing; a stem with a path separator
    /// or `..` in it must not reach the landing directory.
    #[test]
    fn a_listed_stem_that_is_not_a_plain_file_name_errors_before_any_fetch() {
        for bad in ["../../escape", "/etc/passwd", "a/b", ".."] {
            let fake = FakeSsh::new();
            let d = dest();
            reply_home(&fake);
            fake.reply(
                0,
                &format!("TP_SEGMENT={A}\t10\t{A}\nTP_SEGMENT={bad}\t10\t{A}\n"),
            );
            let err = derive(&request(&d, A), &fake).unwrap_err();
            assert!(
                err.to_string().contains("listed by user@host"),
                "{bad:?}: {err:#}"
            );
            assert_eq!(fake.calls().len(), 2, "{bad:?}");
        }
    }

    #[test]
    fn a_failed_fetch_stops_the_pull() {
        let fake = FakeSsh::new();
        let d = dest();
        reply_home(&fake);
        reply_listing(&fake);
        fake.reply_with_stderr(1, "", "Permission denied");
        let err = derive(&request(&d, A), &fake).unwrap_err();
        assert!(err.to_string().contains("fetching"), "{err:#}");
        assert!(err.to_string().contains("Permission denied"), "{err:#}");
        assert_eq!(fake.calls().len(), 3);
    }

    #[test]
    fn a_login_banner_errors_and_quotes_the_reply() {
        let fake = FakeSsh::new();
        let d = dest();
        fake.reply(0, "Welcome to the machine!\nTP_HOME=/home/remote\n");
        let err = derive(&request(&d, A), &fake).unwrap_err();
        assert!(format!("{err:#}").contains("Welcome to the machine!"));
    }

    #[test]
    fn the_remote_dir_defaults_to_the_home_swap_of_the_local_project() {
        let fake = FakeSsh::new();
        let d = dest();
        reply_home(&fake);
        fake.reply(0, "");
        let mut request = request(&d, A);
        request.remote_dir = None;
        request.local_project = Path::new("/home/local/a/b");
        let err = derive(&request, &fake).unwrap_err();
        assert!(
            err.to_string()
                .contains("/home/remote/.claude/projects/-home-remote-a-b"),
            "{err:#}"
        );
    }

    #[test]
    fn a_relative_local_project_errors_before_any_remote_call() {
        let fake = FakeSsh::new();
        let d = dest();
        let mut request = request(&d, A);
        request.local_project = Path::new("relative");
        let err = derive(&request, &fake).unwrap_err();
        assert!(err.to_string().contains("must be absolute"), "{err:#}");
        assert!(fake.calls().is_empty());
    }
}
