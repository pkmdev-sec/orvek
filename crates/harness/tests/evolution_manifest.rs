use orvek_harness::{Digest, ManifestError, ValidatedHarnessRevision};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn parent() -> Digest {
    Digest::of(b"trusted parent")
}

fn behavior() -> Value {
    json!({
        "instructions": "Work carefully and verify externally meaningful behavior.",
        "skills": {
            "review": "Inspect the relevant evidence before reporting a conclusion.",
            "verify": "Run the narrow verifier before completing the task."
        },
        "recovery_reminders": [
            "inspect_status_before_retry",
            "preserve_pinned_revision"
        ],
        "subagent_roles": {
            "reader": {
                "instructions": "Collect bounded evidence and return cited findings.",
                "tools": ["read_file", "search"]
            }
        },
        "verifier": {
            "after_tool_calls": 8,
            "before_completion": true
        },
        "budgets": {
            "tool_calls": 64,
            "subagents": 4,
            "verifier_runs": 16,
            "output_bytes": 1048576,
            "tokens": 200000,
            "elapsed_ms": 600000
        }
    })
}

fn revision() -> ValidatedHarnessRevision {
    ValidatedHarnessRevision::from_behavior_json(
        parent(),
        &serde_json::to_vec(&behavior()).unwrap(),
    )
    .unwrap()
}

#[test]
fn canonical_manifest_round_trips_with_stable_identities() {
    let original = revision();
    let decoded = ValidatedHarnessRevision::from_manifest_json(original.canonical_bytes()).unwrap();

    assert_eq!(decoded, original);
    assert_eq!(decoded.digest(), original.digest());
    assert_eq!(decoded.behavior_digest(), original.behavior_digest());
    assert_eq!(decoded.envelope_digest(), original.envelope_digest());
    assert_eq!(decoded.policy_id(), "behavior-v1");
    assert_eq!(decoded.canonical_bytes(), original.canonical_bytes());
}

#[test]
fn equivalent_map_orderings_have_identical_canonical_bytes() {
    let first = br#"{
        "instructions":"Be precise.",
        "skills":{"alpha":"First skill.","zeta":"Last skill."},
        "recovery_reminders":["preserve_pinned_revision","inspect_status_before_retry"],
        "subagent_roles":{
            "alpha":{"instructions":"First role.","tools":["search","read_file"]},
            "zeta":{"instructions":"Last role.","tools":["write_file","read_file"]}
        },
        "verifier":{"after_tool_calls":2,"before_completion":true},
        "budgets":{"tool_calls":4,"subagents":2,"verifier_runs":3,"output_bytes":1024,"tokens":2048,"elapsed_ms":1000}
    }"#;
    let second = br#"{
        "budgets":{"elapsed_ms":1000,"tokens":2048,"output_bytes":1024,"verifier_runs":3,"subagents":2,"tool_calls":4},
        "verifier":{"before_completion":true,"after_tool_calls":2},
        "subagent_roles":{
            "zeta":{"tools":["read_file","write_file"],"instructions":"Last role."},
            "alpha":{"tools":["read_file","search"],"instructions":"First role."}
        },
        "recovery_reminders":["inspect_status_before_retry","preserve_pinned_revision"],
        "skills":{"zeta":"Last skill.","alpha":"First skill."},
        "instructions":"Be precise."
    }"#;

    let first = ValidatedHarnessRevision::from_behavior_json(parent(), first).unwrap();
    let second = ValidatedHarnessRevision::from_behavior_json(parent(), second).unwrap();

    assert_eq!(first.digest(), second.digest());
    assert_eq!(first.canonical_bytes(), second.canonical_bytes());
}

#[test]
fn patch_is_bound_to_previous_revision_and_preserves_compiled_policy() {
    let original = revision();
    let patch = br#"{"instructions":"Use the new behavior and verify it."}"#;
    let updated = original.apply_patch_json(patch).unwrap();

    assert_eq!(updated.parent(), original.digest());
    assert_ne!(updated.digest(), original.digest());
    assert_ne!(updated.behavior_digest(), original.behavior_digest());
    assert_eq!(updated.envelope_digest(), original.envelope_digest());
    assert_eq!(updated.policy_id(), original.policy_id());
}

