use std::io::{self, Write};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use clap::ArgMatches;
use uuid::Uuid;

use crate::error::{LokinError, Result};
use crate::llm::{
    GroqInference, InferenceRequest, LlmConfig, infer_plan, initial_plan_request,
    revision_plan_request, validate_plan_fits_budget,
};
use crate::session::{
    EffectiveState, Plan, PlanItem, PlanItemStatus, PlanSource, SessionEvent, SessionProjection,
    StoredEvent, reconstruct_all, validate_plan,
};
use crate::storage::EventStore;

pub fn dispatch(matches: &ArgMatches) -> Result<()> {
    match matches.subcommand() {
        Some(("start", args)) => start(args),
        Some(("status", _)) => status(),
        Some(("pause", args)) => pause(args),
        Some(("checkpoint", args)) => checkpoint(args),
        Some(("resume", _)) => resume(),
        Some(("stop", _)) => stop(),
        Some(("plan", args)) => plan(args),
        Some(("history", args)) => history(args),
        _ => Ok(()),
    }
}

fn start(args: &ArgMatches) -> Result<()> {
    let goal = required_string(args, "goal").trim().to_string();
    let duration_minutes = *args
        .get_one::<u16>("duration")
        .expect("clap always supplies duration");
    validate_nonempty(&goal, "Goal")?;
    validate_duration(duration_minutes, "Session duration")?;

    let context = args.get_one::<String>("context").map(String::as_str);
    if args.get_flag("plan") {
        return start_with_llm_plan(goal, duration_minutes, context);
    }

    start_without_plan(goal, duration_minutes)
}

fn start_without_plan(goal: String, duration_minutes: u16) -> Result<()> {
    let store = EventStore::from_environment()?;
    let (session_id, started_at) = store.mutate(|events| {
        let now = Utc::now();
        let projections = reconstruct_all(events, now)?;
        if active_projection(&projections)?.is_some() {
            return Err(LokinError::InvalidTransition(
                "A Lokin session is already active. Stop it or let its deadline pass first."
                    .to_string(),
            ));
        }

        let session_id = Uuid::new_v4();
        let event = StoredEvent::new(
            session_id,
            now,
            SessionEvent::SessionStarted {
                goal: goal.clone(),
                duration_minutes,
            },
        );
        Ok(((session_id, now), vec![event]))
    })?;

    render_started_session(session_id, &goal, duration_minutes, started_at);
    Ok(())
}

fn start_with_llm_plan(goal: String, duration_minutes: u16, context: Option<&str>) -> Result<()> {
    let config = LlmConfig::from_environment()?;
    let inference = GroqInference::from_config(&config)?;
    let plan = interactively_plan(
        &config,
        &inference,
        Duration::minutes(i64::from(duration_minutes)),
        |feedback| initial_plan_request(&goal, duration_minutes, context, feedback),
    )?;
    let Some(plan) = plan else {
        println!("Session planning cancelled. No session was started.");
        return Ok(());
    };
    let source = PlanSource::Llm {
        provider: "groq".to_string(),
        model: config.model().to_string(),
    };

    let store = EventStore::from_environment()?;
    let (session_id, started_at) = store.mutate(|events| {
        let now = Utc::now();
        let projections = reconstruct_all(events, now)?;
        if active_projection(&projections)?.is_some() {
            return Err(LokinError::InvalidTransition(
                "A Lokin session is already active. Stop it or let its deadline pass first."
                    .to_string(),
            ));
        }
        let session_id = Uuid::new_v4();
        Ok((
            (session_id, now),
            vec![
                StoredEvent::new(
                    session_id,
                    now,
                    SessionEvent::SessionStarted {
                        goal: goal.clone(),
                        duration_minutes,
                    },
                ),
                StoredEvent::new(
                    session_id,
                    now,
                    SessionEvent::PlanCreated {
                        plan: plan.clone(),
                        source: source.clone(),
                    },
                ),
            ],
        ))
    })?;

    render_started_session(session_id, &goal, duration_minutes, started_at);
    println!("LLM plan saved from Groq ({}).", config.model());
    Ok(())
}

