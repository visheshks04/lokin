//! The only asynchronous boundary in Lokin.
//!
//! Session reconstruction and JSONL persistence deliberately remain synchronous.
//! This module owns the async HTTP work needed to ask a configured LLM for a plan,
//! and exposes a synchronous adapter for the command handlers.

use std::env;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::Duration;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{LokinError, Result};
use crate::session::{Plan, SessionProjection, validate_plan};

const API_KEY_ENV: &str = "GROQ_API_KEY";
const MODEL_ENV: &str = "LOKIN_LLM_MODEL";
const BASE_URL_ENV: &str = "LOKIN_LLM_BASE_URL";
const DEFAULT_BASE_URL: &str = "https://api.groq.com/openai/v1";
const REQUEST_TIMEOUT: StdDuration = StdDuration::from_secs(45);

/// User-selected provider configuration. The API key is intentionally private and
/// this type does not derive `Debug`, so it cannot accidentally appear in logs.
pub struct LlmConfig {
    api_key: String,
    model: String,
    base_url: String,
}

impl LlmConfig {
    pub fn from_environment() -> Result<Self> {
        let api_key = required_environment(API_KEY_ENV)?;
        let model = required_environment(MODEL_ENV)?;
        let base_url = env::var(BASE_URL_ENV)
            .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string())
            .trim()
            .trim_end_matches('/')
            .to_string();

        if base_url.is_empty() {
            return Err(LokinError::LlmConfiguration(format!(
                "{BASE_URL_ENV} cannot be empty when set."
            )));
        }

        Ok(Self {
            api_key,
            model,
            base_url,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }
}

fn required_environment(name: &str) -> Result<String> {
    let value = env::var(name).map_err(|_| {
        LokinError::LlmConfiguration(format!("Set {name} before using LLM planning."))
    })?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(LokinError::LlmConfiguration(format!(
            "Set {name} to a non-empty value before using LLM planning."
        )));
    }
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InferenceRequest {
    pub system_prompt: String,
    pub user_prompt: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InferenceResponse {
    pub content: String,
}

#[async_trait]
pub trait LLMInference: Send + Sync {
    async fn infer(&self, model: &str, request: &InferenceRequest) -> Result<InferenceResponse>;
}

pub struct GroqInference {
    api_key: String,
    base_url: String,
    client: Client,
}

impl GroqInference {
    pub fn from_config(config: &LlmConfig) -> Result<Self> {
        let client = Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| {
                LokinError::LlmInference(format!("could not create HTTP client: {error}"))
            })?;
        Ok(Self {
            api_key: config.api_key.clone(),
            base_url: config.base_url.clone(),
            client,
        })
    }
}

#[async_trait]
impl LLMInference for GroqInference {
    async fn infer(&self, model: &str, request: &InferenceRequest) -> Result<InferenceResponse> {
        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&ChatCompletionRequest::new(model, request))
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    LokinError::LlmInference("request timed out after 45 seconds".to_string())
                } else {
                    LokinError::LlmInference(format!("request could not be sent: {error}"))
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(LokinError::LlmInference(format!(
                "Groq returned HTTP {}.",
                status.as_u16()
            )));
        }

        let response: ChatCompletionResponse = response.json().await.map_err(|error| {
            LokinError::LlmInference(format!("response was not valid JSON: {error}"))
        })?;
        let choice = response.choices.into_iter().next().ok_or_else(|| {
            LokinError::LlmInference("response did not contain a completion choice".to_string())
        })?;
        if let Some(refusal) = choice.message.refusal {
            return Err(LokinError::LlmInference(format!(
                "model declined the request: {refusal}"
            )));
        }
        let content = choice.message.content.ok_or_else(|| {
            LokinError::LlmInference("response did not contain plan content".to_string())
        })?;
        if content.trim().is_empty() {
            return Err(LokinError::LlmInference(
                "response contained empty plan content".to_string(),
            ));
        }
        Ok(InferenceResponse { content })
    }
}

#[derive(Serialize)]
struct ChatCompletionRequest<'a> {
    model: &'a str,
    messages: [ChatMessage<'a>; 2],
    temperature: f32,
    max_completion_tokens: u16,
    stream: bool,
    response_format: ResponseFormat,
}

impl<'a> ChatCompletionRequest<'a> {
    fn new(model: &'a str, request: &'a InferenceRequest) -> Self {
        Self {
            model,
            messages: [
                ChatMessage {
                    role: "system",
                    content: &request.system_prompt,
                },
                ChatMessage {
                    role: "user",
                    content: &request.user_prompt,
                },
            ],
            temperature: 0.2,
            max_completion_tokens: 1_024,
            stream: false,
            response_format: ResponseFormat::plan_schema(),
        }
    }
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    format_type: &'static str,
    json_schema: JsonSchema,
}