#[test]
fn empty_patch_is_rejected() {
    assert!(matches!(
        revision().apply_patch_json(br#"{}"#),
        Err(ManifestError::EmptyPatch)
    ));
}

#[test]
fn manifest_identity_tampering_is_rejected() {
    let original = revision();
    let mut manifest: Value = serde_json::from_slice(original.canonical_bytes()).unwrap();

    manifest["behavior_digest"] = json!(Digest::of(b"different behavior").to_string());
    assert!(matches!(
        ValidatedHarnessRevision::from_manifest_json(&serde_json::to_vec(&manifest).unwrap()),
        Err(ManifestError::IdentityMismatch {
            identity: "behavior"
        })
    ));

    manifest = serde_json::from_slice(original.canonical_bytes()).unwrap();
    manifest["envelope_digest"] = json!(Digest::of(b"different envelope").to_string());
    assert!(matches!(
        ValidatedHarnessRevision::from_manifest_json(&serde_json::to_vec(&manifest).unwrap()),
        Err(ManifestError::IdentityMismatch {
            identity: "compiled envelope"
        })
    ));
}

#[test]
fn unsupported_schema_is_rejected() {
    let original = revision();
    let mut manifest: Value = serde_json::from_slice(original.canonical_bytes()).unwrap();
    manifest["schema_version"] = json!(2);

    assert!(matches!(
        ValidatedHarnessRevision::from_manifest_json(&serde_json::to_vec(&manifest).unwrap()),
        Err(ManifestError::UnsupportedSchema { actual: 2 })
    ));
}

#[test]
fn candidate_cannot_name_authority_or_implementation_fields() {
    let forbidden = [
        "model",
        "provider",
        "credentials",
        "tool_implementation",
        "permissions",
        "network",
        "evaluator",
        "registry",
        "store",
        "ipc",
        "source",
        "source_suggestion",
    ];

    for field in forbidden {
        let mut candidate = behavior();
        candidate[field] = json!("candidate controlled");
        assert!(
            matches!(
                ValidatedHarnessRevision::from_behavior_json(
                    parent(),
                    &serde_json::to_vec(&candidate).unwrap()
                ),
                Err(ManifestError::InvalidJson {
                    kind: "behavior",
                    ..
                })
            ),
            "field {field} was accepted"
        );

        let mut patch = serde_json::Map::new();
        patch.insert(field.to_owned(), json!("candidate controlled"));
        assert!(
            matches!(
                revision().apply_patch_json(&serde_json::to_vec(&Value::Object(patch)).unwrap()),
                Err(ManifestError::InvalidJson { kind: "patch", .. })
            ),
            "patch field {field} was accepted"
        );
    }
}

#[test]
fn unknown_manifest_fields_fail_closed() {
    let original = revision();
    let mut manifest: Value = serde_json::from_slice(original.canonical_bytes()).unwrap();
    manifest["activation"] = json!(true);

    assert!(matches!(
        ValidatedHarnessRevision::from_manifest_json(&serde_json::to_vec(&manifest).unwrap()),
        Err(ManifestError::InvalidJson {
            kind: "manifest",
            ..
        })
    ));
}

#[test]
fn tool_names_are_a_closed_enum() {
    let mut candidate = behavior();
    candidate["subagent_roles"]["reader"]["tools"] = json!(["read_file", "send_network"]);

    assert!(matches!(
        ValidatedHarnessRevision::from_behavior_json(
            parent(),
            &serde_json::to_vec(&candidate).unwrap()
        ),
        Err(ManifestError::InvalidJson {
            kind: "behavior",
            ..
        })
    ));
}

#[test]
fn identifiers_reject_paths_and_traversal() {
    for invalid in ["../escape", "nested/skill", ".hidden", "Uppercase"] {
        let mut candidate = behavior();
        candidate["skills"] = json!({invalid: "Text only."});
        assert!(matches!(
            ValidatedHarnessRevision::from_behavior_json(
                parent(),
                &serde_json::to_vec(&candidate).unwrap()
            ),
            Err(ManifestError::InvalidIdentifier { field: "skill" })
        ));
    }
}

#[test]
fn text_rejects_terminal_controls_and_script_documents() {
    for instructions in ["\u{1b}[31mcolored", "\u{0}hidden"] {
        let mut candidate = behavior();
        candidate["instructions"] = json!(instructions);
        assert!(matches!(
            ValidatedHarnessRevision::from_behavior_json(
                parent(),
                &serde_json::to_vec(&candidate).unwrap()
            ),
            Err(ManifestError::TerminalControl {
                field: "instructions"
            })
        ));
    }

    for body in ["#!/bin/sh\necho no", "Read this <script>alert(1)</script>"] {
        let mut candidate = behavior();
        candidate["skills"] = json!({"unsafe": body});
        assert!(matches!(
            ValidatedHarnessRevision::from_behavior_json(
                parent(),
                &serde_json::to_vec(&candidate).unwrap()
            ),
            Err(ManifestError::ExecutableText {
                field: "skill body"
            })
        ));
    }
}

#[test]
fn text_and_collection_limits_fail_at_first_value_over_the_ceiling() {
    let mut candidate = behavior();
    candidate["instructions"] = json!("x".repeat(32 * 1024 + 1));
    assert!(matches!(
        ValidatedHarnessRevision::from_behavior_json(
            parent(),
            &serde_json::to_vec(&candidate).unwrap()
        ),
        Err(ManifestError::TextTooLarge {
            field: "instructions",
            maximum: 32768
        })
    ));

    let skills: BTreeMap<_, _> = (0..17)
        .map(|index| (format!("skill-{index}"), "Text only."))
        .collect();
    candidate = behavior();
    candidate["skills"] = serde_json::to_value(skills).unwrap();
    assert!(matches!(
        ValidatedHarnessRevision::from_behavior_json(
            parent(),
            &serde_json::to_vec(&candidate).unwrap()
        ),
        Err(ManifestError::TooManyEntries {
            field: "skills",
            actual: 17,
            maximum: 16
        })
    ));
}

#[test]
fn every_resource_budget_is_bounded_by_the_compiled_envelope() {
    let above_ceiling = [
        ("tool_calls", 257_u64),
        ("subagents", 9),
        ("verifier_runs", 65),
        ("output_bytes", 8 * 1024 * 1024 + 1),
        ("tokens", 1_000_001),
        ("elapsed_ms", 2 * 60 * 60 * 1_000 + 1),
    ];

    for (field, value) in above_ceiling {
        let mut candidate = behavior();
        candidate["budgets"][field] = json!(value);
        assert!(
            matches!(
                ValidatedHarnessRevision::from_behavior_json(
                    parent(),
                    &serde_json::to_vec(&candidate).unwrap()
                ),
                Err(ManifestError::InvalidBudget { field: actual, .. }) if actual == field
            ),
            "budget {field} was accepted"
        );
    }
}

#[test]
fn zero_budgets_and_impossible_verifier_schedules_are_rejected() {
    let mut candidate = behavior();
    candidate["budgets"]["tool_calls"] = json!(0);
    assert!(matches!(
        ValidatedHarnessRevision::from_behavior_json(
            parent(),
            &serde_json::to_vec(&candidate).unwrap()
        ),
        Err(ManifestError::InvalidBudget {
            field: "tool_calls",
            ..
        })
    ));

    candidate = behavior();
    candidate["verifier"] = json!({"after_tool_calls": null, "before_completion": false});
    assert!(matches!(
        ValidatedHarnessRevision::from_behavior_json(
            parent(),
            &serde_json::to_vec(&candidate).unwrap()
        ),
        Err(ManifestError::EmptyVerifierSchedule)
    ));

    candidate = behavior();
    candidate["verifier"]["after_tool_calls"] = json!(65);
    assert!(matches!(
        ValidatedHarnessRevision::from_behavior_json(
            parent(),
            &serde_json::to_vec(&candidate).unwrap()
        ),
        Err(ManifestError::VerifierIntervalExceedsBudget)
    ));

    candidate = behavior();
    candidate["verifier"]["after_tool_calls"] = json!(1);
    candidate["budgets"]["verifier_runs"] = json!(16);
    assert!(matches!(
        ValidatedHarnessRevision::from_behavior_json(
            parent(),
            &serde_json::to_vec(&candidate).unwrap()
        ),
        Err(ManifestError::VerifierScheduleExceedsBudget)
    ));
}

#[test]
fn debug_output_exposes_identities_but_not_behavior_text() {
    let revision = revision();
    let output = format!("{revision:?}");

    assert!(output.contains("behavior_digest"));
    assert!(!output.contains("Work carefully"));
    assert!(!output.contains("Collect bounded evidence"));
}

#[test]
fn oversized_manifest_is_rejected_before_parsing() {
    let oversized = vec![b' '; 256 * 1024 + 1];
    assert!(matches!(
        ValidatedHarnessRevision::from_manifest_json(&oversized),
        Err(ManifestError::InputTooLarge {
            kind: "manifest",
            maximum: 262144
        })
    ));
}