fn render_started_session(
    session_id: Uuid,
    goal: &str,
    duration_minutes: u16,
    started_at: DateTime<Utc>,
) {
    println!("Session started.");
    println!("ID: {session_id}");
    println!("Goal: {goal}");
    println!("Started: {}", format_timestamp(started_at));
    println!("Budget: {duration_minutes} minutes (wall clock)");
    println!(
        "Deadline: {}",
        format_timestamp(started_at + Duration::minutes(i64::from(duration_minutes)))
    );
}

fn status() -> Result<()> {
    let store = EventStore::from_environment()?;
    let events = store.read_all()?;
    let projections = reconstruct_all(&events, Utc::now())?;

    match projections.last() {
        None => println!("No Lokin sessions yet. Start one with `lokin start <goal>`."),
        Some(projection) if projection.effective_state == EffectiveState::Stopped => {
            println!("No active Lokin session. Use `lokin history` to inspect past sessions.");
        }
        Some(projection) => render_projection(projection),
    }
    Ok(())
}

fn pause(args: &ArgMatches) -> Result<()> {
    let note = args
        .get_one::<String>("note")
        .map(|value| value.trim().to_string());
    if let Some(note) = &note {
        validate_nonempty(note, "Checkpoint note")?;
    }

    let store = EventStore::from_environment()?;
    let (session_id, paused_at) = store.mutate(|events| {
        let now = Utc::now();
        let projections = reconstruct_all(events, now)?;
        let current = require_active_projection(&projections)?;
        if current.effective_state != EffectiveState::Running {
            return Err(LokinError::InvalidTransition(
                "The current session is already paused.".to_string(),
            ));
        }

        let mut appended = vec![StoredEvent::new(
            current.metadata.session_id,
            now,
            SessionEvent::SessionPaused,
        )];
        if let Some(note) = &note {
            appended.push(StoredEvent::new(
                current.metadata.session_id,
                now,
                SessionEvent::CheckpointAdded { note: note.clone() },
            ));
        }
        Ok(((current.metadata.session_id, now), appended))
    })?;

    println!(
        "Session {session_id} paused at {}.",
        format_timestamp(paused_at)
    );
    if note.is_some() {
        println!("Checkpoint recorded.");
    }
    println!("The wall-clock deadline has not changed.");
    Ok(())
}

fn checkpoint(args: &ArgMatches) -> Result<()> {
    let note = required_string(args, "note").trim().to_string();
    validate_nonempty(&note, "Checkpoint note")?;

    let store = EventStore::from_environment()?;
    let (session_id, occurred_at) = store.mutate(|events| {
        let now = Utc::now();
        let projections = reconstruct_all(events, now)?;
        let current = require_active_projection(&projections)?;
        Ok((
            (current.metadata.session_id, now),
            vec![StoredEvent::new(
                current.metadata.session_id,
                now,
                SessionEvent::CheckpointAdded { note: note.clone() },
            )],
        ))
    })?;

    println!(
        "Checkpoint added to session {session_id} at {}.",
        format_timestamp(occurred_at)
    );
    Ok(())
}

fn resume() -> Result<()> {
    let store = EventStore::from_environment()?;
    let (session_id, resumed_at) = store.mutate(|events| {
        let now = Utc::now();
        let projections = reconstruct_all(events, now)?;
        let current = require_active_projection(&projections)?;
        if current.effective_state != EffectiveState::Paused {
            return Err(LokinError::InvalidTransition(
                "The current session is already running.".to_string(),
            ));
        }
        Ok((
            (current.metadata.session_id, now),
            vec![StoredEvent::new(
                current.metadata.session_id,
                now,
                SessionEvent::SessionResumed,
            )],
        ))
    })?;

    println!(
        "Session {session_id} resumed at {}.",
        format_timestamp(resumed_at)
    );
    Ok(())
}

