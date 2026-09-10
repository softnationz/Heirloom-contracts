//! Transaction compensation via the Saga pattern (#125).
//!
//! A [`Saga`] is an ordered list of steps, each with a forward action and an
//! optional compensation registered in a [`CompensationRegistry`]. Forward
//! execution retries a failing step a bounded number of times ("forward
//! recovery"); if a step still fails, previously completed steps are undone
//! in reverse order by invoking their compensations ("backward recovery").

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

/// Output of a successful forward step, passed to its compensation if the
/// saga later needs to roll back.
pub type StepOutput = Value;

type ForwardAction = Box<dyn Fn() -> Result<StepOutput, String> + Send + Sync>;
type CompensationFn = Box<dyn Fn(&StepOutput) -> Result<(), String> + Send + Sync>;

/// Registry mapping step name to its compensation (undo) action.
///
/// Kept separate from the forward actions so compensations can be
/// registered, inspected, or swapped independently of the steps that
/// trigger them.
#[derive(Default)]
pub struct CompensationRegistry {
    compensations: HashMap<String, CompensationFn>,
}

impl CompensationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        step_name: impl Into<String>,
        compensation: impl Fn(&StepOutput) -> Result<(), String> + Send + Sync + 'static,
    ) {
        self.compensations
            .insert(step_name.into(), Box::new(compensation));
    }

    pub fn has(&self, step_name: &str) -> bool {
        self.compensations.contains_key(step_name)
    }

    fn run(&self, step_name: &str, output: &StepOutput) -> Option<Result<(), String>> {
        self.compensations.get(step_name).map(|f| f(output))
    }
}

/// A single step in a saga: a forward action plus how many times to retry it.
struct SagaStep {
    name: String,
    forward: ForwardAction,
    max_retries: u32,
}

/// Status of an individual step at the end of a saga run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SagaStepStatus {
    Completed,
    Failed,
    Compensated,
    CompensationFailed,
    NotStarted,
}

/// Overall status of a saga run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SagaStatus {
    Completed,
    Compensated,
    CompensationFailed,
}

/// Per-step record kept as part of a [`SagaExecution`].
#[derive(Clone, Debug, Serialize)]
pub struct SagaStepRecord {
    pub name: String,
    pub status: SagaStepStatus,
    pub attempts: u32,
    pub output: Option<StepOutput>,
    pub error: Option<String>,
}

/// Full state trace of one saga run, suitable for audit logs or a status API.
#[derive(Clone, Debug, Serialize)]
pub struct SagaExecution {
    pub saga_name: String,
    pub status: SagaStatus,
    pub steps: Vec<SagaStepRecord>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
}

/// Orchestrates forward execution and backward compensation of a fixed
/// sequence of steps.
pub struct Saga {
    name: String,
    steps: Vec<SagaStep>,
    compensations: CompensationRegistry,
}

/// Builder for a [`Saga`].
pub struct SagaBuilder {
    name: String,
    steps: Vec<SagaStep>,
    compensations: CompensationRegistry,
}

impl SagaBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            steps: Vec::new(),
            compensations: CompensationRegistry::new(),
        }
    }

    /// Register a forward step. `max_retries` is how many additional
    /// attempts are made after the first failure before the saga gives up
    /// and begins backward recovery.
    pub fn step(
        mut self,
        name: impl Into<String>,
        max_retries: u32,
        forward: impl Fn() -> Result<StepOutput, String> + Send + Sync + 'static,
    ) -> Self {
        self.steps.push(SagaStep {
            name: name.into(),
            forward: Box::new(forward),
            max_retries,
        });
        self
    }

    /// Register the compensation for a previously-added step.
    pub fn compensate(
        mut self,
        step_name: impl Into<String>,
        compensation: impl Fn(&StepOutput) -> Result<(), String> + Send + Sync + 'static,
    ) -> Self {
        self.compensations.register(step_name, compensation);
        self
    }

    pub fn build(self) -> Saga {
        Saga {
            name: self.name,
            steps: self.steps,
            compensations: self.compensations,
        }
    }
}

