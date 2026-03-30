//! Pure redaction logic: given step entries with redaction state, produce a redacted document.

use std::collections::{HashMap, HashSet};

use toolpath::v1;

use crate::model::StepEntry;

/// Build a redacted toolpath document (a single-path [`v1::Graph`]) from the
/// original path and the entries with redaction state.
pub fn build_redacted_document(original: &v1::Path, entries: &[StepEntry]) -> v1::Graph {
    let mut output_steps: Vec<v1::Step> = Vec::new();
    let mut id_remap: HashMap<String, String> = HashMap::new();

    let mut i = 0;
    while i < entries.len() {
        if entries[i].included {
            let mut step = entries[i].step.clone();
            apply_text_redactions(&mut step, &entries[i].text_redactions);
            id_remap.insert(step.step.id.clone(), step.step.id.clone());
            output_steps.push(step);
            i += 1;
        } else {
            // Group consecutive excluded entries.
            let group_start = i;
            while i < entries.len() && !entries[i].included {
                i += 1;
            }
            let group = &entries[group_start..i];
            let count = group.len();

            let first = &group[0].step;
            let placeholder_id = format!("redacted-{}", first.step.id);

            // Map all excluded step IDs to the placeholder.
            for entry in group {
                id_remap.insert(entry.step.step.id.clone(), placeholder_id.clone());
            }

            let placeholder = build_placeholder_step(&placeholder_id, first, count);
            output_steps.push(placeholder);
        }
    }

    // Rewrite parent references.
    for step in &mut output_steps {
        step.step.parents = step
            .step
            .parents
            .iter()
            .map(|p| id_remap.get(p).cloned().unwrap_or_else(|| p.clone()))
            .collect();
        // Deduplicate parents (multiple excluded parents may map to same placeholder).
        let mut seen = HashSet::new();
        step.step.parents.retain(|p| seen.insert(p.clone()));
    }

    // Update head.
    let head = output_steps
        .last()
        .map(|s| s.step.id.clone())
        .unwrap_or_else(|| original.path.head.clone());

    // Build path meta with "(redacted)" suffix.
    let meta = original.meta.clone().map(|mut m| {
        let title = m.title.unwrap_or_default();
        m.title = Some(format!("{title} (redacted)"));
        m
    });

    let path = v1::Path {
        path: v1::PathIdentity {
            id: original.path.id.clone(),
            base: original.path.base.clone(),
            head,
            graph_ref: original.path.graph_ref.clone(),
        },
        steps: output_steps,
        meta,
    };

    v1::Graph::from_path(path)
}

fn build_placeholder_step(id: &str, first_excluded: &v1::Step, count: usize) -> v1::Step {
    let mut change = HashMap::new();
    let mut extra = HashMap::new();
    extra.insert("count".to_string(), serde_json::json!(count));
    change.insert(
        "toolpath://redaction".to_string(),
        v1::ArtifactChange {
            raw: None,
            structural: Some(v1::StructuralChange {
                change_type: "redaction".to_string(),
                extra,
            }),
        },
    );

    v1::Step {
        step: v1::StepIdentity {
            id: id.to_string(),
            parents: first_excluded.step.parents.clone(),
            actor: "tool:redact".to_string(),
            timestamp: first_excluded.step.timestamp.clone(),
        },
        change,
        meta: Some(v1::StepMeta {
            intent: Some(format!("[redacted] ({count} steps redacted)")),
            ..Default::default()
        }),
    }
}