fn stop() -> Result<()> {
    let store = EventStore::from_environment()?;
    let (session_id, stopped_at) = store.mutate(|events| {
        let now = Utc::now();
        let projections = reconstruct_all(events, now)?;
        let current = require_active_projection(&projections)?;
        Ok((
            (current.metadata.session_id, now),
            vec![StoredEvent::new(
                current.metadata.session_id,
                now,
                SessionEvent::SessionStopped,
            )],
        ))
    })?;

    println!(
        "Session {session_id} stopped at {}.",
        format_timestamp(stopped_at)
    );
    println!("Its complete event history has been preserved.");
    Ok(())
}

fn plan(args: &ArgMatches) -> Result<()> {
    match args.subcommand() {
        Some(("show", _)) => plan_show(),
        Some(("revise", args)) => plan_revise(args),
        Some(("add", args)) => plan_add(args),
        Some(("update", args)) => plan_update(args),
        Some(("remove", args)) => plan_remove(args),
        _ => Ok(()),
    }
}

fn plan_show() -> Result<()> {
    let store = EventStore::from_environment()?;
    let events = store.read_all()?;
    let projections = reconstruct_all(&events, Utc::now())?;
    let current = require_active_projection(&projections)?;
    render_plan(current);
    Ok(())
}

fn plan_revise(args: &ArgMatches) -> Result<()> {
    let context = args.get_one::<String>("context").map(String::as_str);
    let config = LlmConfig::from_environment()?;
    let inference = GroqInference::from_config(&config)?;
    let store = EventStore::from_environment()?;
    let events = store.read_all()?;
    let projections = reconstruct_all(&events, Utc::now())?;
    let current = require_active_projection(&projections)?;
    if current.current_plan.is_none() {
        return Err(LokinError::InvalidTransition(
            "No plan exists yet. Start with `lokin plan add` or begin a new session with `--plan`."
                .to_string(),
        ));
    }

    let session_id = current.metadata.session_id;
    let remaining_wall_clock = current.remaining_wall_clock;
    let base_session_events = events_for_session(&events, session_id);
    let plan = interactively_plan(&config, &inference, remaining_wall_clock, |feedback| {
        revision_plan_request(current, context, feedback)
    })?;
    let Some(plan) = plan else {
        println!("Plan revision cancelled. No changes were saved.");
        return Ok(());
    };
    let source = PlanSource::Llm {
        provider: "groq".to_string(),
        model: config.model().to_string(),
    };

    store.mutate(|events| {
        if events_for_session(events, session_id) != base_session_events {
            return Err(LokinError::InvalidTransition(
                "The session changed while the plan was being generated. Rerun `lokin plan revise`."
                    .to_string(),
            ));
        }
        let now = Utc::now();
        let projections = reconstruct_all(events, now)?;
        let current = require_active_projection(&projections)?;
        if current.metadata.session_id != session_id || current.current_plan.is_none() {
            return Err(LokinError::InvalidTransition(
                "The active session changed while the plan was being generated. Rerun `lokin plan revise`."
                    .to_string(),
            ));
        }
        validate_plan_fits_budget(&plan, current.remaining_wall_clock)?;
        Ok((
            (),
            vec![StoredEvent::new(
                session_id,
                now,
                SessionEvent::PlanRevised {
                    plan: plan.clone(),
                    source: source.clone(),
                },
            )],
        ))
    })?;

    println!("LLM plan revision saved from Groq ({}).", config.model());
    Ok(())
}

