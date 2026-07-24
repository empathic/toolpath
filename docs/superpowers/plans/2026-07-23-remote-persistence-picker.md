# Remote Persistence Picker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let `path resume --remote` choose a remote session-persistence backend (plain/tmux/abduco/dtach/zellij/shpool) via a picker mirroring the harness picker, plus a reserved `--via ssh|mosh|et` transport axis.

**Architecture:** All work is in `crates/path-cli/src/cmd_resume.rs`. A `PersistBackend` enum + pure `persist_plan()` produce a `PersistPlan { remote_command, extra_file, post_note }`; a `Transport` enum + `launch_invocation()` seam carries it. A one-shot `remote_which` probe over the existing libssh2 `ExecStrategy` feeds candidate assembly + pre-selection, which the picker resolves. `run_remote` is rewired to: probe → pick backend → build plan → ship (+ extra_file) → note → launch via transport.

**Tech Stack:** Rust (edition 2024), clap `ValueEnum`, ssh2 (libssh2), existing skim/fzf picker in `crates/path-cli/src/skim_picker.rs`.

## Global Constraints

- Package `path-cli`; no new dependencies.
- Session name is `format!("path-{}", session_id.replace(['.', ':'], "-"))` — reuse verbatim (tmux/abduco/dtach/zellij/shpool all key on it).
- `INNER` = existing inner launch string built by `remote_launch_command`'s head: `[cd <cwd> && ]<harness> <argv…>`, quoted via `shell_quote`/`shell_single_quote`.
- Remote command strings must be valid remote-shell syntax (they're passed as one argv element to `ssh` and re-split remotely). Reuse `shell_single_quote` for nesting.
- Backend display order: `tmux, zellij, abduco, dtach, shpool, plain`. Pre-selection priority: `tmux > zellij > abduco > dtach > plain` (shpool never auto-preferred).
- `--via` v1 implements only `ssh`; `mosh`/`et` return a clear "not yet supported" error.
- `--tmux` becomes a deprecated alias for `--persist tmux`; error if combined with `--persist`.
- TDD: failing test → run (fail) → implement → run (pass) → commit, per task.
- Reference: design spec `docs/superpowers/specs/2026-07-23-remote-persistence-picker-design.md`.

---

### Task 1: `PersistBackend` enum + clap/describe

**Files:**
- Modify: `crates/path-cli/src/cmd_resume.rs` (near the `Harness` enum)
- Test: same file `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `enum PersistBackend { Plain, Tmux, Abduco, Dtach, Zellij, Shpool }`; `PersistBackend::bin(&self) -> Option<&'static str>`; `PersistBackend::describe(&self) -> &'static str`; `PersistBackend::DISPLAY_ORDER: [PersistBackend; 6]`; derives `clap::ValueEnum`, `Copy`, `Clone`, `PartialEq`, `Eq`, `Debug`.

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn persist_backend_bin_and_order() {
    assert_eq!(PersistBackend::Plain.bin(), None);
    assert_eq!(PersistBackend::Tmux.bin(), Some("tmux"));
    assert_eq!(PersistBackend::Shpool.bin(), Some("shpool"));
    // Display order is stable and complete.
    assert_eq!(PersistBackend::DISPLAY_ORDER.len(), 6);
    assert_eq!(PersistBackend::DISPLAY_ORDER[0], PersistBackend::Tmux);
    assert!(PersistBackend::Tmux.describe().contains("detach"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p path-cli --lib persist_backend_bin_and_order`
Expected: FAIL — `PersistBackend` not found.

- [ ] **Step 3: Write minimal implementation**
```rust
/// Remote session-persistence backend for `--remote` resume. See the
/// design spec: three launch mechanisms (direct-wrap, layout-wrap for
/// zellij, attach-only for shpool).
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum PersistBackend {
    Plain,
    Tmux,
    Abduco,
    Dtach,
    Zellij,
    Shpool,
}

impl PersistBackend {
    /// Probe target on the remote (`command -v <bin>`); `Plain` has none.
    fn bin(&self) -> Option<&'static str> {
        match self {
            PersistBackend::Plain => None,
            PersistBackend::Tmux => Some("tmux"),
            PersistBackend::Abduco => Some("abduco"),
            PersistBackend::Dtach => Some("dtach"),
            PersistBackend::Zellij => Some("zellij"),
            PersistBackend::Shpool => Some("shpool"),
        }
    }

    fn describe(&self) -> &'static str {
        match self {
            PersistBackend::Plain => "plain — no persistence; dies on disconnect",
            PersistBackend::Tmux => "tmux — detachable; survives drops, reattachable",
            PersistBackend::Abduco => "abduco — minimal detach/attach; survives drops",
            PersistBackend::Dtach => "dtach — tiny detach/attach; survives drops",
            PersistBackend::Zellij => "zellij — detachable workspace (layout-launched)",
            PersistBackend::Shpool => "shpool — persistent shell (attach-only; run the command yourself)",
        }
    }

    /// Fixed picker display order.
    const DISPLAY_ORDER: [PersistBackend; 6] = [
        PersistBackend::Tmux,
        PersistBackend::Zellij,
        PersistBackend::Abduco,
        PersistBackend::Dtach,
        PersistBackend::Shpool,
        PersistBackend::Plain,
    ];
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p path-cli --lib persist_backend_bin_and_order`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(resume): PersistBackend enum for remote persistence backends"
```

---

### Task 2: `PersistPlan` + `persist_plan()` for direct-wrap backends

Replaces `remote_launch_command`. Covers `plain/tmux/abduco/dtach`; zellij/shpool land in Tasks 3–4 (return `Plain`-shaped output until then is NOT acceptable — implement their arms as `todo!()`-free explicit branches that Tasks 3/4 fill; here we route them through the direct path with a placeholder that Task 3/4 replace, so keep them out of this task's tests).

**Files:**
- Modify: `crates/path-cli/src/cmd_resume.rs` (`remote_launch_command` → `persist_plan`; update its 2 call sites and the `tmux:`-bool test constructors)
- Test: same file

**Interfaces:**
- Consumes: `remote_launch_command`'s existing INNER-building head (harness/argv/cwd/quoting).
- Produces:
  ```rust
  struct PersistPlan {
      remote_command: String,
      extra_file: Option<(String, Vec<u8>)>, // (remote path, contents) — zellij layout
      post_note: Option<String>,             // shpool guidance
  }
  fn persist_plan(harness: Harness, session_id: &str, cwd: Option<&str>, backend: PersistBackend, home: &str) -> PersistPlan
  ```
  `home` is the resolved remote home (for zellij layout path in Task 3).

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn persist_plan_direct_wrap_backends() {
    let id = "sess-1";
    let plain = persist_plan(Harness::Claude, id, None, PersistBackend::Plain, "/home/u");
    assert_eq!(plain.remote_command, "claude -r sess-1");
    assert!(plain.extra_file.is_none() && plain.post_note.is_none());

    let tmux = persist_plan(Harness::Claude, id, Some("/srv/w"), PersistBackend::Tmux, "/home/u");
    assert_eq!(
        tmux.remote_command,
        "tmux new-session -A -s path-sess-1 'cd /srv/w && claude -r sess-1'"
    );

    let abduco = persist_plan(Harness::Claude, id, None, PersistBackend::Abduco, "/home/u");
    assert_eq!(abduco.remote_command, "abduco -A path-sess-1 sh -c 'claude -r sess-1'");

    let dtach = persist_plan(Harness::Claude, id, None, PersistBackend::Dtach, "/home/u");
    assert_eq!(
        dtach.remote_command,
        "dtach -A /tmp/path-dtach-sess-1 -z sh -c 'claude -r sess-1'"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p path-cli --lib persist_plan_direct_wrap_backends`
Expected: FAIL — `persist_plan`/`PersistPlan` not found.

- [ ] **Step 3: Write minimal implementation**

Add the struct + function; keep the INNER-building head from `remote_launch_command`. (zellij/shpool arms delegate to a `TODO(task-3/4)` explicit branch — implement as shown so it compiles and the direct arms pass; Tasks 3–4 replace the marked arms.)
```rust
struct PersistPlan {
    remote_command: String,
    extra_file: Option<(String, Vec<u8>)>,
    post_note: Option<String>,
}

fn persist_plan(
    harness: Harness,
    session_id: &str,
    cwd: Option<&str>,
    backend: PersistBackend,
    home: &str,
) -> PersistPlan {
    // INNER — identical to the old remote_launch_command head.
    let launch: Vec<String> = std::iter::once(harness.name().to_string())
        .chain(argv_for(harness, session_id).iter().map(|a| shell_quote(a)))
        .collect();
    let inner = launch.join(" ");
    let inner = match cwd {
        Some(dir) => format!("cd {} && {inner}", shell_quote(dir)),
        None => inner,
    };
    let name = format!("path-{}", session_id.replace(['.', ':'], "-"));

    let mut extra_file = None;
    let mut post_note = None;
    let remote_command = match backend {
        PersistBackend::Plain => inner,
        PersistBackend::Tmux => format!(
            "tmux new-session -A -s {} {}",
            shell_quote(&name),
            shell_single_quote(&inner)
        ),
        PersistBackend::Abduco => format!(
            "abduco -A {} sh -c {}",
            shell_quote(&name),
            shell_single_quote(&inner)
        ),
        PersistBackend::Dtach => format!(
            "dtach -A {} -z sh -c {}",
            shell_quote(&format!("/tmp/path-dtach-{}", session_id.replace(['.', ':'], "-"))),
            shell_single_quote(&inner)
        ),
        PersistBackend::Zellij => zellij_plan(&name, &inner, home, &mut extra_file),
        PersistBackend::Shpool => shpool_plan(&name, &inner, &mut post_note),
    };
    PersistPlan { remote_command, extra_file, post_note }
}
```
Add temporary stubs so it compiles (Tasks 3–4 replace them):
```rust
fn zellij_plan(name: &str, _inner: &str, _home: &str, _extra: &mut Option<(String, Vec<u8>)>) -> String {
    // TODO(task-3): layout-wrap. Temporary: attach/create by name.
    format!("zellij attach --create {}", shell_quote(name))
}
fn shpool_plan(name: &str, _inner: &str, _note: &mut Option<String>) -> String {
    // TODO(task-4): attach-only + post_note.
    format!("shpool attach {}", shell_quote(name))
}
```
Then replace the two `remote_launch_command(...)` call sites in `run_remote` with `persist_plan(Harness::Claude, &session_id, launch_cwd.as_deref(), backend, &home)` — but `backend` doesn't exist until Task 8; for now pass `PersistBackend::Tmux` if `args.tmux` else `PersistBackend::Plain`, and use `.remote_command`. Delete `remote_launch_command` and update the two `remote_launch_command_*` tests to call `persist_plan(...).remote_command`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p path-cli --lib persist_plan_direct_wrap_backends persist && cargo test -p path-cli resume`
Expected: PASS (all resume tests green).

- [ ] **Step 5: Commit**
```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(resume): persist_plan + PersistPlan replacing remote_launch_command (direct-wrap backends)"
```

---

### Task 3: zellij layout-wrap

**Files:** Modify `crates/path-cli/src/cmd_resume.rs` (`zellij_plan`); Test: same file.

**Interfaces:**
- Produces: `zellij_plan` fills `extra_file = Some(("<home>/.cache/path/zellij-<name>.kdl", <kdl bytes>))` and returns `zellij --session <name> --layout <path>`.

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn persist_plan_zellij_ships_layout() {
    let p = persist_plan(Harness::Claude, "sess-1", Some("/srv/w"), PersistBackend::Zellij, "/home/u");
    assert_eq!(
        p.remote_command,
        "zellij --session path-sess-1 --layout /home/u/.cache/path/zellij-path-sess-1.kdl"
    );
    let (path, body) = p.extra_file.expect("layout shipped");
    assert_eq!(path, "/home/u/.cache/path/zellij-path-sess-1.kdl");
    let body = String::from_utf8(body).unwrap();
    assert!(body.contains("cd /srv/w && claude -r sess-1"), "body: {body}");
    assert!(body.contains("pane"), "must be a KDL layout: {body}");
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p path-cli --lib persist_plan_zellij_ships_layout`
Expected: FAIL — assertion (stub returns `attach --create`, no extra_file).

- [ ] **Step 3: Implement**
```rust
fn zellij_plan(
    name: &str,
    inner: &str,
    home: &str,
    extra: &mut Option<(String, Vec<u8>)>,
) -> String {
    let layout_path = format!("{home}/.cache/path/zellij-{name}.kdl");
    // Single-pane layout that runs INNER via the shell. `close_on_exit`
    // keeps the pane if the harness exits so output stays visible.
    let kdl = format!(
        "layout {{\n    pane command=\"sh\" {{\n        args \"-c\" {inner:?}\n    }}\n}}\n"
    );
    *extra = Some((layout_path.clone(), kdl.into_bytes()));
    format!(
        "zellij --session {} --layout {}",
        shell_quote(name),
        shell_quote(&layout_path)
    )
}
```
Note: the `.cache/path` dir is created by the ship step (Task 9 mkdirs the `extra_file`'s parent).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p path-cli --lib persist_plan_zellij_ships_layout`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(resume): zellij layout-wrap persistence backend"
```

---

### Task 4: shpool attach-only + post_note

**Files:** Modify `crates/path-cli/src/cmd_resume.rs` (`shpool_plan`); Test: same file.

**Interfaces:**
- Produces: `shpool_plan` returns `shpool attach <name>` and sets `post_note = Some("In the shpool session, run:  <INNER>")`.

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn persist_plan_shpool_attach_only_with_note() {
    let p = persist_plan(Harness::Claude, "sess-1", Some("/srv/w"), PersistBackend::Shpool, "/home/u");
    assert_eq!(p.remote_command, "shpool attach path-sess-1");
    let note = p.post_note.expect("shpool note");
    assert!(note.contains("cd /srv/w && claude -r sess-1"), "note: {note}");
    assert!(p.extra_file.is_none());
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p path-cli --lib persist_plan_shpool_attach_only_with_note`
Expected: FAIL — no post_note.

- [ ] **Step 3: Implement**
```rust
fn shpool_plan(name: &str, inner: &str, note: &mut Option<String>) -> String {
    *note = Some(format!(
        "shpool has no command arg — in the persistent shell, run:\n    {inner}"
    ));
    format!("shpool attach {}", shell_quote(name))
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p path-cli --lib persist_plan_shpool_attach_only_with_note`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(resume): shpool attach-only persistence backend + post-note"
```

---

### Task 5: `Transport` enum + `launch_invocation` seam

**Files:** Modify `crates/path-cli/src/cmd_resume.rs`; Test: same file.

**Interfaces:**
- Produces: `enum Transport { Ssh, Mosh, Et }` (derives `clap::ValueEnum`, `Copy`, `Clone`, `PartialEq`, `Eq`, `Debug`); `fn launch_invocation(transport: Transport, remote: &str, remote_cmd: &str) -> Result<(String, Vec<String>)>`. `Ssh` delegates to existing `ssh_invocation_tty(remote, remote_cmd, true)`; `Mosh`/`Et` bail.

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn launch_invocation_ssh_and_deferred_transports() {
    let (bin, argv) = launch_invocation(Transport::Ssh, "ssh://h", "claude -r x").unwrap();
    assert_eq!(bin, "ssh");
    assert_eq!(argv, vec!["-t".to_string(), "h".to_string(), "claude -r x".to_string()]);

    let err = launch_invocation(Transport::Mosh, "ssh://h", "claude -r x").unwrap_err();
    assert!(err.to_string().contains("not yet supported"), "{err}");
    assert!(launch_invocation(Transport::Et, "ssh://h", "x").is_err());
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p path-cli --lib launch_invocation_ssh_and_deferred_transports`
Expected: FAIL — not found.

- [ ] **Step 3: Implement**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Transport {
    Ssh,
    Mosh,
    Et,
}

/// Carry the persist-wrapped remote command over the chosen transport.
/// v1 implements only ssh; mosh/et are reserved (spec: Transport axis).
fn launch_invocation(
    transport: Transport,
    remote: &str,
    remote_cmd: &str,
) -> Result<(String, Vec<String>)> {
    match transport {
        Transport::Ssh => ssh_invocation_tty(remote, remote_cmd, true),
        Transport::Mosh => anyhow::bail!(
            "--via mosh is not yet supported (reserved); use --via ssh"
        ),
        Transport::Et => anyhow::bail!(
            "--via et is not yet supported (reserved); use --via ssh"
        ),
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p path-cli --lib launch_invocation_ssh_and_deferred_transports`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(resume): Transport enum + launch_invocation seam (ssh; mosh/et reserved)"
```

---

### Task 6: `remote_which` probe on `ExecStrategy`

**Files:** Modify `crates/path-cli/src/cmd_resume.rs` (`trait ExecStrategy`, `RealExec` impl, `RecordingExec` impl + fields); Test: same file.

**Interfaces:**
- Produces: `fn remote_which(&self, target: &SshTarget, bins: &[&str]) -> Result<std::collections::BTreeSet<String>>` on `ExecStrategy`. `RealExec` runs one exec channel: `for b in bins; do command -v "$b" >/dev/null 2>&1 && echo "$b"; done`. `RecordingExec` returns a canned set (new field `available: BTreeSet<String>`, default all).

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn recording_exec_remote_which_returns_canned_set() {
    let rec = RecordingExec::with_available(["tmux", "dtach"]);
    let got = rec
        .remote_which(&SshTarget { user: None, host: "h".into(), port: None }, &["tmux", "zellij", "dtach"])
        .unwrap();
    assert!(got.contains("tmux") && got.contains("dtach"));
    assert!(!got.contains("zellij"));
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p path-cli --lib recording_exec_remote_which_returns_canned_set`
Expected: FAIL — no `remote_which` / `with_available`.

- [ ] **Step 3: Implement**

Add to the trait:
```rust
/// Which of `bins` exist on the remote (`command -v`). One exec channel.
fn remote_which(
    &self,
    target: &SshTarget,
    bins: &[&str],
) -> Result<std::collections::BTreeSet<String>>;
```
`RealExec` impl (uses `with_conn` + `exec_channel_capture`, like `remote_home`'s SCP branch):
```rust
fn remote_which(
    &self,
    target: &SshTarget,
    bins: &[&str],
) -> Result<std::collections::BTreeSet<String>> {
    if bins.is_empty() {
        return Ok(Default::default());
    }
    let probe = bins
        .iter()
        .map(|b| format!("command -v {} >/dev/null 2>&1 && echo {}", shell_single_quote(b), shell_single_quote(b)))
        .collect::<Vec<_>>()
        .join("; ");
    self.with_conn(target, |conn| {
        let out = exec_channel_capture(&conn.sess, &probe)?;
        Ok(out.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
    })
}
```
`RecordingExec`: add field `available: std::collections::BTreeSet<String>`, constructor `with_available`, and impl:
```rust
pub fn with_available<'a>(bins: impl IntoIterator<Item = &'a str>) -> Self {
    Self { available: bins.into_iter().map(String::from).collect(), ..Default::default() }
}
// in impl ExecStrategy for RecordingExec:
fn remote_which(&self, _t: &SshTarget, bins: &[&str]) -> Result<std::collections::BTreeSet<String>> {
    Ok(bins.iter().filter(|b| self.available.contains(**b)).map(|b| b.to_string()).collect())
}
```
Default `RecordingExec` (via `Default`) has an empty `available`; existing tests that expect a working launch must use `with_available(["tmux","abduco","dtach","zellij","shpool"])` OR pass an explicit `--persist` that skips probing (Task 8 makes `--persist` skip the probe requirement). Keep existing tests green by having them pass `--persist plain` (no probe needed) or `with_available`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p path-cli --lib recording_exec_remote_which_returns_canned_set`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(resume): ExecStrategy::remote_which probe (+ RecordingExec canned availability)"
```

---

### Task 7: CLI flags `--persist` / `--via` + `--tmux` deprecation

**Files:** Modify `crates/path-cli/src/cmd_resume.rs` (`struct ResumeArgs`); Test: same file.

**Interfaces:**
- Produces on `ResumeArgs`: `pub persist: Option<PersistBackend>` (`#[arg(long, value_enum, requires = "remote")]`), `pub via: Transport` (`#[arg(long, value_enum, default_value_t = Transport::Ssh, requires = "remote")]`). Keep `pub tmux: bool`. New `fn resolve_persist_flag(args: &ResumeArgs) -> Result<Option<PersistBackend>>`: returns `Some(Tmux)` for `--tmux` (with a deprecation eprintln), `Some(x)` for `--persist x`, error if both set, `None` if neither.

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn resolve_persist_flag_maps_tmux_and_rejects_conflict() {
    let mut a = crate::cmd_resume::test_args("claude-x"); // helper builds a minimal ResumeArgs
    a.remote = Some("ssh://h".into());
    a.tmux = true;
    assert_eq!(resolve_persist_flag(&a).unwrap(), Some(PersistBackend::Tmux));

    a.persist = Some(PersistBackend::Dtach);
    assert!(resolve_persist_flag(&a).is_err()); // both --tmux and --persist

    a.tmux = false;
    assert_eq!(resolve_persist_flag(&a).unwrap(), Some(PersistBackend::Dtach));
}
```
(If no `test_args` helper exists, construct `ResumeArgs { .. }` inline with all fields — check the struct's current fields first.)

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p path-cli --lib resolve_persist_flag_maps_tmux_and_rejects_conflict`
Expected: FAIL — fields/fn missing.

- [ ] **Step 3: Implement**

Add fields to `ResumeArgs`:
```rust
/// Remote session-persistence backend. Skips the picker. Requires --remote.
#[arg(long, value_enum, requires = "remote")]
pub persist: Option<PersistBackend>,

/// Transport for the interactive launch. ssh (default); mosh/et reserved.
#[arg(long, value_enum, default_value_t = Transport::Ssh, requires = "remote")]
pub via: Transport,
```
Add the resolver:
```rust
fn resolve_persist_flag(args: &ResumeArgs) -> Result<Option<PersistBackend>> {
    match (args.tmux, args.persist) {
        (true, Some(_)) => anyhow::bail!("--tmux is a deprecated alias for --persist tmux; don't combine it with --persist"),
        (true, None) => {
            eprintln!("note: --tmux is deprecated; use --persist tmux");
            Ok(Some(PersistBackend::Tmux))
        }
        (false, p) => Ok(p),
    }
}
```
Update every `ResumeArgs { … }` construction in tests to add `persist: None, via: Transport::Ssh,`.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p path-cli --lib resolve_persist_flag_maps_tmux_and_rejects_conflict && cargo test -p path-cli resume`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(resume): --persist and --via flags (+ --tmux deprecation alias)"
```

---

### Task 8: Backend candidate assembly + pre-selection (pure)

**Files:** Modify `crates/path-cli/src/cmd_resume.rs`; Test: same file.

**Interfaces:**
- Produces:
  ```rust
  fn persist_candidates(available: &std::collections::BTreeSet<String>) -> Vec<PersistBackend> // plain + available, in DISPLAY_ORDER
  fn preferred_backend(available: &std::collections::BTreeSet<String>) -> PersistBackend      // priority; plain if none
  ```

- [ ] **Step 1: Write the failing test**
```rust
#[test]
fn persist_candidates_and_preference() {
    use std::collections::BTreeSet;
    let avail: BTreeSet<String> = ["dtach", "zellij"].iter().map(|s| s.to_string()).collect();
    let cands = persist_candidates(&avail);
    // DISPLAY_ORDER filtered to available + always Plain, in order.
    assert_eq!(cands, vec![PersistBackend::Zellij, PersistBackend::Dtach, PersistBackend::Plain]);
    assert_eq!(preferred_backend(&avail), PersistBackend::Zellij); // tmux absent -> zellij

    let none: BTreeSet<String> = BTreeSet::new();
    assert_eq!(persist_candidates(&none), vec![PersistBackend::Plain]);
    assert_eq!(preferred_backend(&none), PersistBackend::Plain);

    let with_tmux: BTreeSet<String> = ["tmux", "shpool"].iter().map(|s| s.to_string()).collect();
    assert_eq!(preferred_backend(&with_tmux), PersistBackend::Tmux); // shpool never preferred over tmux
}
```

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p path-cli --lib persist_candidates_and_preference`
Expected: FAIL — not found.

- [ ] **Step 3: Implement**
```rust
fn persist_candidates(available: &std::collections::BTreeSet<String>) -> Vec<PersistBackend> {
    PersistBackend::DISPLAY_ORDER
        .into_iter()
        .filter(|b| match b.bin() {
            None => true, // Plain always offered
            Some(bin) => available.contains(bin),
        })
        .collect()
}

fn preferred_backend(available: &std::collections::BTreeSet<String>) -> PersistBackend {
    const PRIORITY: [PersistBackend; 4] = [
        PersistBackend::Tmux,
        PersistBackend::Zellij,
        PersistBackend::Abduco,
        PersistBackend::Dtach,
    ];
    PRIORITY
        .into_iter()
        .find(|b| b.bin().is_some_and(|bin| available.contains(bin)))
        .unwrap_or(PersistBackend::Plain)
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p path-cli --lib persist_candidates_and_preference`
Expected: PASS.

- [ ] **Step 5: Commit**
```bash
git add crates/path-cli/src/cmd_resume.rs
git commit -m "feat(resume): persist candidate assembly + preferred-backend selection"
```

---

### Task 9: Wire `run_remote` (probe → pick → plan → ship → note → launch)

**Files:** Modify `crates/path-cli/src/cmd_resume.rs` (`run_remote`); Test: same file + `crates/path-cli/tests/resume.rs`.

**Interfaces:**
- Consumes: everything above. Picker UI: for the interactive selection, mirror the existing harness picker (`crate::skim_picker` usage in `share`/`resume` — read `pick_harness`/its call site and follow the same pattern). Selection resolution:
  1. `resolve_persist_flag(args)?` → if `Some(b)`, use `b` (skip probe/picker).
  2. else probe `remote_which(&target, &bins_of_all_backends)`; `cands = persist_candidates(&avail)`.
  3. if interactive (stdin+stderr TTY): fuzzy-pick from `cands` with `preferred_backend(&avail)` pre-selected; else use `preferred_backend(&avail)` (print a note if it fell back to `Plain`).

- [ ] **Step 1: Write the failing integration test** (in `crates/path-cli/tests/resume.rs`, using `RecordingExec::with_available`)
```rust
#[test]
fn remote_resume_persist_dtach_records_launch_and_ships() {
    let rec = RecordingExec::with_available(["dtach"]);
    let mut args = /* build ResumeArgs for a file input */;
    args.remote = Some("ssh://h".into());
    args.harness = Some(Harness::Claude);
    args.persist = Some(PersistBackend::Dtach);
    run_with_strategy(&args, &rec).unwrap();
    let cap = rec.captured();
    assert_eq!(cap.binary, "ssh");
    assert!(cap.args.iter().any(|a| a.contains("dtach -A /tmp/path-dtach-")), "{:?}", cap.args);
}
```
(Model construction on the existing `remote_resume_*` integration tests in the same file.)

- [ ] **Step 2: Run to verify fail**

Run: `cargo test -p path-cli --test resume remote_resume_persist_dtach`
Expected: FAIL — `persist`/wiring absent.

- [ ] **Step 3: Implement**

In `run_remote`, after `let home = exec.remote_home(...)?` and before the ship step, resolve the backend:
```rust
let backend = match resolve_persist_flag(args)? {
    Some(b) => b,
    None => {
        let bins: Vec<&str> = PersistBackend::DISPLAY_ORDER.iter().filter_map(|b| b.bin()).collect();
        let avail = exec.remote_which(&target, &bins)
            .with_context(|| format!("probing persistence backends on {remote}"))?;
        let cands = persist_candidates(&avail);
        let preferred = preferred_backend(&avail);
        if io_is_interactive() { // stdin+stderr TTY, mirror existing picker guard
            pick_persist_backend(&cands, preferred)? // fuzzy UI mirroring pick_harness
        } else {
            if preferred == PersistBackend::Plain {
                eprintln!("note: no persistence backend on remote; launching plain (survives nothing)");
            }
            preferred
        }
    }
};
let plan = persist_plan(Harness::Claude, &session_id, launch_cwd.as_deref(), backend, &home);
```
Then: build `projects_dir`/ship JSONL as today, PLUS ship `plan.extra_file` (mkdir its parent, write it):
```rust
if let Some((path, body)) = &plan.extra_file {
    if let Some(parent) = std::path::Path::new(path).parent().and_then(|p| p.to_str()) {
        exec.remote_mkdirs(&target, parent).with_context(|| format!("creating {parent} on {remote}"))?;
    }
    exec.remote_write(&target, path, body).with_context(|| format!("shipping {path} to {remote}"))?;
}
if let Some(note) = &plan.post_note {
    eprintln!("{note}");
}
```
Finally replace the launch:
```rust
let (binary, argv) = launch_invocation(args.via, remote, &plan.remote_command)?;
```
Implement `pick_persist_backend` mirroring the harness picker (read `skim_picker.rs` + the `pick_harness` call site; present `describe()` rows, pre-select `preferred`). Implement `io_is_interactive()` if not already present (reuse the existing TTY check used by `p import`).

- [ ] **Step 4: Run to verify pass**

Run: `cargo test -p path-cli --test resume && cargo test -p path-cli --lib cmd_resume && cargo clippy -p path-cli -- -D warnings`
Expected: PASS + clean.

- [ ] **Step 5: Commit**
```bash
git add crates/path-cli/src/cmd_resume.rs crates/path-cli/tests/resume.rs
git commit -m "feat(resume): wire persistence picker + transport through run_remote"
```

---

### Task 10: Docs + version bump

**Files:** Modify `crates/path-cli/src/cmd_resume.rs` (module docs), `CLAUDE.md` (resume bullet), `docs/agents/remote-resume-targets.md`, `crates/path-cli/Cargo.toml`, root `Cargo.toml`, `site/_data/crates.json`, `CHANGELOG.md`.

- [ ] **Step 1: Update module docs + CLAUDE.md**

In `cmd_resume.rs` module docs, document `--persist <backend>` (six backends, three mechanisms), `--via ssh|mosh|et`, and that reachability rides `~/.ssh/config` aliases. In `CLAUDE.md`'s `path resume` bullet, add a sentence on the persistence picker + `--via`.

- [ ] **Step 2: Version bump** (per the release checklist)

Bump `path-cli` minor in `crates/path-cli/Cargo.toml`, root `Cargo.toml` `[workspace.dependencies]`, `site/_data/crates.json`, and add a `CHANGELOG.md` entry describing the persistence picker + `--via`.

- [ ] **Step 3: Verify build + full suite**

Run: `cargo test -p path-cli && cargo clippy --workspace -- -D warnings`
Expected: PASS + clean.

- [ ] **Step 4: Commit**
```bash
git add -A
git commit -m "docs(resume): document persistence picker + --via; bump path-cli"
```

---

## Self-Review

- **Spec coverage:** `--persist` 6 backends (Tasks 1–4, 8–9); 3 mechanisms — direct (T2), layout/zellij (T3), attach-only/shpool (T4); `--via ssh|mosh|et` seam (T5); probe (T6); flags + `--tmux` deprecation (T7); picker + pre-selection + non-TTY default (T8–9); reachability via alias (already shipped; documented T10); tests throughout; version bump (T10). Covered.
- **Open questions from spec** (zellij `--session … --layout` on existing session; libssh2 ProxyCommand ship fallback) are intentionally out of v1 — noted in spec, not tasks.
- **Type consistency:** `PersistBackend`, `PersistPlan { remote_command, extra_file, post_note }`, `Transport`, `persist_plan(harness, session_id, cwd, backend, home)`, `remote_which(target, bins) -> BTreeSet<String>`, `persist_candidates`/`preferred_backend`, `resolve_persist_flag`, `launch_invocation(transport, remote, remote_cmd)` — used consistently across tasks.
- **Placeholder scan:** the only forward-refs are the Task-2 zellij/shpool stubs, explicitly replaced in Tasks 3–4; `pick_persist_backend`/`io_is_interactive` are specified to mirror the existing harness picker (named files to read at execution).
