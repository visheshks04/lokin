use std::fs;
use std::process::{Command as ProcessCommand, Stdio};

use assert_cmd::Command;
use chrono::{Duration, Utc};
use httpmock::Method::POST;
use httpmock::MockServer;
use predicates::prelude::*;
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

fn lokin(data_dir: &TempDir) -> Command {
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("lokin"));
    command.env("LOKIN_DATA_DIR", data_dir.path());
    command
}

fn read_events(data_dir: &TempDir) -> Vec<Value> {
    let contents = fs::read_to_string(data_dir.path().join("events.jsonl")).unwrap();
    contents
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn lifecycle_plan_and_history_survive_process_restarts() {
    let data_dir = tempfile::tempdir().unwrap();

    lokin(&data_dir)
        .args(["start", "Build persistence", "--duration", "30"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Session started."));

    lokin(&data_dir)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("State: Running"))
        .stdout(predicate::str::contains("Goal: Build persistence"));

    lokin(&data_dir)
        .args(["pause", "--note", "Domain model complete"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Checkpoint recorded."));

    let paused_events = read_events(&data_dir);
    assert_eq!(paused_events.len(), 3);
    assert_eq!(paused_events[1]["type"], "session_paused");
    assert_eq!(paused_events[2]["type"], "checkpoint_added");
    assert_eq!(
        paused_events[1]["occurred_at"],
        paused_events[2]["occurred_at"]
    );

    lokin(&data_dir)
        .args(["checkpoint", "--note", "Storage tests remain"])
        .assert()
        .success();
    lokin(&data_dir).arg("resume").assert().success();
    lokin(&data_dir)
        .args(["plan", "add", "Finish tests", "--duration", "15"])
        .assert()
        .success()
        .stdout(predicate::str::contains("initial snapshot"));
    lokin(&data_dir)
        .args(["plan", "update", "1", "--status", "active"])
        .assert()
        .success()
        .stdout(predicate::str::contains("revised snapshot"));

    lokin(&data_dir)
        .args(["plan", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("[active] Finish tests (15m)"));

    let events = read_events(&data_dir);
    let session_id = events[0]["session_id"].as_str().unwrap();
    assert_eq!(events[5]["type"], "plan_created");
    assert_eq!(events[6]["type"], "plan_revised");
    assert_eq!(
        events[6]["data"]["plan"]["items"].as_array().unwrap().len(),
        1
    );

    lokin(&data_dir)
        .args(["history", "show", &session_id[..8]])
        .assert()
        .success()
        .stdout(predicate::str::contains("Domain model complete"))
        .stdout(predicate::str::contains("plan_revised"));

    lokin(&data_dir).arg("stop").assert().success();
    lokin(&data_dir)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("No active Lokin session"));
    lokin(&data_dir)
        .args(["history", "--search", "persistence"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Build persistence"))
        .stdout(predicate::str::contains("Stopped"));
}

#[test]
fn invalid_transition_does_not_append_an_event() {
    let data_dir = tempfile::tempdir().unwrap();
    lokin(&data_dir)
        .args(["start", "Test transitions", "--duration", "25"])
        .assert()
        .success();

    let before = read_events(&data_dir);
    lokin(&data_dir)
        .arg("resume")
        .assert()
        .failure()
        .stderr(predicate::str::contains("already running"));
    assert_eq!(read_events(&data_dir), before);
}

#[test]
fn naturally_completed_session_is_displayed_and_does_not_block_start() {
    let data_dir = tempfile::tempdir().unwrap();
    let session_id = Uuid::new_v4();
    let start_time = Utc::now() - Duration::minutes(2);
    let event = json!({
        "schema_version": 1,
        "session_id": session_id,
        "occurred_at": start_time,
        "type": "session_started",
        "data": {
            "goal": "Expired session",
            "duration_minutes": 1
        }
    });
    fs::write(
        data_dir.path().join("events.jsonl"),
        format!("{}\n", serde_json::to_string(&event).unwrap()),
    )
    .unwrap();

    lokin(&data_dir)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("State: Completed"))
        .stdout(predicate::str::contains("Remaining wall-clock: 00:00:00"));

    lokin(&data_dir)
        .args(["start", "Next session", "--duration", "25"])
        .assert()
        .success();
    assert_eq!(read_events(&data_dir).len(), 2);
}

#[test]
fn simultaneous_starts_create_only_one_active_session() {
    let data_dir = tempfile::tempdir().unwrap();
    let binary = assert_cmd::cargo::cargo_bin!("lokin");

    let first = ProcessCommand::new(binary)
        .env("LOKIN_DATA_DIR", data_dir.path())
        .args(["start", "Concurrent one", "--duration", "25"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let second = ProcessCommand::new(binary)
        .env("LOKIN_DATA_DIR", data_dir.path())
        .args(["start", "Concurrent two", "--duration", "25"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let first_status = first.wait_with_output().unwrap().status;
    let second_status = second.wait_with_output().unwrap().status;
    assert_ne!(first_status.success(), second_status.success());
    assert_eq!(read_events(&data_dir).len(), 1);
}

#[test]
fn input_validation_uses_runtime_and_clap_exit_codes() {
    let data_dir = tempfile::tempdir().unwrap();

    lokin(&data_dir)
        .args(["start", "   "])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("Goal cannot be empty"));

    lokin(&data_dir)
        .args(["start", "Goal", "--duration", "not-a-number"])
        .assert()
        .code(2);
}

#[test]
fn planned_start_confirms_before_persisting_an_llm_sourced_snapshot() {
    let data_dir = tempfile::tempdir().unwrap();
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(POST)
            .path("/openai/v1/chat/completions")
            .header("authorization", "Bearer test-key")
            .body_contains("\"model\":\"strict-model\"");
        then.status(200).json_body(json!({
            "choices": [{
                "message": {
                    "content": r#"{"items":[{"description":"Write tests","duration_minutes":10,"status":"pending"}]}"#,
                    "refusal": null
                }
            }]
        }));
    });

    lokin(&data_dir)
        .args([
            "start",
            "Build LLM planning",
            "--duration",
            "20",
            "--plan",
            "--context",
            "Use a small plan",
        ])
        .env("GROQ_API_KEY", "test-key")
        .env("LOKIN_LLM_MODEL", "strict-model")
        .env(
            "LOKIN_LLM_BASE_URL",
            format!("{}/openai/v1", server.base_url()),
        )
        .write_stdin("a\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("Proposed plan:"))
        .stdout(predicate::str::contains("LLM plan saved from Groq"));

    mock.assert();
    let events = read_events(&data_dir);
    assert_eq!(events.len(), 2);
    assert_eq!(events[1]["type"], "plan_created");
    assert_eq!(events[1]["data"]["source"]["kind"], "llm");
    assert_eq!(events[1]["data"]["source"]["provider"], "groq");
    assert_eq!(events[1]["data"]["source"]["model"], "strict-model");
}

#[test]
fn cancelling_planned_start_leaves_history_untouched() {
    let data_dir = tempfile::tempdir().unwrap();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST).path("/openai/v1/chat/completions");
        then.status(200).json_body(json!({
            "choices": [{
                "message": {
                    "content": r#"{"items":[{"description":"Draft","duration_minutes":5,"status":"pending"}]}"#,
                    "refusal": null
                }
            }]
        }));
    });

    lokin(&data_dir)
        .args(["start", "Do not save", "--duration", "10", "--plan"])
        .env("GROQ_API_KEY", "test-key")
        .env("LOKIN_LLM_MODEL", "strict-model")
        .env(
            "LOKIN_LLM_BASE_URL",
            format!("{}/openai/v1", server.base_url()),
        )
        .write_stdin("c\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("No session was started"));

    assert!(!data_dir.path().join("events.jsonl").exists());
}

#[test]
fn planned_start_requires_llm_configuration_without_writing_events() {
    let data_dir = tempfile::tempdir().unwrap();

    lokin(&data_dir)
        .args(["start", "Needs configuration", "--plan"])
        .env_remove("GROQ_API_KEY")
        .env_remove("LOKIN_LLM_MODEL")
        .assert()
        .code(1)
        .stderr(predicate::str::contains("Set GROQ_API_KEY"));

    assert!(!data_dir.path().join("events.jsonl").exists());
}

#[test]
fn llm_revision_replaces_an_active_plan_with_a_provenance_snapshot() {
    let data_dir = tempfile::tempdir().unwrap();
    lokin(&data_dir)
        .args(["start", "Revise me", "--duration", "30"])
        .assert()
        .success();
    lokin(&data_dir)
        .args(["plan", "add", "Initial task", "--duration", "10"])
        .assert()
        .success();

    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST)
            .path("/openai/v1/chat/completions")
            .body_contains("Current plan JSON");
        then.status(200).json_body(json!({
            "choices": [{
                "message": {
                    "content": r#"{"items":[{"description":"Revised task","duration_minutes":12,"status":"active"}]}"#,
                    "refusal": null
                }
            }]
        }));
    });

    lokin(&data_dir)
        .args(["plan", "revise", "--context", "The first approach changed"])
        .env("GROQ_API_KEY", "test-key")
        .env("LOKIN_LLM_MODEL", "strict-model")
        .env(
            "LOKIN_LLM_BASE_URL",
            format!("{}/openai/v1", server.base_url()),
        )
        .write_stdin("accept\n")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "LLM plan revision saved from Groq",
        ));

    let events = read_events(&data_dir);
    assert_eq!(events.len(), 3);
    assert_eq!(events[2]["type"], "plan_revised");
    assert_eq!(
        events[2]["data"]["plan"]["items"][0]["description"],
        "Revised task"
    );
    assert_eq!(events[2]["data"]["source"]["kind"], "llm");
}