fn interactively_plan<F>(
    config: &LlmConfig,
    inference: &GroqInference,
    remaining_wall_clock: Duration,
    mut request_for_feedback: F,
) -> Result<Option<Plan>>
where
    F: FnMut(&[String]) -> InferenceRequest,
{
    let mut feedback = Vec::new();
    loop {
        println!("Generating a plan with Groq ({}).", config.model());
        let request = request_for_feedback(&feedback);
        let plan = infer_plan(inference, config.model(), &request, remaining_wall_clock)?;
        render_proposed_plan(&plan, remaining_wall_clock);

        match prompt_line("Accept this plan? [a]ccept / [r]efine / [c]ancel: ")? {
            None => return Ok(None),
            Some(choice) if matches!(choice.trim().to_lowercase().as_str(), "a" | "accept") => {
                return Ok(Some(plan));
            }
            Some(choice) if matches!(choice.trim().to_lowercase().as_str(), "c" | "cancel") => {
                return Ok(None);
            }
            Some(choice) if matches!(choice.trim().to_lowercase().as_str(), "r" | "refine") => {
                let Some(next_feedback) = prompt_line("What should change? ")? else {
                    return Ok(None);
                };
                let next_feedback = next_feedback.trim().to_string();
                if next_feedback.is_empty() {
                    println!("Refinement feedback cannot be empty.");
                } else {
                    feedback.push(next_feedback);
                }
            }
            Some(_) => println!("Enter a, r, or c."),
        }
    }
}

fn prompt_line(prompt: &str) -> Result<Option<String>> {
    print!("{prompt}");
    io::stdout()
        .flush()
        .map_err(|error| LokinError::InteractiveInput(error.to_string()))?;
    let mut line = String::new();
    let bytes = io::stdin()
        .read_line(&mut line)
        .map_err(|error| LokinError::InteractiveInput(error.to_string()))?;
    if bytes == 0 { Ok(None) } else { Ok(Some(line)) }
}

fn render_proposed_plan(plan: &Plan, remaining_wall_clock: Duration) {
    println!("\nProposed plan:");
    render_plan_items(Some(plan));
    let total_minutes: u32 = plan
        .items
        .iter()
        .map(|item| u32::from(item.duration_minutes))
        .sum();
    println!("Total planned: {total_minutes} minutes");
    println!(
        "Available wall-clock: {}\n",
        format_duration(remaining_wall_clock)
    );
}

fn plan_add(args: &ArgMatches) -> Result<()> {
    let description = required_string(args, "description").trim().to_string();
    let duration_minutes = *args
        .get_one::<u16>("duration")
        .expect("clap validates plan duration");
    validate_nonempty(&description, "Plan item description")?;
    validate_duration(duration_minutes, "Plan item duration")?;

    let store = EventStore::from_environment()?;
    let result = store.mutate(|events| {
        let now = Utc::now();
        let projections = reconstruct_all(events, now)?;
        let current = require_active_projection(&projections)?;
        let created = current.current_plan.is_none();
        let mut plan = current
            .current_plan
            .clone()
            .unwrap_or(Plan { items: Vec::new() });
        plan.items.push(PlanItem {
            description: description.clone(),
            duration_minutes,
            status: PlanItemStatus::Pending,
        });
        validate_plan(&plan)?;
        let event = if created {
            SessionEvent::PlanCreated {
                plan: plan.clone(),
                source: PlanSource::Manual,
            }
        } else {
            SessionEvent::PlanRevised {
                plan: plan.clone(),
                source: PlanSource::Manual,
            }
        };
        Ok((
            PlanMutationResult::new(current, plan, created),
            vec![StoredEvent::new(current.metadata.session_id, now, event)],
        ))
    })?;

    println!(
        "Plan item added; {} snapshot saved.",
        if result.created { "initial" } else { "revised" }
    );
    render_plan_warnings(&result);
    Ok(())
}