impl Saga {
    pub fn builder(name: impl Into<String>) -> SagaBuilder {
        SagaBuilder::new(name)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Run every step in order. On the first step that exhausts its
    /// retries, previously completed steps are compensated in reverse order.
    pub fn execute(&self) -> SagaExecution {
        let started_at = Utc::now();
        let mut records: Vec<SagaStepRecord> = self
            .steps
            .iter()
            .map(|s| SagaStepRecord {
                name: s.name.clone(),
                status: SagaStepStatus::NotStarted,
                attempts: 0,
                output: None,
                error: None,
            })
            .collect();

        let mut failed_at: Option<usize> = None;

        for (idx, step) in self.steps.iter().enumerate() {
            let mut attempts = 0;
            let outcome = loop {
                attempts += 1;
                match (step.forward)() {
                    Ok(output) => break Ok(output),
                    Err(err) => {
                        if attempts > step.max_retries {
                            break Err(err);
                        }
                        // Forward recovery: retry the same step.
                    }
                }
            };

            records[idx].attempts = attempts;
            match outcome {
                Ok(output) => {
                    records[idx].status = SagaStepStatus::Completed;
                    records[idx].output = Some(output);
                }
                Err(err) => {
                    records[idx].status = SagaStepStatus::Failed;
                    records[idx].error = Some(err);
                    failed_at = Some(idx);
                    break;
                }
            }
        }

        let status = match failed_at {
            None => SagaStatus::Completed,
            Some(failed_idx) => {
                // Backward recovery: undo completed steps in reverse order.
                let mut any_compensation_failed = false;
                for idx in (0..failed_idx).rev() {
                    let record = &mut records[idx];
                    if record.status != SagaStepStatus::Completed {
                        continue;
                    }
                    let output = record.output.clone().unwrap_or(Value::Null);
                    match self.compensations.run(&record.name, &output) {
                        Some(Ok(())) => {
                            record.status = SagaStepStatus::Compensated;
                        }
                        Some(Err(err)) => {
                            record.status = SagaStepStatus::CompensationFailed;
                            record.error = Some(err);
                            any_compensation_failed = true;
                        }
                        None => {
                            // No compensation registered; nothing to undo.
                        }
                    }
                }

                if any_compensation_failed {
                    SagaStatus::CompensationFailed
                } else {
                    SagaStatus::Compensated
                }
            }
        };

        SagaExecution {
            saga_name: self.name.clone(),
            status,
            steps: records,
            started_at,
            finished_at: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn all_steps_succeed() {
        let saga = Saga::builder("book-trip")
            .step("reserve-flight", 0, || Ok(serde_json::json!({"id": "f1"})))
            .step("reserve-hotel", 0, || Ok(serde_json::json!({"id": "h1"})))
            .build();

        let execution = saga.execute();
        assert_eq!(execution.status, SagaStatus::Completed);
        assert!(execution
            .steps
            .iter()
            .all(|s| s.status == SagaStepStatus::Completed));
    }

    #[test]
    fn failure_triggers_compensation_of_prior_steps() {
        let flight_compensated = Arc::new(AtomicU32::new(0));
        let flight_compensated_clone = Arc::clone(&flight_compensated);

        let saga = Saga::builder("book-trip")
            .step("reserve-flight", 0, || Ok(serde_json::json!({"id": "f1"})))
            .step("reserve-hotel", 0, || Err("hotel sold out".to_string()))
            .compensate("reserve-flight", move |_output| {
                flight_compensated_clone.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .build();

        let execution = saga.execute();
        assert_eq!(execution.status, SagaStatus::Compensated);
        assert_eq!(flight_compensated.load(Ordering::SeqCst), 1);

        assert_eq!(execution.steps[0].status, SagaStepStatus::Compensated);
        assert_eq!(execution.steps[1].status, SagaStepStatus::Failed);
    }

    #[test]
    fn forward_retry_recovers_from_transient_failure() {
        let attempts = Arc::new(AtomicU32::new(0));
        let attempts_clone = Arc::clone(&attempts);

        let saga = Saga::builder("flaky")
            .step("flaky-step", 2, move || {
                let n = attempts_clone.fetch_add(1, Ordering::SeqCst) + 1;
                if n < 2 {
                    Err("transient".to_string())
                } else {
                    Ok(serde_json::json!({"n": n}))
                }
            })
            .build();

        let execution = saga.execute();
        assert_eq!(execution.status, SagaStatus::Completed);
        assert_eq!(execution.steps[0].attempts, 2);
    }

    #[test]
    fn compensation_failure_is_recorded_but_others_still_run() {
        let hotel_compensated = Arc::new(AtomicU32::new(0));
        let hotel_compensated_clone = Arc::clone(&hotel_compensated);

        let saga = Saga::builder("book-trip")
            .step("reserve-flight", 0, || Ok(serde_json::json!({"id": "f1"})))
            .step("reserve-hotel", 0, || Ok(serde_json::json!({"id": "h1"})))
            .step("reserve-car", 0, || Err("no cars left".to_string()))
            .compensate("reserve-flight", |_| Err("refund API down".to_string()))
            .compensate("reserve-hotel", move |_| {
                hotel_compensated_clone.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .build();

        let execution = saga.execute();
        assert_eq!(execution.status, SagaStatus::CompensationFailed);
        assert_eq!(
            execution.steps[0].status,
            SagaStepStatus::CompensationFailed
        );
        assert_eq!(execution.steps[1].status, SagaStepStatus::Compensated);
        assert_eq!(hotel_compensated.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn step_without_registered_compensation_is_left_untouched() {
        let saga = Saga::builder("book-trip")
            .step("reserve-flight", 0, || Ok(serde_json::json!({"id": "f1"})))
            .step("reserve-hotel", 0, || Err("hotel sold out".to_string()))
            .build();

        let execution = saga.execute();
        assert_eq!(execution.status, SagaStatus::Compensated);
        assert_eq!(execution.steps[0].status, SagaStepStatus::Completed);
    }

    #[test]
    fn compensation_registry_reports_registered_steps() {
        let mut registry = CompensationRegistry::new();
        assert!(!registry.has("reserve-flight"));
        registry.register("reserve-flight", |_| Ok(()));
        assert!(registry.has("reserve-flight"));
    }
}
