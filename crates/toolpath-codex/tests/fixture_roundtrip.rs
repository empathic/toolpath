//! End-to-end check: parse the real recorded Codex Python-runtime
//! session, run it through the provider + derive, and verify the
//! output covers everything important.

use std::path::PathBuf;
use toolpath_codex::provider::to_view;
use toolpath_codex::{CodexConvo, PathResolver, RolloutReader, derive};
use toolpath_convo::{Role, ToolCategory};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-codex-python.jsonl")
}

fn session() -> toolpath_codex::Session {
    RolloutReader::read_session(fixture_path()).unwrap()
}

#[test]
fn fixture_parses() {
    let s = session();
    assert_eq!(s.lines.len(), 138);
    let meta = s.meta().expect("session_meta missing");
    assert_eq!(meta.originator, "codex-tui");
    assert!(meta.git.is_some());
}

#[test]
fn view_has_expected_turn_count() {
    let s = session();
    let view = to_view(&s);
    // From prior inspection: 1 user + 1 developer + 10 assistant messages.
    assert!(
        view.turns.len() >= 10 && view.turns.len() <= 14,
        "expected 10-14 turns, got {}",
        view.turns.len()
    );
    let users = view.turns.iter().filter(|t| t.role == Role::User).count();
    let assistants = view
        .turns
        .iter()
        .filter(|t| t.role == Role::Assistant)
        .count();
    let system = view.turns.iter().filter(|t| t.role == Role::System).count();
    // The fixture has two user messages: the actual prompt plus a
    // `function_call_output`-style carrier that encodes tool output.
    // Accept either 1 or 2 so the test stays robust across wire variants.
    assert!(
        (1..=2).contains(&users),
        "expected 1-2 user turns, got {}",
        users
    );
    assert!(
        assistants >= 8,
        "expected ≥8 assistant turns, got {}",
        assistants
    );
    assert!(system >= 1, "expected at least the developer message");
}

#[test]
fn tool_calls_pair_correctly() {
    let s = session();
    let view = to_view(&s);
    let total_tools: usize = view.turns.iter().map(|t| t.tool_uses.len()).sum();
    assert!(total_tools > 0);
    let with_result: usize = view
        .turns
        .iter()
        .flat_map(|t| &t.tool_uses)
        .filter(|tu| tu.result.is_some())
        .count();
    let ratio = with_result as f64 / total_tools as f64;
    assert!(
        ratio > 0.8,
        "tool pairing ratio too low: {}/{}",
        with_result,
        total_tools
    );
}

#[test]
fn exec_commands_surface_as_shell_category() {
    let s = session();
    let view = to_view(&s);
    let shell_calls: Vec<&toolpath_convo::ToolInvocation> = view
        .turns
        .iter()
        .flat_map(|t| &t.tool_uses)
        .filter(|tu| tu.category == Some(ToolCategory::Shell))
        .collect();
    assert!(
        !shell_calls.is_empty(),
        "expected at least one exec_command tool invocation"
    );
}

#[test]
fn apply_patch_preserved() {
    let s = session();
    let view = to_view(&s);
    let apply_patches: Vec<&toolpath_convo::ToolInvocation> = view
        .turns
        .iter()
        .flat_map(|t| &t.tool_uses)
        .filter(|tu| tu.name == "apply_patch")
        .collect();
    assert!(
        !apply_patches.is_empty(),
        "session should have at least one apply_patch"
    );
    let first = &apply_patches[0];
    let input = first.input.as_str().unwrap();
    assert!(input.contains("*** Begin Patch") || input.contains("*** Add File"));
}

#[test]
fn files_changed_matches_patch_events() {
    let s = session();
    let view = to_view(&s);
    assert!(!view.files_changed.is_empty());
    let expected = ["Cargo.toml", "src/lib.rs", "src/main.rs", "src/runtime.rs"];
    for basename in expected {
        let found = view.files_changed.iter().any(|p| p.ends_with(basename));
        assert!(
            found,
            "expected files_changed to include {}; got: {:?}",
            basename, view.files_changed
        );
    }
}

#[test]
fn token_usage_captured() {
    let s = session();
    let view = to_view(&s);
    let u = view.total_usage.expect("total_usage missing");
    assert!(u.input_tokens.unwrap_or(0) > 0);
    assert!(u.output_tokens.unwrap_or(0) > 0);
    // Reasoning is surfaced under breakdowns["output"]["reasoning"], derived
    // by differencing the cumulative `reasoning_output_tokens` counter — never
    // raw-summed. The fixture's final cumulative reasoning is 979 ≤ output
    // 11929, so the session total breakdown must match and respect the
    // reasoning ⊆ output invariant.
    let session_reasoning = u
        .breakdowns
        .get("output")
        .and_then(|m| m.get("reasoning"))
        .copied()
        .expect("session reasoning breakdown present");
    assert_eq!(session_reasoning, 979);
    assert!(session_reasoning <= u.output_tokens.unwrap());
}