fn plan_update(args: &ArgMatches) -> Result<()> {
    let item_number = *args
        .get_one::<usize>("item_number")
        .expect("clap validates item number");
    let description = args
        .get_one::<String>("description")
        .map(|value| value.trim().to_string());
    let duration = args.get_one::<u16>("duration").copied();
    let status = args
        .get_one::<String>("status")
        .map(|value| PlanItemStatus::parse(value))
        .transpose()?;

    if let Some(description) = &description {
        validate_nonempty(description, "Plan item description")?;
    }
    if let Some(duration) = duration {
        validate_duration(duration, "Plan item duration")?;
    }

    let store = EventStore::from_environment()?;
    let result = store.mutate(|events| {
        let now = Utc::now();
        let projections = reconstruct_all(events, now)?;
        let current = require_active_projection(&projections)?;
        let mut plan = current.current_plan.clone().ok_or_else(|| {
            LokinError::InvalidTransition(
                "No plan exists yet. Add the first item with `lokin plan add`.".to_string(),
            )
        })?;
        let index = plan_index(item_number, plan.items.len())?;
        let item = &mut plan.items[index];
        if let Some(description) = &description {
            item.description.clone_from(description);
        }
        if let Some(duration) = duration {
            item.duration_minutes = duration;
        }
        if let Some(status) = status {
            item.status = status;
        }
        validate_plan(&plan)?;
        Ok((
            PlanMutationResult::new(current, plan.clone(), false),
            vec![StoredEvent::new(
                current.metadata.session_id,
                now,
                SessionEvent::PlanRevised {
                    plan,
                    source: PlanSource::Manual,
                },
            )],
        ))
    })?;

    println!("Plan item {item_number} updated; revised snapshot saved.");
    render_plan_warnings(&result);
    Ok(())
}

fn plan_remove(args: &ArgMatches) -> Result<()> {
    let item_number = *args
        .get_one::<usize>("item_number")
        .expect("clap validates item number");

    let store = EventStore::from_environment()?;
    let result = store.mutate(|events| {
        let now = Utc::now();
        let projections = reconstruct_all(events, now)?;
        let current = require_active_projection(&projections)?;
        let mut plan = current.current_plan.clone().ok_or_else(|| {
            LokinError::InvalidTransition("No plan exists for this session.".to_string())
        })?;
        let index = plan_index(item_number, plan.items.len())?;
        plan.items.remove(index);
        Ok((
            PlanMutationResult::new(current, plan.clone(), false),
            vec![StoredEvent::new(
                current.metadata.session_id,
                now,
                SessionEvent::PlanRevised {
                    plan,
                    source: PlanSource::Manual,
                },
            )],
        ))
    })?;

    println!("Plan item {item_number} removed; revised snapshot saved.");
    render_plan_warnings(&result);
    Ok(())
}

fn history(args: &ArgMatches) -> Result<()> {
    match args.subcommand() {
        Some(("show", args)) => history_show(required_string(args, "session_id")),
        _ => history_list(args.get_one::<String>("search").map(String::as_str)),
    }
}

fn history_list(search: Option<&str>) -> Result<()> {
    let store = EventStore::from_environment()?;
    let events = store.read_all()?;
    let projections = reconstruct_all(&events, Utc::now())?;
    let query = search.map(|value| value.trim().to_lowercase());
    let matching: Vec<_> = projections
        .iter()
        .filter(|projection| match &query {
            None => true,
            Some(query) => {
                projection.metadata.goal.to_lowercase().contains(query)
                    || projection
                        .metadata
                        .session_id
                        .to_string()
                        .starts_with(query)
            }
        })
        .collect();

    if matching.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }

    println!("Lokin history ({} session(s)):", matching.len());
    for projection in matching.iter().rev() {
        println!(
            "{}  {}  {:<9}  budget {:>5}  active {:>8}  paused {:>8}  {}",
            format_timestamp(projection.metadata.start_time),
            projection.metadata.session_id,
            projection.effective_state,
            format!("{}m", projection.metadata.duration_minutes),
            format_duration(projection.active_work_duration),
            format_duration(projection.paused_duration),
            projection.metadata.goal
        );
    }
    Ok(())
}

