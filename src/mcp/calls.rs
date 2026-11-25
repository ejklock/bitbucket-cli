//! Runs the child process of one tool call. A call is awaited in the request itself or as an
//! MCP task; either way it reports a heartbeat while the child runs, and a cancel kills the
//! child: the child future is dropped inside a `select!` against the cancel future, and the
//! child was started with `kill_on_drop`.

use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{CallToolResult, ProgressNotificationParam, ProgressToken, Task};
use rmcp::task_manager::{TaskContext, TaskExit, TaskManager, TaskOptions};
use rmcp::{Peer, RoleServer};
use tokio::time::{Instant, MissedTickBehavior, interval_at};

use super::{child_result, tool_error};

/// How often a running call reports that it is alive.
pub(super) const HEARTBEAT: Duration = Duration::from_secs(5);

/// Where a call's heartbeat goes: progress notifications to the token the client sent, if any,
/// and, for a task, the task's status message.
pub(super) struct Relay {
    peer: Peer<RoleServer>,
    token: Option<ProgressToken>,
    task: Option<TaskContext>,
}

impl Relay {
    pub(super) fn new(peer: Peer<RoleServer>, token: Option<ProgressToken>) -> Self {
        Self {
            peer,
            token,
            task: None,
        }
    }

    async fn beat(&self, beats: u32, running_for: Duration) {
        let message = format!("running for {}s", running_for.as_secs());
        if let Some(task) = &self.task {
            task.set_status_message(message.clone());
        }
        if let Some(token) = &self.token {
            let progress = ProgressNotificationParam::new(token.clone(), f64::from(beats))
                .with_message(message);
            let _ = self.peer.notify_progress(progress).await;
        }
    }
}

/// How a call ended.
enum Outcome {
    Finished(CallToolResult),
    Cancelled,
}

/// Runs `program` for each call, with a heartbeat of `heartbeat`.
#[derive(Clone)]
pub(super) struct ChildRunner {
    program: Arc<PathBuf>,
    heartbeat: Duration,
}

impl ChildRunner {
    pub(super) fn new(program: PathBuf, heartbeat: Duration) -> Self {
        Self {
            program: Arc::new(program),
            heartbeat,
        }
    }

    /// Runs the call in the request: its result, or an error result once `cancel` resolves and
    /// the child is killed.
    pub(super) async fn run_here(
        &self,
        argv: &[String],
        relay: &Relay,
        cancel: impl Future<Output = ()>,
    ) -> CallToolResult {
        match self.run(argv, relay, cancel).await {
            Outcome::Finished(result) => result,
            Outcome::Cancelled => tool_error("the call was cancelled".to_owned()),
        }
    }

    /// Runs the call as a task that never expires; `tasks/cancel` kills the child and ends the
    /// task `cancelled`.
    pub(super) fn run_as_task(&self, tasks: &TaskManager, argv: Vec<String>, relay: Relay) -> Task {
        let runner = self.clone();
        let options = TaskOptions::new().with_ttl_ms(None);
        tasks.spawn(options, move |task| {
            let relay = Relay {
                task: Some(task.clone()),
                ..relay
            };
            Box::pin(async move {
                match runner.run(&argv, &relay, task.cancelled()).await {
                    Outcome::Finished(result) => Ok(result),
                    Outcome::Cancelled => Err(TaskExit::Cancelled),
                }
            })
        })
    }

    async fn run(
        &self,
        argv: &[String],
        relay: &Relay,
        cancel: impl Future<Output = ()>,
    ) -> Outcome {
        let could_not_run = |e: std::io::Error| {
            Outcome::Finished(tool_error(format!(
                "could not run {}: {e}",
                self.program.display()
            )))
        };
        let child = tokio::process::Command::new(self.program.as_path())
            .args(argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        let child = match child {
            Ok(child) => child,
            Err(e) => return could_not_run(e),
        };
        let output = child.wait_with_output();
        tokio::pin!(output, cancel);

        let started = Instant::now();
        let mut ticks = interval_at(started + self.heartbeat, self.heartbeat);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut beats = 0_u32;
        loop {
            tokio::select! {
                finished = &mut output => {
                    return match finished {
                        Ok(output) => Outcome::Finished(child_result(&output)),
                        Err(e) => could_not_run(e),
                    };
                }
                () = &mut cancel => return Outcome::Cancelled,
                _ = ticks.tick() => {
                    beats += 1;
                    relay.beat(beats, started.elapsed()).await;
                }
            }
        }
    }
}
