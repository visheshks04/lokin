# Lokin

Lokin is a command-line focus timer for committing to one finite block of work. The long-term goal is a task-agnostic lock-in loop: choose a goal and a time budget, work, report what happened, and let Lokin help adjust the remaining plan.

> **Project status:** early prototype. The command-line interface exists, but session state is not yet persisted and the current handlers are demonstrations only.

## Current scope

The project is being developed in two layers.

### Core session manager

The core product is intended to provide:

- `start <goal> --duration <minutes>`
- one active lock-in at a time
- `status`, including elapsed and remaining time
- `pause --note "..."`
- `resume`
- `stop`
- persistent active-session state
- completed-session history
- a hard wall-clock deadline that includes pauses and breaks
- persisted plan and checkpoint state

### Minimal adaptive layer

After the deterministic session loop is reliable, Lokin will add three narrowly scoped AI capabilities:

1. Context-aware initial planning: use the goal, duration, and optional context to produce a structured schedule. If important context is missing, ask one to three focused questions first.
2. Progress-aware replanning: treat a pause note as a checkpoint, interpret the reported progress, and reallocate only the remaining time.
3. Adaptive break allocation: propose breaks using elapsed work, progress, remaining time, and upcoming workload. Rust rules will constrain the proposal.

The first release deliberately does not include RAG, repository awareness, screen monitoring, distraction detection, calendar or Slack integrations, habit tracking, gamification, a GUI, cloud sync, login, or historical personalization.

## What works today

The current binary is a small Rust program using [`clap`](https://docs.rs/clap) for command parsing. It exposes these commands:

| Command | Current behavior |
| --- | --- |
| `start` | Parses a required goal and an optional duration, then prints them. Duration defaults to 25 minutes. |
| `status` | Prints a placeholder remaining-time message. |
| `pause` | Prints a placeholder message. Its current option is an unused duration argument. |
| `resume` | Prints a placeholder message. |
| `stop` | Prints a placeholder message. |

Every invocation is currently independent. Starting a session does not create a record that a later `status`, `pause`, `resume`, or `stop` command can read.

## Installation and development

Lokin requires a current Rust toolchain and Cargo. From the repository root:

```bash
cargo build
```

Run the command through Cargo while developing:

```bash
cargo run -- --help
```

The project currently targets Rust edition 2024. Dependencies are declared in [`Cargo.toml`](Cargo.toml), and build output is excluded through [`.gitignore`](.gitignore).

## Usage examples

These examples demonstrate the current command interface. They do not yet create a durable session.

```bash
# Start a 25-minute lock-in
cargo run -- start "Write the authentication API"

# Start a custom-duration lock-in
cargo run -- start "Read chapter three" --duration 45

# Inspect the current status
cargo run -- status

# Pause, resume, or stop the current session
cargo run -- pause
cargo run -- resume
cargo run -- stop
```

The intended future pause interface is checkpoint-oriented:

```bash
cargo run -- pause --note "Finished the database schema; API validation remains"
```

That option is not implemented yet.

## Architecture

The current execution path is intentionally small:

1. [`src/main.rs`](src/main.rs) builds the CLI and dispatches the selected subcommand.
2. [`src/cli/mod.rs`](src/cli/mod.rs) defines the command names, arguments, defaults, and help text.
3. [`src/cli/handlers.rs`](src/cli/handlers.rs) contains the current command handlers, which only print output.
4. [`src/session.rs`](src/session.rs) is an unfinished draft of a session model and is not imported into the application.

The next intended architecture is:

```text
CLI command
    ↓
application/lifecycle service
    ↓
session state machine
    ↓
durable storage (active session + history)
    ↓
clock and plan/checkpoint calculations
```

The AI layer should sit above this deterministic core. It should receive structured session state and return validated suggestions; it should not own the deadline or be able to extend the user's time budget.

## Persistence design goals

The first persistence implementation should be deliberately simple and local:

- Store one active session separately from completed history.
- Give each session a stable ID.
- Store timestamps in an unambiguous format such as UTC or Unix time.
- Calculate the deadline once at `start`.
- Keep the deadline unchanged during pause and resume.
- Record checkpoint notes and lifecycle transitions rather than overwriting the story of the session.
- Write state safely so an interrupted process does not leave a half-written record.
- Make the storage directory configurable for tests.

A useful lifecycle invariant is:

```text
no active session → start → active ⇄ paused → stop → history
```

Invalid transitions should produce user-facing errors: a second `start`, `pause` without an active session, `resume` while already active, or `stop` when nothing is running.

## Testing roadmap

There are currently no automated tests. The first tests should cover:

- creating and loading an active session
- rejecting a second active session
- calculating elapsed and remaining wall-clock time
- preserving the deadline across pause/resume
- recording checkpoint notes
- moving a stopped session into history
- recovering state in a fresh process
- handling a missing or corrupted state file

Use an injected clock and a temporary storage directory so timing tests are deterministic and do not touch a developer's real home directory.

Useful checks once implementation work begins:

```bash
cargo check
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## Roadmap

1. Replace the unfinished session draft with a valid domain model.
2. Wire the model into the CLI and add local active-session persistence.
3. Implement lifecycle validation, wall-clock deadlines, checkpoints, and history.
4. Add deterministic tests and improve command error messages and formatting.
5. Persist a structured initial plan and expose it through status/checkpoints.
6. Add a planner interface for context questions and initial schedules.
7. Add progress-aware replanning and constrained adaptive break suggestions.

## Product boundary

Lokin is task-agnostic. It manages a finite block of time and adapts based on what the user reports. The goal might be writing Rust, studying, exercising, practicing music, or doing household work; Lokin does not need to understand the domain to enforce the time budget and help maintain momentum.

## License

No license has been specified yet.