fn history_show(reference: &str) -> Result<()> {
    let store = EventStore::from_environment()?;
    let events = store.read_all()?;
    let projections = reconstruct_all(&events, Utc::now())?;
    let projection = find_projection(&projections, reference)?;
    render_projection(projection);

    println!("\nCheckpoints:");
    if projection.checkpoints.is_empty() {
        println!("  None");
    } else {
        for checkpoint in &projection.checkpoints {
            println!(
                "  {}  {}",
                format_timestamp(checkpoint.occurred_at),
                checkpoint.note
            );
        }
    }

    println!("\nLatest plan:");
    render_plan_items(projection.current_plan.as_ref());

    println!("\nPlan snapshots:");
    if projection.plan_revisions.is_empty() {
        println!("  None");
    } else {
        for (index, revision) in projection.plan_revisions.iter().enumerate() {
            println!(
                "  {}. {}  {} ({} item(s))",
                index + 1,
                format_timestamp(revision.occurred_at),
                if revision.created {
                    "created"
                } else {
                    "revised"
                },
                revision.plan.items.len(),
            );
            println!("     source: {}", revision.source);
        }
    }

    println!("\nEvent timeline:");
    for event in events
        .iter()
        .filter(|event| event.session_id == projection.metadata.session_id)
    {
        println!(
            "  {}  {}{}",
            format_timestamp(event.occurred_at),
            event.event.name(),
            event_detail(&event.event)
        );
    }
    Ok(())
}

fn active_projection(projections: &[SessionProjection]) -> Result<Option<&SessionProjection>> {
    let mut active = projections.iter().filter(|projection| {
        matches!(
            projection.effective_state,
            EffectiveState::Running | EffectiveState::Paused
        )
    });
    let first = active.next();
    if active.next().is_some() {
        return Err(LokinError::CorruptLog(
            "more than one session is currently active".to_string(),
        ));
    }
    Ok(first)
}

fn events_for_session(events: &[StoredEvent], session_id: Uuid) -> Vec<StoredEvent> {
    events
        .iter()
        .filter(|event| event.session_id == session_id)
        .cloned()
        .collect()
}

fn require_active_projection(projections: &[SessionProjection]) -> Result<&SessionProjection> {
    active_projection(projections)?.ok_or(LokinError::NoActiveSession)
}

fn find_projection<'a>(
    projections: &'a [SessionProjection],
    reference: &str,
) -> Result<&'a SessionProjection> {
    let normalized = reference.trim().to_lowercase();
    let matching: Vec<_> = projections
        .iter()
        .filter(|projection| {
            projection
                .metadata
                .session_id
                .to_string()
                .starts_with(&normalized)
        })
        .collect();
    match matching.as_slice() {
        [] => Err(LokinError::UnknownSession(reference.to_string())),
        [projection] => Ok(*projection),
        _ => Err(LokinError::InvalidInput(format!(
            "Session reference '{reference}' is ambiguous. Use more of the ID."
        ))),
    }
}

fn render_projection(projection: &SessionProjection) {
    println!("Session: {}", projection.metadata.session_id);
    println!("Goal: {}", projection.metadata.goal);
    println!("State: {}", projection.effective_state);
    println!(
        "Started: {}",
        format_timestamp(projection.metadata.start_time)
    );
    println!(
        "Deadline: {}",
        format_timestamp(projection.metadata.deadline())
    );
    if let Some(stopped_at) = projection.stopped_at {
        println!("Stopped: {}", format_timestamp(stopped_at));
    }
    println!(
        "Wall-clock elapsed: {}",
        format_duration(projection.wall_clock_elapsed)
    );
    println!(
        "Remaining wall-clock: {}",
        format_duration(projection.remaining_wall_clock)
    );
    println!(
        "Active work: {}",
        format_duration(projection.active_work_duration)
    );
    println!("Paused: {}", format_duration(projection.paused_duration));
    if let Some(current_pause) = projection.current_pause_duration {
        println!("Current pause: {}", format_duration(current_pause));
    }
    println!("Checkpoints: {}", projection.checkpoints.len());
    match &projection.current_plan {
        Some(plan) => {
            let completed = plan
                .items
                .iter()
                .filter(|item| item.status == PlanItemStatus::Completed)
                .count();
            println!("Plan: {completed}/{} item(s) completed", plan.items.len());
        }
        None => println!("Plan: none"),
    }
}