#[test]
fn reasoning_breakdown_differenced_dedup_safe_against_real_fixture() {
    use toolpath_convo::TokenUsage;
    let s = session();
    let view = to_view(&s);

    let reasoning_of = |u: Option<&TokenUsage>| -> u32 {
        u.and_then(|u| u.breakdowns.get("output"))
            .and_then(|m| m.get("reasoning"))
            .copied()
            .unwrap_or(0)
    };

    // Sum of per-step attributed reasoning across the whole session must equal
    // the final cumulative reasoning (979). If differencing were unsafe —
    // summing the twice-emitted counts, or stamping the cumulative — this would
    // overshoot. This is the dedup-safe / no-double-count proof on real data.
    let attributed_reasoning: u32 = view
        .turns
        .iter()
        .map(|t| reasoning_of(t.attributed_token_usage.as_ref()))
        .sum();
    assert_eq!(
        attributed_reasoning, 979,
        "Σ attributed reasoning != cumulative"
    );

    // Per step, reasoning ⊆ output.
    for t in &view.turns {
        if let Some(a) = t.attributed_token_usage.as_ref() {
            let r = reasoning_of(Some(a));
            assert!(
                r <= a.output_tokens.unwrap_or(0),
                "step reasoning {} exceeds output {:?}",
                r,
                a.output_tokens
            );
        }
    }

    // Round (group) totals: Σ over group token_usage reasoning == 979 too, and
    // each round's reasoning ⊆ its output.
    let round_reasoning: u32 = view
        .turns
        .iter()
        .filter(|t| t.token_usage.is_some())
        .map(|t| {
            let u = t.token_usage.as_ref().unwrap();
            let r = reasoning_of(Some(u));
            assert!(r <= u.output_tokens.unwrap_or(0));
            r
        })
        .sum();
    assert_eq!(
        round_reasoning, 979,
        "Σ round-total reasoning != cumulative"
    );
}

#[test]
fn encrypted_reasoning_does_not_land_on_thinking() {
    // Codex rollouts almost always carry OpenAI's encrypted reasoning
    // ciphertext — not plaintext. With `Turn.extra` gone, the ciphertext is
    // dropped on the conversion to the IR; the invariant we still preserve
    // is that it must not pollute `turn.thinking` (which would render as
    // garbage).
    let s = session();
    let view = to_view(&s);
    let with_thinking = view.turns.iter().filter(|t| t.thinking.is_some()).count();
    assert_eq!(
        with_thinking, 0,
        "encrypted reasoning must not land on turn.thinking"
    );
}

#[test]
fn events_preserve_non_turn_content() {
    let s = session();
    let view = to_view(&s);
    let has_turn_context = view.events.iter().any(|e| e.event_type == "turn_context");
    let has_task_started = view.events.iter().any(|e| e.event_type == "task_started");
    let has_task_complete = view.events.iter().any(|e| e.event_type == "task_complete");
    let has_patch_apply = view
        .events
        .iter()
        .any(|e| e.event_type == "patch_apply_end");
    assert!(has_turn_context);
    assert!(has_task_started);
    assert!(has_task_complete);
    assert!(has_patch_apply);
}

#[test]
fn derive_path_produces_file_artifacts_with_raw_diffs() {
    let s = session();
    let path = derive::derive_path(&s, &derive::DeriveConfig::default());

    let convo_prefix = "codex://";
    let file_artifacts: Vec<(&str, &toolpath::v1::ArtifactChange)> = path
        .steps
        .iter()
        .flat_map(|s| s.change.iter())
        .filter(|(k, _)| !k.starts_with(convo_prefix))
        .map(|(k, v)| (k.as_str(), v))
        .collect();

    assert!(!file_artifacts.is_empty(), "no file artifacts emitted");

    for (path, change) in &file_artifacts {
        assert!(
            change.raw.is_some(),
            "file artifact {} missing raw perspective",
            path
        );
        assert!(
            change.structural.is_some(),
            "file artifact {} missing structural perspective",
            path
        );
    }
}

#[test]
fn derive_path_validates_as_path_document() {
    let s = session();
    let path = derive::derive_path(&s, &derive::DeriveConfig::default());
    let doc = toolpath::v1::Graph::from_path(path);
    let json = doc.to_json().unwrap();
    let parsed = toolpath::v1::Graph::from_json(&json).unwrap();
    let p = parsed.single_path().expect("single-path graph");
    assert!(!p.steps.is_empty());
    let anc = toolpath::v1::query::ancestors(&p.steps, &p.path.head);
    assert_eq!(anc.len(), p.steps.len(), "all steps on head ancestry");
}

#[test]
fn list_sessions_via_convo() {
    let temp = tempfile::tempdir().unwrap();
    let codex = temp.path().join(".codex");
    let day = codex.join("sessions/2026/04/20");
    std::fs::create_dir_all(&day).unwrap();
    let dst = day.join("rollout-2026-04-20T12-43-30-019dabc6-8fef-7681-a054-b5bb75fcb97d.jsonl");
    std::fs::copy(fixture_path(), &dst).unwrap();

    let resolver = PathResolver::new().with_codex_dir(&codex);
    let mgr = CodexConvo::with_resolver(resolver);
    let sessions = mgr.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].line_count, 138);
    assert!(sessions[0].first_user_message.is_some());
}