#[derive(Serialize)]
struct JsonSchema {
    name: &'static str,
    strict: bool,
    schema: Value,
}

impl ResponseFormat {
    fn plan_schema() -> Self {
        Self {
            format_type: "json_schema",
            json_schema: JsonSchema {
                name: "lokin_plan",
                strict: true,
                schema: json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["items"],
                    "properties": {
                        "items": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["description", "duration_minutes", "status"],
                                "properties": {
                                    "description": { "type": "string", "minLength": 1 },
                                    "duration_minutes": { "type": "integer", "minimum": 1 },
                                    "status": {
                                        "type": "string",
                                        "enum": ["pending", "active", "completed", "skipped"]
                                    }
                                }
                            }
                        }
                    }
                }),
            },
        }
    }
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: AssistantMessage,
}

#[derive(Deserialize)]
struct AssistantMessage {
    content: Option<String>,
    refusal: Option<String>,
}

/// Synchronously invokes an async inference implementation without making the
/// rest of the CLI asynchronous.
pub fn infer_plan<I: LLMInference + ?Sized>(
    inference: &I,
    model: &str,
    request: &InferenceRequest,
    remaining_wall_clock: Duration,
) -> Result<Plan> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            LokinError::LlmInference(format!("could not start async runtime: {error}"))
        })?;
    runtime.block_on(infer_valid_plan_async(
        inference,
        model,
        request,
        remaining_wall_clock,
    ))
}

async fn infer_valid_plan_async<I: LLMInference + ?Sized>(
    inference: &I,
    model: &str,
    request: &InferenceRequest,
    remaining_wall_clock: Duration,
) -> Result<Plan> {
    let first = inference.infer(model, request).await?;
    match parse_generated_plan(&first.content, remaining_wall_clock) {
        Ok(plan) => Ok(plan),
        Err(validation_error) => {
            let corrective_request = InferenceRequest {
                system_prompt: request.system_prompt.clone(),
                user_prompt: format!(
                    "{}\n\nYour previous response was rejected: {validation_error}. \\
                     Return a corrected plan that exactly matches the required JSON schema \\
                     and fits the stated remaining wall-clock time.",
                    request.user_prompt
                ),
            };
            let corrected = inference.infer(model, &corrective_request).await?;
            parse_generated_plan(&corrected.content, remaining_wall_clock)
        }
    }
}

fn parse_generated_plan(content: &str, remaining_wall_clock: Duration) -> Result<Plan> {
    let plan: Plan = serde_json::from_str(content).map_err(|error| {
        LokinError::LlmInference(format!(
            "model response did not match the plan schema: {error}"
        ))
    })?;
    if plan.items.is_empty() {
        return Err(LokinError::LlmInference(
            "model response contained an empty plan".to_string(),
        ));
    }
    validate_plan_fits_budget(&plan, remaining_wall_clock)?;
    Ok(plan)
}

pub fn validate_plan_fits_budget(plan: &Plan, remaining_wall_clock: Duration) -> Result<()> {
    validate_plan(plan).map_err(|error| {
        LokinError::LlmInference(format!("model response contained an invalid plan: {error}"))
    })?;
    let total_minutes: i64 = plan
        .items
        .iter()
        .map(|item| i64::from(item.duration_minutes))
        .sum();
    if Duration::minutes(total_minutes) > remaining_wall_clock {
        return Err(LokinError::LlmInference(format!(
            "model plan totals {total_minutes} minutes, exceeding the remaining wall-clock time"
        )));
    }
    Ok(())
}

pub fn initial_plan_request(
    goal: &str,
    duration_minutes: u16,
    context: Option<&str>,
    feedback: &[String],
) -> InferenceRequest {
    InferenceRequest {
        system_prompt: planning_system_prompt(),
        user_prompt: format!(
            "Create an initial Lokin plan.\nGoal: {goal}\nHard wall-clock budget: {duration_minutes} minutes\n{}{}",
            optional_context(context),
            feedback_section(feedback),
        ),
    }
}

