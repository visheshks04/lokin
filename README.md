# Lokin

Lokin is a task-agnostic command-line focus timer. It gives a goal a fixed wall-clock budget, records meaningful session transitions, and reconstructs timing and state from an append-only event history.

The core lifecycle is local and deterministic. Optional LLM planning uses a user-supplied Groq API key, but no provider credentials, prompts, or raw model responses are written to Lokin history.

## Features

- One running or paused session at a time, enforced across concurrent processes
- Hard wall-clock deadline that pauses never extend
- Restart-safe start, status, pause, checkpoint, resume, and stop lifecycle
- Standalone checkpoints and optional checkpoint notes when pausing
- Manual plans with complete historical snapshots
- Optional, user-confirmed Groq plan generation and plan revision
- Searchable, append-only session history
- Active, paused, elapsed, and remaining time reconstructed from events
- Natural completion derived from the deadline without a completion event
- JSONL persistence in the operating system's local application-data directory

## Install and run

Lokin requires a current Rust toolchain. Build it from the repository root:

```bash
cargo build
```

Run it through Cargo during development:

```bash
cargo run -- --help
```

After installing it with `cargo install --path .`, use `lokin` directly:

```bash
cargo install --path .
lokin --help
```

## Session lifecycle

Start a session with a goal and optional duration. The default duration is 25 minutes.

```bash
lokin start "Write the persistence layer" --duration 45
lokin status
```

Pause without changing the deadline. An optional note creates a separate checkpoint event at the same timestamp.

```bash
lokin pause --note "Domain model is complete"
lokin status
```

Add checkpoints while running or paused:

```bash
lokin checkpoint --note "Storage tests remain"
```

Resume or stop the session:

```bash
lokin resume
lokin stop
```

The lifecycle is:

```text
no active session → start → running ⇄ paused → stop
                                  ↓
                         natural completion
```

Natural completion occurs when the current time reaches:

```text
start_time + duration_minutes
```

No `session_completed` event is written. Completed sessions no longer block a new session.

## Manual plans

Plans are intentionally manual in this MVP. Each item has a description, duration, and one of four statuses: `pending`, `active`, `completed`, or `skipped`.

```bash
lokin plan add "Implement event reducer" --duration 20
lokin plan add "Test persistence" --duration 15
lokin plan show

lokin plan update 1 --status active
lokin plan update 1 --description "Finish event reducer" --duration 10
lokin plan remove 2
```

The first addition writes `plan_created`. Every later add, update, or removal writes `plan_revised` containing the complete plan snapshot. Old snapshots remain in history.

Plan durations must be positive. A plan may exceed the original budget or current remaining time; Lokin saves it and prints a warning rather than rejecting it.

## LLM planning with Groq

LLM planning is opt-in. It uses Groq's OpenAI-compatible Chat Completions API and requires a configured model that supports strict JSON-schema output.

Set these environment variables before invoking an LLM command:

```bash
export GROQ_API_KEY="your-groq-api-key"
export LOKIN_LLM_MODEL="your-strict-structured-output-model"
```

`LOKIN_LLM_BASE_URL` normally should not be set. It overrides Groq's standard `https://api.groq.com/openai/v1` base URL and exists for local testing or compatible endpoints.

Generate a plan before the session begins:

```bash
lokin start "Write persistence" --duration 30 --plan
lokin start "Write persistence" --duration 30 --plan --context "The event reducer already exists"
```

Lokin displays the proposal and asks whether to accept, refine, or cancel. Refinement feedback produces another proposal. A session is not started until you accept, so provider latency and planning discussion never consume its hard wall-clock budget. Cancelling writes no events.

Revise an existing running or paused plan explicitly:

```bash
lokin plan revise --context "The storage tests are now complete"
```

Revision receives the current plan, timing state, and checkpoints. It presents a complete replacement snapshot for confirmation. If the session changes while the model is working, Lokin rejects the stale proposal rather than overwriting newer history.