/// Apply text redactions to a step by working on its JSON representation.
fn apply_text_redactions(step: &mut v1::Step, redactions: &[crate::model::TextRedaction]) {
    if redactions.is_empty() {
        return;
    }

    let mut value = match serde_json::to_value(&*step) {
        Ok(v) => v,
        Err(_) => return,
    };

    // Group redactions by json_pointer and sort each group by offset descending.
    let mut by_pointer: HashMap<&str, Vec<&crate::model::TextRedaction>> = HashMap::new();
    for r in redactions {
        by_pointer
            .entry(r.json_pointer.as_str())
            .or_default()
            .push(r);
    }

    for (pointer, group) in by_pointer {
        // Sort by start offset ascending, then merge overlapping/adjacent ranges.
        let mut ranges: Vec<(usize, usize)> = group.iter().map(|r| (r.start, r.end)).collect();
        ranges.sort();
        let mut merged: Vec<(usize, usize)> = Vec::new();
        for (s, e) in ranges {
            if let Some(last) = merged.last_mut()
                && s <= last.1
            {
                // Overlapping or adjacent — merge.
                last.1 = last.1.max(e);
                continue;
            }
            merged.push((s, e));
        }

        // Apply from the end so earlier offsets remain valid.
        merged.reverse();

        if let Some(target) = value.pointer_mut(pointer)
            && let Some(s) = target.as_str().map(|s| s.to_string())
        {
            let mut result = s;
            for (start, end) in &merged {
                if *start <= result.len() && *end <= result.len() && start <= end {
                    let char_count = end - start;
                    let replacement = format!("[REDACTED({char_count})]");
                    result.replace_range(*start..*end, &replacement);
                }
            }
            *target = serde_json::Value::String(result);
        }
    }

    if let Ok(modified) = serde_json::from_value(value) {
        *step = modified;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TextRedaction;
    use std::collections::HashMap;
    use toolpath::v1;

    fn make_step(id: &str, actor: &str, parents: &[&str]) -> v1::Step {
        let mut step = v1::Step::new(id, actor, "2026-01-29T10:00:00Z");
        for p in parents {
            step = step.with_parent(*p);
        }
        step = step.with_raw_change("file.rs", "@@ test diff @@");
        step
    }

    fn make_entry(step: v1::Step, included: bool) -> StepEntry {
        StepEntry {
            index: 0,
            summary: String::new(),
            actor_label: String::new(),
            step,
            included,
            expanded: false,
            text_redactions: Vec::new(),
        }
    }

    fn make_path(steps: Vec<v1::Step>) -> v1::Path {
        let head = steps.last().map(|s| s.step.id.clone()).unwrap_or_default();
        v1::Path {
            path: v1::PathIdentity {
                id: "test-path".to_string(),
                base: None,
                head,
                graph_ref: None,
            },
            steps,
            meta: Some(v1::PathMeta {
                title: Some("Test session".to_string()),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn no_redactions() {
        let steps = vec![
            make_step("s1", "human:user", &[]),
            make_step("s2", "agent:claude", &["s1"]),
        ];
        let path = make_path(steps.clone());
        let entries: Vec<_> = steps.into_iter().map(|s| make_entry(s, true)).collect();

        let doc = build_redacted_document(&path, &entries);
        match doc.into_single_path() {
            Some(p) => {
                assert_eq!(p.steps.len(), 2);
                assert_eq!(p.steps[0].step.id, "s1");
                assert_eq!(p.steps[1].step.id, "s2");
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn exclude_single_step() {
        let steps = vec![
            make_step("s1", "human:user", &[]),
            make_step("s2", "agent:claude", &["s1"]),
            make_step("s3", "human:user", &["s2"]),
        ];
        let path = make_path(steps.clone());
        let mut entries: Vec<_> = steps.into_iter().map(|s| make_entry(s, true)).collect();
        entries[1].included = false;

        let doc = build_redacted_document(&path, &entries);
        match doc.into_single_path() {
            Some(p) => {
                assert_eq!(p.steps.len(), 3);
                assert_eq!(p.steps[0].step.id, "s1");
                assert_eq!(p.steps[1].step.id, "redacted-s2");
                assert_eq!(p.steps[1].step.actor, "tool:redact");
                assert_eq!(p.steps[2].step.id, "s3");
                // s3's parent should point to the placeholder.
                assert_eq!(p.steps[2].step.parents, vec!["redacted-s2"]);
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn exclude_consecutive_steps() {
        let steps = vec![
            make_step("s1", "human:user", &[]),
            make_step("s2", "agent:claude", &["s1"]),
            make_step("s3", "agent:claude", &["s2"]),
            make_step("s4", "agent:claude", &["s3"]),
            make_step("s5", "human:user", &["s4"]),
        ];
        let path = make_path(steps.clone());
        let mut entries: Vec<_> = steps.into_iter().map(|s| make_entry(s, true)).collect();
        entries[1].included = false;
        entries[2].included = false;
        entries[3].included = false;

        let doc = build_redacted_document(&path, &entries);
        match doc.into_single_path() {
            Some(p) => {
                assert_eq!(p.steps.len(), 3); // s1, placeholder, s5
                assert_eq!(p.steps[1].step.id, "redacted-s2");
                // Check placeholder intent.
                let intent = p.steps[1].meta.as_ref().unwrap().intent.as_ref().unwrap();
                assert_eq!(intent, "[redacted] (3 steps redacted)");
                // s5's parent should be the placeholder.
                assert_eq!(p.steps[2].step.parents, vec!["redacted-s2"]);
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn exclude_non_consecutive() {
        let steps = vec![
            make_step("s1", "human:user", &[]),
            make_step("s2", "agent:claude", &["s1"]),
            make_step("s3", "human:user", &["s2"]),
            make_step("s4", "agent:claude", &["s3"]),
            make_step("s5", "human:user", &["s4"]),
        ];
        let path = make_path(steps.clone());
        let mut entries: Vec<_> = steps.into_iter().map(|s| make_entry(s, true)).collect();
        entries[1].included = false; // s2
        entries[3].included = false; // s4

        let doc = build_redacted_document(&path, &entries);
        match doc.into_single_path() {
            Some(p) => {
                assert_eq!(p.steps.len(), 5); // s1, placeholder1, s3, placeholder2, s5
                assert_eq!(p.steps[1].step.id, "redacted-s2");
                assert_eq!(p.steps[3].step.id, "redacted-s4");
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn parent_rewriting_simple() {
        let steps = vec![
            make_step("s1", "human:user", &[]),
            make_step("s2", "agent:claude", &["s1"]),
            make_step("s3", "human:user", &["s2"]),
        ];
        let path = make_path(steps.clone());
        let mut entries: Vec<_> = steps.into_iter().map(|s| make_entry(s, true)).collect();
        entries[1].included = false;

        let doc = build_redacted_document(&path, &entries);
        match doc.into_single_path() {
            Some(p) => {
                // s3's parent was s2, should now point to placeholder.
                assert_eq!(p.steps[2].step.parents, vec!["redacted-s2"]);
                // Placeholder's parent should be s1 (the original parent of s2).
                assert_eq!(p.steps[1].step.parents, vec!["s1"]);
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn head_rewriting() {
        let steps = vec![
            make_step("s1", "human:user", &[]),
            make_step("s2", "agent:claude", &["s1"]),
            make_step("s3", "human:user", &["s2"]),
        ];
        let path = make_path(steps.clone());
        let mut entries: Vec<_> = steps.into_iter().map(|s| make_entry(s, true)).collect();
        entries[2].included = false; // exclude the last step

        let doc = build_redacted_document(&path, &entries);
        match doc.into_single_path() {
            Some(p) => {
                assert_eq!(p.path.head, "redacted-s3");
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn text_redaction_basic() {
        let mut step = make_step("s1", "human:user", &[]);
        step = step.with_intent("Fix the secret API key abc123 in config");
        let path = make_path(vec![step.clone()]);

        let mut entry = make_entry(step, true);
        entry.text_redactions.push(TextRedaction {
            json_pointer: "/meta/intent".to_string(),
            start: 25,
            end: 31,
            original: "abc123".to_string(),
        });

        let doc = build_redacted_document(&path, &[entry]);
        match doc.into_single_path() {
            Some(p) => {
                let intent = p.steps[0].meta.as_ref().unwrap().intent.as_ref().unwrap();
                assert!(intent.contains("[REDACTED(6)]"));
                assert!(!intent.contains("abc123"));
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn text_redaction_multiple_in_same_field() {
        let mut step = make_step("s1", "human:user", &[]);
        step = step.with_intent("key=SECRET1 and token=SECRET2");
        let path = make_path(vec![step.clone()]);

        let mut entry = make_entry(step, true);
        entry.text_redactions.push(TextRedaction {
            json_pointer: "/meta/intent".to_string(),
            start: 4,
            end: 11,
            original: "SECRET1".to_string(),
        });
        entry.text_redactions.push(TextRedaction {
            json_pointer: "/meta/intent".to_string(),
            start: 22,
            end: 29,
            original: "SECRET2".to_string(),
        });

        let doc = build_redacted_document(&path, &[entry]);
        match doc.into_single_path() {
            Some(p) => {
                let intent = p.steps[0].meta.as_ref().unwrap().intent.as_ref().unwrap();
                assert!(intent.contains("[REDACTED(7)]"));
                assert!(!intent.contains("SECRET1"));
                assert!(!intent.contains("SECRET2"));
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn text_redaction_in_diff() {
        let step = v1::Step::new("s1", "human:user", "2026-01-29T10:00:00Z").with_raw_change(
            "file.rs",
            "--- a\n+++ b\n@@ -1 +1 @@\n-secret_key\n+new_line",
        );
        let path = make_path(vec![step.clone()]);

        let mut entry = make_entry(step, true);
        entry.text_redactions.push(TextRedaction {
            json_pointer: "/change/file.rs/raw".to_string(),
            start: 30,
            end: 40,
            original: "secret_key".to_string(),
        });

        let doc = build_redacted_document(&path, &[entry]);
        match doc.into_single_path() {
            Some(p) => {
                let raw = p.steps[0].change["file.rs"].raw.as_ref().unwrap();
                assert!(raw.contains("[REDACTED(10)]"));
                assert!(!raw.contains("secret_key"));
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn text_redaction_deep_field() {
        let mut step = make_step("s1", "human:user", &[]);
        // Add a structural change with extra fields.
        let mut extra = HashMap::new();
        extra.insert(
            "text".to_string(),
            serde_json::json!("my secret password here"),
        );
        step.change.insert(
            "conversation".to_string(),
            v1::ArtifactChange {
                raw: None,
                structural: Some(v1::StructuralChange {
                    change_type: "conversation.append".to_string(),
                    extra,
                }),
            },
        );
        let path = make_path(vec![step.clone()]);

        let mut entry = make_entry(step, true);
        entry.text_redactions.push(TextRedaction {
            json_pointer: "/change/conversation/structural/text".to_string(),
            start: 10,
            end: 18,
            original: "password".to_string(),
        });

        let doc = build_redacted_document(&path, &[entry]);
        match doc.into_single_path() {
            Some(p) => {
                let structural = p.steps[0].change["conversation"]
                    .structural
                    .as_ref()
                    .unwrap();
                let text = structural.extra["text"].as_str().unwrap();
                assert!(text.contains("[REDACTED(8)]"));
                assert!(!text.contains("password"));
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn text_redaction_preserves_other_fields() {
        let step = v1::Step::new("s1", "human:user", "2026-01-29T10:00:00Z")
            .with_raw_change("a.rs", "diff-a")
            .with_raw_change("b.rs", "diff-b")
            .with_intent("keep this intent");
        let path = make_path(vec![step.clone()]);

        let mut entry = make_entry(step, true);
        entry.text_redactions.push(TextRedaction {
            json_pointer: "/change/a.rs/raw".to_string(),
            start: 0,
            end: 6,
            original: "diff-a".to_string(),
        });

        let doc = build_redacted_document(&path, &[entry]);
        match doc.into_single_path() {
            Some(p) => {
                // a.rs should be redacted.
                let raw_a = p.steps[0].change["a.rs"].raw.as_ref().unwrap();
                assert!(raw_a.contains("[REDACTED(6)]"));
                // b.rs should be untouched.
                let raw_b = p.steps[0].change["b.rs"].raw.as_ref().unwrap();
                assert_eq!(raw_b, "diff-b");
                // Intent should be untouched.
                let intent = p.steps[0].meta.as_ref().unwrap().intent.as_ref().unwrap();
                assert_eq!(intent, "keep this intent");
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn combined_step_and_text_redaction() {
        let steps = vec![
            make_step("s1", "human:user", &[]),
            make_step("s2", "agent:claude", &["s1"]),
            v1::Step::new("s3", "human:user", "2026-01-29T10:00:00Z")
                .with_parent("s2")
                .with_raw_change("file.rs", "contains SECRET here"),
        ];
        let path = make_path(steps.clone());
        let mut entries: Vec<_> = steps.into_iter().map(|s| make_entry(s, true)).collect();
        entries[1].included = false; // exclude s2
        entries[2].text_redactions.push(TextRedaction {
            json_pointer: "/change/file.rs/raw".to_string(),
            start: 9,
            end: 15,
            original: "SECRET".to_string(),
        });

        let doc = build_redacted_document(&path, &entries);
        match doc.into_single_path() {
            Some(p) => {
                assert_eq!(p.steps.len(), 3); // s1, placeholder, s3
                assert_eq!(p.steps[1].step.actor, "tool:redact");
                let raw = p.steps[2].change["file.rs"].raw.as_ref().unwrap();
                assert!(raw.contains("[REDACTED(6)]"));
                assert!(!raw.contains("SECRET"));
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn all_steps_excluded() {
        let steps = vec![
            make_step("s1", "human:user", &[]),
            make_step("s2", "agent:claude", &["s1"]),
        ];
        let path = make_path(steps.clone());
        let entries: Vec<_> = steps.into_iter().map(|s| make_entry(s, false)).collect();

        let doc = build_redacted_document(&path, &entries);
        match doc.into_single_path() {
            Some(p) => {
                assert_eq!(p.steps.len(), 1);
                assert_eq!(p.steps[0].step.id, "redacted-s1");
                assert_eq!(p.path.head, "redacted-s1");
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn overlapping_redactions_merge() {
        let mut step = make_step("s1", "human:user", &[]);
        step = step.with_intent("the SECRET_KEY_VALUE is here");
        let path = make_path(vec![step.clone()]);

        let mut entry = make_entry(step, true);
        // Two overlapping redactions: "SECRET_KEY" (4..14) and "KEY_VALUE" (11..20)
        entry.text_redactions.push(TextRedaction {
            json_pointer: "/meta/intent".to_string(),
            start: 4,
            end: 14,
            original: "SECRET_KEY".to_string(),
        });
        entry.text_redactions.push(TextRedaction {
            json_pointer: "/meta/intent".to_string(),
            start: 11,
            end: 20,
            original: "KEY_VALUE".to_string(),
        });

        let doc = build_redacted_document(&path, &[entry]);
        match doc.into_single_path() {
            Some(p) => {
                let intent = p.steps[0].meta.as_ref().unwrap().intent.as_ref().unwrap();
                // Should merge into one [REDACTED(16)] covering chars 4..20.
                assert!(intent.contains("[REDACTED(16)]"));
                assert!(!intent.contains("SECRET"));
                assert!(!intent.contains("KEY"));
                assert!(!intent.contains("VALUE"));
                // Should only have ONE redaction marker, not two.
                assert_eq!(intent.matches("[REDACTED").count(), 1);
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn adjacent_redactions_merge() {
        let mut step = make_step("s1", "human:user", &[]);
        step = step.with_intent("AABBCC");
        let path = make_path(vec![step.clone()]);

        let mut entry = make_entry(step, true);
        // Adjacent: "AA" (0..2) and "BB" (2..4)
        entry.text_redactions.push(TextRedaction {
            json_pointer: "/meta/intent".to_string(),
            start: 0,
            end: 2,
            original: "AA".to_string(),
        });
        entry.text_redactions.push(TextRedaction {
            json_pointer: "/meta/intent".to_string(),
            start: 2,
            end: 4,
            original: "BB".to_string(),
        });

        let doc = build_redacted_document(&path, &[entry]);
        match doc.into_single_path() {
            Some(p) => {
                let intent = p.steps[0].meta.as_ref().unwrap().intent.as_ref().unwrap();
                // Should merge into one [REDACTED(4)] covering chars 0..4.
                assert!(intent.contains("[REDACTED(4)]"));
                assert_eq!(intent.matches("[REDACTED").count(), 1);
                assert!(intent.contains("CC")); // "CC" should remain
            }
            None => panic!("expected Path"),
        }
    }

    #[test]
    fn roundtrip_serialization() {
        let steps = vec![
            make_step("s1", "human:user", &[]),
            make_step("s2", "agent:claude", &["s1"]),
            make_step("s3", "human:user", &["s2"]),
        ];
        let path = make_path(steps.clone());
        let mut entries: Vec<_> = steps.into_iter().map(|s| make_entry(s, true)).collect();
        entries[1].included = false;

        let doc = build_redacted_document(&path, &entries);
        let json = doc.to_json().unwrap();
        let parsed = v1::Graph::from_json(&json).unwrap();
        match parsed.into_single_path() {
            Some(p) => {
                assert_eq!(p.steps.len(), 3);
                assert_eq!(p.steps[1].step.actor, "tool:redact");
            }
            None => panic!("expected Path"),
        }
    }
}