fn render_plan(projection: &SessionProjection) {
    println!("Plan for session {}:", projection.metadata.session_id);
    render_plan_items(projection.current_plan.as_ref());
    if let Some(plan) = &projection.current_plan {
        if let Some(revision) = projection.plan_revisions.last() {
            println!("Source: {}", revision.source);
        }
        let total: u32 = plan
            .items
            .iter()
            .map(|item| u32::from(item.duration_minutes))
            .sum();
        println!("Total planned: {total} minutes");
        println!(
            "Remaining wall-clock: {}",
            format_duration(projection.remaining_wall_clock)
        );
    }
}

fn render_plan_items(plan: Option<&Plan>) {
    match plan {
        None => println!("  No plan yet. Add one with `lokin plan add`."),
        Some(plan) if plan.items.is_empty() => println!("  Empty plan."),
        Some(plan) => {
            for (index, item) in plan.items.iter().enumerate() {
                println!(
                    "  {}. [{}] {} ({}m)",
                    index + 1,
                    item.status,
                    item.description,
                    item.duration_minutes
                );
            }
        }
    }
}

#[derive(Debug)]
struct PlanMutationResult {
    plan: Plan,
    created: bool,
    session_budget_minutes: u16,
    remaining_wall_clock: Duration,
}

impl PlanMutationResult {
    fn new(projection: &SessionProjection, plan: Plan, created: bool) -> Self {
        Self {
            plan,
            created,
            session_budget_minutes: projection.metadata.duration_minutes,
            remaining_wall_clock: projection.remaining_wall_clock,
        }
    }
}

fn render_plan_warnings(result: &PlanMutationResult) {
    let total_minutes: i64 = result
        .plan
        .items
        .iter()
        .map(|item| i64::from(item.duration_minutes))
        .sum();
    if total_minutes > i64::from(result.session_budget_minutes) {
        println!(
            "Warning: the plan totals {total_minutes} minutes, exceeding the {}-minute session budget.",
            result.session_budget_minutes
        );
    }
    if Duration::minutes(total_minutes) > result.remaining_wall_clock {
        println!(
            "Warning: the plan exceeds the current remaining wall-clock time ({}).",
            format_duration(result.remaining_wall_clock)
        );
    }
}

fn event_detail(event: &SessionEvent) -> String {
    match event {
        SessionEvent::SessionStarted {
            goal,
            duration_minutes,
        } => format!(" — {goal} ({duration_minutes}m)"),
        SessionEvent::CheckpointAdded { note } => format!(" — {note}"),
        SessionEvent::PlanCreated { plan, source } | SessionEvent::PlanRevised { plan, source } => {
            format!(" — {} item(s), source: {source}", plan.items.len())
        }
        SessionEvent::SessionPaused
        | SessionEvent::SessionResumed
        | SessionEvent::SessionStopped => String::new(),
    }
}

fn required_string<'a>(args: &'a ArgMatches, name: &str) -> &'a str {
    args.get_one::<String>(name)
        .map(String::as_str)
        .expect("clap validates required string arguments")
}

fn validate_nonempty(value: &str, label: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(LokinError::InvalidInput(format!(
            "{label} cannot be empty."
        )));
    }
    Ok(())
}

fn validate_duration(value: u16, label: &str) -> Result<()> {
    if value == 0 {
        return Err(LokinError::InvalidInput(format!(
            "{label} must be longer than zero minutes."
        )));
    }
    Ok(())
}

fn plan_index(item_number: usize, item_count: usize) -> Result<usize> {
    if item_number == 0 || item_number > item_count {
        return Err(LokinError::InvalidInput(format!(
            "Plan item {item_number} does not exist; choose a number from 1 to {item_count}."
        )));
    }
    Ok(item_number - 1)
}

fn format_duration(duration: Duration) -> String {
    let total_seconds = duration.num_seconds().max(0);
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

fn format_timestamp(timestamp: DateTime<Utc>) -> String {
    timestamp.to_rfc3339_opts(SecondsFormat::Secs, true)
}