Generated plans must fit the available wall-clock time. Lokin asks the model once to correct an invalid or over-budget proposal, then fails without saving anything if the correction is still invalid.

## History

List every preserved session:

```bash
lokin history
```

Filter by case-insensitive goal text or session-ID prefix:

```bash
lokin history --search persistence
lokin history --search f64f7828
```

Inspect one session using its full ID or an unambiguous prefix:

```bash
lokin history show f64f7828
```

Detailed history includes metadata, derived timing, checkpoints, the latest plan, plan-snapshot timestamps, and the ordered event timeline.

## Persistence model

Lokin stores one event per line in `events.jsonl`. The file lives under the operating system's local application-data directory in a `lokin` folder. Set `LOKIN_DATA_DIR` to use a specific directory:

```bash
LOKIN_DATA_DIR=/tmp/lokin-demo lokin start "Try isolated storage"
```

This override is useful for development, automated tests, and inspecting a clean event history.

Each event has this envelope:

```json
{
  "schema_version": 1,
  "session_id": "f64f7828-1656-4c37-bfe2-3ca0630fb450",
  "occurred_at": "2026-08-28T10:06:53.706882Z",
  "type": "session_started",
  "data": {
    "goal": "Build persistence",
    "duration_minutes": 30
  }
}
```

Possible event types are:

- `session_started`
- `session_paused`
- `session_resumed`
- `checkpoint_added`
- `plan_created`
- `plan_revised`
- `session_stopped`

Plan events include a `source` object. Existing and manually-created plans use `{"kind":"manual"}`; Groq-created plans record only `{"kind":"llm","provider":"groq","model":"..."}`. API keys, prompts, refinement text, raw responses, and usage data are never persisted.

The log does not contain mutable current state, deadlines, timer ticks, elapsed counters, pause counters, or cached plans. Those values are projections reconstructed from the event stream whenever a command runs.

An adjacent `events.lock` file protects each load–validate–append transaction so simultaneous commands cannot violate the one-active-session invariant or interleave event batches.

Lokin fails closed if the log contains malformed JSON, an unsupported schema version, inconsistent timestamps, or invalid domain transitions. It does not silently discard or repair history.

## Architecture

The project stays deliberately small:

1. `src/main.rs` parses arguments, dispatches commands, and owns process-level error handling.
2. `src/cli/` defines the builder-style Clap interface and command orchestration.
3. `src/session.rs` defines domain events, plans, projections, validation, and pure timing reconstruction.
4. `src/storage.rs` resolves the data path and provides locked JSONL reading and append batching.
5. `src/llm.rs` owns the only async boundary: Groq HTTP inference and structured-plan validation.
6. `src/error.rs` defines the application's error contract.

There is no repository trait or service hierarchy. Command handlers coordinate one concrete event store and the pure session reducer. The CLI, storage, and reducer are synchronous; only LLM HTTP work runs in a small current-thread Tokio runtime.

## Timing rules

- The deadline is always derived from immutable start metadata.
- Pausing changes how elapsed wall time is classified, not how much time remains.
- Active time plus paused time equals wall-clock elapsed.
- Timing is capped at an explicit stop or natural deadline.
- A session stopped early reports the wall-clock budget that was unused at stop time.
- A session completed while paused has no current pause, but its final paused interval counts through the deadline.

## Development and tests

Run the complete quality suite:

```bash
cargo check
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

Unit tests use fixed timestamps to verify event reconstruction without a clock abstraction. Integration tests run the compiled binary in separate processes with temporary data directories, including a concurrent-start scenario.

## Out of scope

This MVP deliberately excludes:

- automatic clarification interviews, pause-triggered replanning, or adaptive break suggestions
- RAG, VLMs, local models, or other providers beyond Groq
- adaptive break suggestions
- background timers or notifications
- databases, event compaction, or history deletion
- GUI, cloud sync, authentication, integrations, or personalization

## License

No license has been specified yet.