pub fn revision_plan_request(
    projection: &SessionProjection,
    context: Option<&str>,
    feedback: &[String],
) -> InferenceRequest {
    let plan = projection
        .current_plan
        .as_ref()
        .expect("revision requires a plan");
    let plan_json = serde_json::to_string(plan).expect("Plan always serializes");
    let checkpoints = if projection.checkpoints.is_empty() {
        "none".to_string()
    } else {
        projection
            .checkpoints
            .iter()
            .map(|checkpoint| {
                format!(
                    "{}: {}",
                    checkpoint.occurred_at.to_rfc3339(),
                    checkpoint.note
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    InferenceRequest {
        system_prompt: planning_system_prompt(),
        user_prompt: format!(
            "Revise this active Lokin plan as a complete replacement snapshot.\n\
             Goal: {}\n\
             Session budget: {} minutes\n\
             Remaining hard wall-clock time: {} seconds\n\
             Active work: {} seconds\n\
             Paused time: {} seconds\n\
             Current plan JSON: {plan_json}\n\
             Checkpoints:\n{checkpoints}\n{}{}",
            projection.metadata.goal,
            projection.metadata.duration_minutes,
            projection.remaining_wall_clock.num_seconds().max(0),
            projection.active_work_duration.num_seconds().max(0),
            projection.paused_duration.num_seconds().max(0),
            optional_context(context),
            feedback_section(feedback),
        ),
    }
}

fn planning_system_prompt() -> String {
    "You create concise task-agnostic Lokin plans. A Lokin deadline is immutable: pauses never extend it. Return only JSON that conforms to the supplied schema. Every plan item must have a clear description, a positive whole-minute duration, and a valid status. The total duration must fit the stated remaining hard wall-clock time.".to_string()
}

fn optional_context(context: Option<&str>) -> String {
    context
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!("Additional context: {}\n", value.trim()))
        .unwrap_or_default()
}

fn feedback_section(feedback: &[String]) -> String {
    if feedback.is_empty() {
        String::new()
    } else {
        format!("Refinement feedback:\n{}\n", feedback.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use httpmock::Method::POST;
    use httpmock::MockServer;

    use super::*;

    struct FakeInference {
        responses: Mutex<VecDeque<Result<InferenceResponse>>>,
        requests: Mutex<Vec<InferenceRequest>>,
    }

    impl FakeInference {
        fn new(responses: Vec<Result<InferenceResponse>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl LLMInference for FakeInference {
        async fn infer(
            &self,
            _model: &str,
            request: &InferenceRequest,
        ) -> Result<InferenceResponse> {
            self.requests.lock().unwrap().push(request.clone());
            self.responses.lock().unwrap().pop_front().unwrap()
        }
    }

    #[test]
    fn valid_plan_is_decoded_without_a_second_request() {
        let fake = FakeInference::new(vec![Ok(InferenceResponse {
            content: r#"{"items":[{"description":"Write tests","duration_minutes":15,"status":"pending"}]}"#.to_string(),
        })]);
        let plan = infer_plan(
            &fake,
            "test-model",
            &initial_plan_request("Write tests", 20, None, &[]),
            Duration::minutes(20),
        )
        .unwrap();

        assert_eq!(plan.items.len(), 1);
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn over_budget_plan_gets_one_corrective_request() {
        let fake = FakeInference::new(vec![
            Ok(InferenceResponse {
                content: r#"{"items":[{"description":"Too much","duration_minutes":30,"status":"pending"}]}"#.to_string(),
            }),
            Ok(InferenceResponse {
                content: r#"{"items":[{"description":"Fits","duration_minutes":10,"status":"active"}]}"#.to_string(),
            }),
        ]);
        let plan = infer_plan(
            &fake,
            "test-model",
            &initial_plan_request("Fit budget", 10, None, &[]),
            Duration::minutes(10),
        )
        .unwrap();

        assert_eq!(plan.items[0].description, "Fits");
        assert_eq!(fake.requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn initial_prompt_includes_context_and_feedback() {
        let request = initial_plan_request(
            "Learn Rust",
            30,
            Some("Focus on ownership"),
            &["Make the first task shorter".to_string()],
        );

        assert!(request.user_prompt.contains("Focus on ownership"));
        assert!(request.user_prompt.contains("Make the first task shorter"));
        assert!(request.system_prompt.contains("immutable"));
    }

    #[test]
    fn groq_inference_uses_the_openai_compatible_chat_completion_shape() {
        let server = MockServer::start();
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/openai/v1/chat/completions")
                .header("authorization", "Bearer test-key")
                .body_contains("\"model\":\"strict-model\"")
                .body_contains("\"response_format\"")
                .body_contains("\"strict\":true");
            then.status(200).json_body(json!({
                "choices": [{
                    "message": {
                        "content": r#"{"items":[{"description":"Mocked plan","duration_minutes":5,"status":"pending"}]}"#,
                        "refusal": null
                    }
                }]
            }));
        });
        let config = LlmConfig {
            api_key: "test-key".to_string(),
            model: "strict-model".to_string(),
            base_url: format!("{}/openai/v1", server.base_url()),
        };
        let inference = GroqInference::from_config(&config).unwrap();

        let plan = infer_plan(
            &inference,
            config.model(),
            &initial_plan_request("Test mock", 10, None, &[]),
            Duration::minutes(10),
        )
        .unwrap();

        mock.assert();
        assert_eq!(plan.items[0].description, "Mocked plan");
    }
}
