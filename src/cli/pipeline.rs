use anyhow::{Context, Result};
use clap::Subcommand;
use colored::{ColoredString, Colorize};
use indicatif::{ProgressBar, ProgressStyle};
use tabled::{Table, Tabled};

use super::{confirm_or_abort, parse_repo, resolve_workspace};
use crate::api::BitbucketClient;
use crate::cli::pagination::effective_limit;
use crate::models::{
    Pipeline, PipelineResultName, PipelineStateName, PipelineStep, PipelineVariable,
    PipelineVariableInput, TriggerPipelineRequest, TriggerPipelineSelector,
};

#[derive(Subcommand)]
pub enum PipelineCommands {
    /// List pipelines
    List {
        /// Repository in format workspace/repo-slug
        repo: String,

        /// Number of results (capped at 100)
        #[arg(short, long, default_value = "25")]
        limit: u32,

        /// Filter by status (PENDING, IN_PROGRESS, COMPLETED, ...)
        #[arg(long)]
        status: Option<String>,

        /// Filter by target branch name
        #[arg(long)]
        target_branch: Option<String>,

        /// Sort order (default: -created_on, most recent first)
        #[arg(long)]
        sort: Option<String>,

        /// Page number to fetch
        #[arg(long)]
        page: Option<u32>,
    },

    /// View pipeline details
    View {
        /// Repository in format workspace/repo-slug
        repo: String,

        /// Pipeline build number
        #[arg(short, long)]
        build: u64,

        /// Show step logs
        #[arg(short, long)]
        logs: bool,

        /// Show only one step: a 1-based index or a step UUID prefix
        #[arg(long)]
        step: Option<String>,

        /// Print complete step logs instead of the first 50 lines (implies --logs)
        #[arg(long)]
        full_logs: bool,
    },

    /// Trigger a new pipeline
    Trigger {
        /// Repository in format workspace/repo-slug
        repo: String,

        /// Branch to run pipeline on
        #[arg(short, long, default_value = "main")]
        branch: String,

        /// Commit hash to run the pipeline on (takes precedence over --branch)
        #[arg(long)]
        commit: Option<String>,

        /// Custom pipeline name (from bitbucket-pipelines.yml)
        #[arg(short, long)]
        pipeline: Option<String>,

        /// Pipeline variable as KEY=VALUE (repeatable)
        #[arg(long, value_name = "KEY=VALUE")]
        var: Vec<String>,

        /// Secured pipeline variable as KEY=VALUE, masked in logs (repeatable)
        #[arg(long, value_name = "KEY=VALUE")]
        secured_var: Vec<String>,

        /// Wait for pipeline to complete
        #[arg(short, long)]
        wait: bool,
    },

    /// Stop a running pipeline
    Stop {
        /// Repository in format workspace/repo-slug
        repo: String,

        /// Pipeline build number
        #[arg(short, long, required_unless_present = "uuid")]
        build: Option<u64>,

        /// Pipeline UUID (skips the build-number lookup)
        #[arg(long, conflicts_with = "build")]
        uuid: Option<String>,
    },

    /// Re-run a pipeline by triggering an equivalent one
    Rerun {
        /// Repository in format workspace/repo-slug
        repo: String,

        /// Build number of the pipeline to re-run
        #[arg(short, long)]
        build: u64,

        /// Wait for the new pipeline to complete
        #[arg(short, long)]
        wait: bool,
    },

    /// View or update the repository pipelines configuration
    Config {
        /// Repository in format workspace/repo-slug
        repo: String,

        /// Enable pipelines for the repository
        #[arg(long, conflicts_with = "disable")]
        enable: bool,

        /// Disable pipelines for the repository
        #[arg(long)]
        disable: bool,

        /// Set the next pipeline build number
        #[arg(long)]
        next_build_number: Option<u64>,
    },

    #[command(flatten)]
    Nested(NestedPipelineCommands),
}

#[derive(Subcommand)]
pub enum NestedPipelineCommands {
    /// Manage repository pipeline variables
    Variable {
        #[command(subcommand)]
        command: VariableCommands,
    },

    /// Manage workspace-level pipeline variables
    WorkspaceVariable {
        #[command(subcommand)]
        command: WorkspaceVariableCommands,
    },

    /// Manage pipeline schedules
    Schedule {
        #[command(subcommand)]
        command: ScheduleCommands,
    },

    /// Manage pipeline dependency caches
    Cache {
        #[command(subcommand)]
        command: CacheCommands,
    },
}

impl NestedPipelineCommands {
    pub async fn run(self) -> Result<()> {
        match self {
            NestedPipelineCommands::Variable { command } => command.run().await,
            NestedPipelineCommands::WorkspaceVariable { command } => command.run().await,
            NestedPipelineCommands::Schedule { command } => command.run().await,
            NestedPipelineCommands::Cache { command } => command.run().await,
        }
    }
}

#[derive(Subcommand)]
pub enum VariableCommands {
    /// List pipeline variables
    List {
        /// Repository in format workspace/repo-slug
        repo: String,
    },

    /// Create or update a pipeline variable
    Set {
        /// Repository in format workspace/repo-slug
        repo: String,

        /// Variable name
        key: String,

        /// Variable value
        value: String,

        /// Mask the value in logs and the UI
        #[arg(long)]
        secured: bool,
    },

    /// Delete a pipeline variable by key
    Delete {
        /// Repository in format workspace/repo-slug
        repo: String,

        /// Variable name
        key: String,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum WorkspaceVariableCommands {
    /// List workspace pipeline variables
    List {
        /// Workspace (defaults to --workspace or the configured default)
        workspace: Option<String>,
    },

    /// Create or update a workspace pipeline variable
    Set {
        /// Variable name
        key: String,

        /// Variable value
        value: String,

        /// Workspace (defaults to --workspace or the configured default)
        workspace: Option<String>,

        /// Mask the value in logs and the UI
        #[arg(long)]
        secured: bool,
    },

    /// Delete a workspace pipeline variable by key
    Delete {
        /// Variable name
        key: String,

        /// Workspace (defaults to --workspace or the configured default)
        workspace: Option<String>,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum ScheduleCommands {
    /// List pipeline schedules
    List {
        /// Repository in format workspace/repo-slug
        repo: String,
    },

    /// Create a pipeline schedule
    Create {
        /// Repository in format workspace/repo-slug
        repo: String,

        /// Branch to run the scheduled pipeline on
        #[arg(long)]
        branch: String,

        /// Cron pattern (e.g. "0 0 12 * * ? *")
        #[arg(long)]
        cron: String,
    },

    /// Delete a pipeline schedule by UUID
    Delete {
        /// Repository in format workspace/repo-slug
        repo: String,

        /// Schedule UUID (with or without braces)
        uuid: String,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum CacheCommands {
    /// List pipeline dependency caches
    List {
        /// Repository in format workspace/repo-slug
        repo: String,
    },

    /// Delete pipeline dependency caches by name
    Delete {
        /// Repository in format workspace/repo-slug
        repo: String,

        /// Cache name (as shown by 'cache list')
        name: String,

        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Tabled)]
struct PipelineRow {
    #[tabled(rename = "#")]
    build: u64,
    #[tabled(rename = "STATUS")]
    status: String,
    #[tabled(rename = "BRANCH")]
    branch: String,
    #[tabled(rename = "TRIGGERED")]
    triggered: String,
    #[tabled(rename = "DURATION")]
    duration: String,
}

#[derive(Tabled)]
struct VariableRow {
    #[tabled(rename = "KEY")]
    key: String,
    #[tabled(rename = "VALUE")]
    value: String,
    #[tabled(rename = "SECURED")]
    secured: String,
    #[tabled(rename = "UUID")]
    uuid: String,
}

#[derive(Tabled)]
struct ScheduleRow {
    #[tabled(rename = "ENABLED")]
    enabled: String,
    #[tabled(rename = "CRON")]
    cron: String,
    #[tabled(rename = "BRANCH")]
    branch: String,
    #[tabled(rename = "UUID")]
    uuid: String,
}

#[derive(Tabled)]
struct CacheRow {
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "PATH")]
    path: String,
    #[tabled(rename = "SIZE")]
    size: String,
    #[tabled(rename = "UUID")]
    uuid: String,
}

impl PipelineCommands {
    pub async fn run(self) -> Result<()> {
        match self {
            PipelineCommands::List {
                repo,
                limit,
                status,
                target_branch,
                sort,
                page,
            } => list(repo, limit, status, target_branch, sort, page).await,
            PipelineCommands::View {
                repo,
                build,
                logs,
                step,
                full_logs,
            } => view(repo, build, logs, step, full_logs).await,
            PipelineCommands::Trigger {
                repo,
                branch,
                commit,
                pipeline,
                var,
                secured_var,
                wait,
            } => trigger(repo, branch, commit, pipeline, var, secured_var, wait).await,
            PipelineCommands::Stop { repo, build, uuid } => stop(repo, build, uuid).await,
            PipelineCommands::Rerun { repo, build, wait } => rerun(repo, build, wait).await,
            PipelineCommands::Config {
                repo,
                enable,
                disable,
                next_build_number,
            } => config(repo, enable, disable, next_build_number).await,
            PipelineCommands::Nested(nested) => nested.run().await,
        }
    }
}

async fn list(
    repo: String,
    limit: u32,
    status: Option<String>,
    target_branch: Option<String>,
    sort: Option<String>,
    page: Option<u32>,
) -> Result<()> {
    let (workspace, repo_slug) = parse_repo(&repo)?;
    let client = BitbucketClient::from_stored().await?;
    let limit = effective_limit(limit);

    let pipelines = client
        .list_pipelines_filtered(
            &workspace,
            &repo_slug,
            crate::api::pipelines::PipelineListFilters {
                page,
                pagelen: Some(limit),
                status: status.as_deref(),
                target_branch: target_branch.as_deref(),
                sort: sort.as_deref(),
            },
        )
        .await?;

    if super::output_json() {
        return super::print_json(&pipelines.values);
    }

    if pipelines.values.is_empty() {
        println!("No pipelines found");
        return Ok(());
    }

    let rows: Vec<PipelineRow> = pipelines.values.iter().map(pipeline_row).collect();

    let table = Table::new(rows).to_string();
    println!("{}", table);

    Ok(())
}

/// The duration column: the recorded seconds, a running marker for an
/// in-progress pipeline, or a dash when neither is known.
fn duration_label(pipeline: &Pipeline) -> String {
    if let Some(seconds) = pipeline.build_seconds_used {
        format_duration(seconds)
    } else if pipeline.state.name == PipelineStateName::InProgress {
        "running...".to_string()
    } else {
        "-".to_string()
    }
}

fn pipeline_row(pipeline: &Pipeline) -> PipelineRow {
    PipelineRow {
        build: pipeline.build_number,
        status: format_status(
            &pipeline.state.name,
            pipeline.state.result.as_ref().map(|r| &r.name),
        ),
        branch: pipeline
            .target
            .ref_name
            .clone()
            .unwrap_or_else(|| "-".to_string()),
        triggered: pipeline.created_on.format("%Y-%m-%d %H:%M").to_string(),
        duration: duration_label(pipeline),
    }
}

async fn view(
    repo: String,
    build: u64,
    logs: bool,
    step: Option<String>,
    full_logs: bool,
) -> Result<()> {
    let (workspace, repo_slug) = parse_repo(&repo)?;
    let client = BitbucketClient::from_stored().await?;
    let show_logs = logs || full_logs;

    let pipeline = client
        .get_pipeline_by_build_number(&workspace, &repo_slug, build)
        .await?;

    if is_json_summary(super::output_json(), show_logs) {
        return view_json(&client, &workspace, &repo_slug, &pipeline, step.as_deref()).await;
    }

    print_pipeline_summary(&pipeline);

    let steps = client
        .list_pipeline_steps(&workspace, &repo_slug, &pipeline.uuid)
        .await?;

    match step {
        Some(selector) => {
            let detail = resolve_step(
                &client,
                &workspace,
                &repo_slug,
                &pipeline.uuid,
                &steps.values,
                &selector,
            )
            .await?;
            view_one_step(
                &client,
                &workspace,
                &repo_slug,
                &pipeline.uuid,
                &detail,
                show_logs.then_some(full_logs),
            )
            .await;
        }
        None => {
            view_all_steps(
                &client,
                &workspace,
                &repo_slug,
                &pipeline.uuid,
                &steps.values,
                show_logs.then_some(full_logs),
            )
            .await;
        }
    }

    Ok(())
}

/// Json output without logs is a structured summary; anything else prints
/// the human-readable view.
fn is_json_summary(json: bool, show_logs: bool) -> bool {
    json && !show_logs
}

async fn view_json(
    client: &BitbucketClient,
    workspace: &str,
    repo_slug: &str,
    pipeline: &Pipeline,
    step: Option<&str>,
) -> Result<()> {
    let steps = client
        .list_pipeline_steps(workspace, repo_slug, &pipeline.uuid)
        .await?;

    let steps = match step {
        Some(selector) => vec![
            resolve_step(
                client,
                workspace,
                repo_slug,
                &pipeline.uuid,
                &steps.values,
                selector,
            )
            .await?,
        ],
        None => steps.values,
    };

    super::print_json(&serde_json::json!({
        "pipeline": pipeline,
        "steps": steps,
    }))
}

/// Pick the step a `--step` selector names and say whether it was chosen by
/// UUID, in which case the caller should refetch it from the step endpoint.
fn choose_step(steps: &[PipelineStep], selector: &str) -> Result<(PipelineStep, bool)> {
    let (index, by_uuid) = select_step(steps, selector)?;
    Ok((steps[index].clone(), by_uuid))
}

/// A UUID selection points at one concrete step, so use the dedicated step
/// endpoint for the freshest detail.
async fn resolve_step(
    client: &BitbucketClient,
    workspace: &str,
    repo_slug: &str,
    pipeline_uuid: &str,
    steps: &[PipelineStep],
    selector: &str,
) -> Result<PipelineStep> {
    let (chosen, by_uuid) = choose_step(steps, selector)?;
    if by_uuid {
        client
            .get_pipeline_step(workspace, repo_slug, pipeline_uuid, &chosen.uuid)
            .await
    } else {
        Ok(chosen)
    }
}

/// The first line of the human-readable view.
fn pipeline_headline(pipeline: &Pipeline) -> String {
    format!(
        "{} Pipeline #{} - {}",
        format_status(
            &pipeline.state.name,
            pipeline.state.result.as_ref().map(|r| &r.name)
        ),
        pipeline.build_number,
        pipeline.target.ref_name.as_deref().unwrap_or("unknown")
    )
}

fn print_pipeline_summary(pipeline: &Pipeline) {
    println!("{}", pipeline_headline(pipeline));
    println!("{}", "─".repeat(60));

    if let Some(creator) = &pipeline.creator {
        println!("{} {}", "Triggered by:".dimmed(), creator.display_name);
    }

    if let Some(trigger) = &pipeline.trigger {
        println!("{} {}", "Trigger type:".dimmed(), trigger.trigger_type);
    }

    println!(
        "{} {}",
        "Started:".dimmed(),
        pipeline.created_on.format("%Y-%m-%d %H:%M:%S")
    );

    if let Some(completed) = pipeline.completed_on {
        println!(
            "{} {}",
            "Completed:".dimmed(),
            completed.format("%Y-%m-%d %H:%M:%S")
        );
    }

    if let Some(seconds) = pipeline.build_seconds_used {
        println!("{} {}", "Duration:".dimmed(), format_duration(seconds));
    }
}

/// Print one step's detail and, when `logs` carries the full-logs flag, its
/// log.
async fn view_one_step(
    client: &BitbucketClient,
    workspace: &str,
    repo_slug: &str,
    pipeline_uuid: &str,
    detail: &PipelineStep,
    logs: Option<bool>,
) {
    print_step_detail(detail);

    if let Some(full_logs) = logs {
        // A single step was explicitly requested, so a failed
        // log fetch is worth surfacing rather than swallowing.
        if let Err(e) = print_step_log(
            client,
            workspace,
            repo_slug,
            pipeline_uuid,
            &detail.uuid,
            full_logs,
        )
        .await
        {
            eprintln!(
                "{} could not fetch log for step {}: {}",
                "⚠".yellow(),
                detail.name.as_deref().unwrap_or(&detail.uuid),
                e
            );
        }
    }
}

/// Print the step list and, when `logs` carries the full-logs flag, each
/// step's log.
async fn view_all_steps(
    client: &BitbucketClient,
    workspace: &str,
    repo_slug: &str,
    pipeline_uuid: &str,
    steps: &[PipelineStep],
    logs: Option<bool>,
) {
    if steps.is_empty() {
        return;
    }

    println!();
    println!("{}", "Steps:".bold());

    for step in steps {
        let name = step.name.as_deref().unwrap_or("Step");
        println!("  {} {}", step_status_icon(step), name);

        if let Some(full_logs) = logs {
            // Iterating over every step: logs for pending or
            // in-progress steps may not exist yet, so ignore
            // fetch errors here.
            let _ = print_step_log(
                client,
                workspace,
                repo_slug,
                pipeline_uuid,
                &step.uuid,
                full_logs,
            )
            .await;
        }
    }
}

/// Parse every `--var` and `--secured-var` argument, plain ones first.
fn variable_inputs(var: &[String], secured_var: &[String]) -> Result<Vec<PipelineVariableInput>> {
    let mut variables = Vec::with_capacity(var.len() + secured_var.len());
    for (specs, secured) in [(var, false), (secured_var, true)] {
        for spec in specs {
            let (key, value) = parse_var(spec)?;
            variables.push(PipelineVariableInput {
                key,
                value,
                secured,
            });
        }
    }
    Ok(variables)
}

/// A commit takes precedence over the branch; a custom pipeline name selects
/// the pipeline from bitbucket-pipelines.yml.
fn trigger_request(
    commit: Option<&str>,
    branch: &str,
    pipeline: Option<&str>,
    variables: Vec<PipelineVariableInput>,
) -> TriggerPipelineRequest {
    let mut request = match commit {
        Some(hash) => TriggerPipelineRequest::for_commit(hash),
        None => TriggerPipelineRequest::for_branch(branch),
    };
    if let Some(pipeline_name) = pipeline {
        request = request.with_pipeline(pipeline_name);
    }
    request.with_variables(variables)
}

fn triggered_message(
    build_number: u64,
    commit: Option<&str>,
    branch: &str,
    variable_count: usize,
) -> String {
    let target_desc = match commit {
        Some(hash) => format!("commit {}", hash.cyan()),
        None => format!("branch {}", branch.cyan()),
    };
    let variable_note = if variable_count > 0 {
        format!(" with {} variable(s)", variable_count)
    } else {
        String::new()
    };
    format!(
        "{} Triggered pipeline #{} on {}{}",
        "✓".green(),
        build_number,
        target_desc,
        variable_note
    )
}

async fn trigger(
    repo: String,
    branch: String,
    commit: Option<String>,
    pipeline: Option<String>,
    var: Vec<String>,
    secured_var: Vec<String>,
    wait: bool,
) -> Result<()> {
    let (workspace, repo_slug) = parse_repo(&repo)?;
    let client = BitbucketClient::from_stored().await?;

    let variables = variable_inputs(&var, &secured_var)?;
    let variable_count = variables.len();
    let request = trigger_request(commit.as_deref(), &branch, pipeline.as_deref(), variables);

    let triggered = client
        .trigger_pipeline(&workspace, &repo_slug, &request)
        .await?;

    let message = triggered_message(
        triggered.build_number,
        commit.as_deref(),
        &branch,
        variable_count,
    );
    report_triggered(&client, &workspace, &repo_slug, &triggered, wait, &message).await
}

/// Report a freshly triggered pipeline: as json (the final pipeline when
/// waiting) or as `message`, followed by the live wait when requested.
async fn report_triggered(
    client: &BitbucketClient,
    workspace: &str,
    repo_slug: &str,
    triggered: &Pipeline,
    wait: bool,
    message: &str,
) -> Result<()> {
    if super::output_json() {
        if wait {
            let done =
                wait_for_pipeline_silently(client, workspace, repo_slug, &triggered.uuid).await?;
            return super::print_json(&done);
        }
        return super::print_json(triggered);
    }

    println!("{}", message);

    if wait {
        println!();
        wait_for_pipeline(client, workspace, repo_slug, &triggered.uuid).await?;
    }

    Ok(())
}

async fn stop(repo: String, build: Option<u64>, uuid: Option<String>) -> Result<()> {
    let (workspace, repo_slug) = parse_repo(&repo)?;
    let client = BitbucketClient::from_stored().await?;

    match (uuid, build) {
        (Some(uuid), _) => stop_by_uuid(&client, &workspace, &repo_slug, &uuid).await,
        (None, Some(build)) => stop_by_build(&client, &workspace, &repo_slug, build).await,
        (None, None) => anyhow::bail!("--build or --uuid is required"),
    }
}

async fn stop_by_uuid(
    client: &BitbucketClient,
    workspace: &str,
    repo_slug: &str,
    uuid: &str,
) -> Result<()> {
    let uuid = ensure_braced_uuid(uuid);
    client.stop_pipeline(workspace, repo_slug, &uuid).await?;
    if super::output_json() {
        return super::print_json(&serde_json::json!({"ok": true}));
    }
    println!("{} Stopped pipeline {}", "✓".green(), uuid);
    Ok(())
}

async fn stop_by_build(
    client: &BitbucketClient,
    workspace: &str,
    repo_slug: &str,
    build: u64,
) -> Result<()> {
    let pipeline = client
        .get_pipeline_by_build_number(workspace, repo_slug, build)
        .await?;

    client
        .stop_pipeline(workspace, repo_slug, &pipeline.uuid)
        .await?;

    if super::output_json() {
        return super::print_json(&serde_json::json!({"ok": true}));
    }

    println!("{} Stopped pipeline #{}", "✓".green(), build);

    Ok(())
}

/// Build the request that re-triggers an equivalent pipeline from the
/// original's branch (and custom pipeline selector, when it has one),
/// returning the branch name with it.
///
/// Bitbucket has no rerun endpoint, so only pipelines that ran against a
/// branch can be re-triggered.
fn rerun_request(original: &Pipeline, build: u64) -> Result<(String, TriggerPipelineRequest)> {
    let Some(ref_name) = original.target.ref_name.clone() else {
        anyhow::bail!(
            "Pipeline #{} has no branch to re-run from (target type '{}'). \
             Bitbucket has no rerun API, so only pipelines that ran against \
             a branch can be re-triggered. Use 'pipeline trigger' instead.",
            build,
            original.target.target_type
        );
    };

    let mut request = TriggerPipelineRequest::for_branch(&ref_name);
    if let Some(selector) = &original.target.selector {
        if let Some(pattern) = &selector.pattern {
            request.target.selector = Some(TriggerPipelineSelector {
                selector_type: selector.selector_type.clone(),
                pattern: pattern.clone(),
            });
        }
    }
    Ok((ref_name, request))
}

fn rerun_message(build: u64, new_build: u64, ref_name: &str) -> String {
    format!(
        "{} Re-ran pipeline #{} as #{} on branch {}",
        "✓".green(),
        build,
        new_build,
        ref_name.cyan()
    )
}

async fn rerun(repo: String, build: u64, wait: bool) -> Result<()> {
    let (workspace, repo_slug) = parse_repo(&repo)?;
    let client = BitbucketClient::from_stored().await?;

    let original = client
        .get_pipeline_by_build_number(&workspace, &repo_slug, build)
        .await?;

    let (ref_name, request) = rerun_request(&original, build)?;

    let new = client
        .trigger_pipeline(&workspace, &repo_slug, &request)
        .await?;

    let message = rerun_message(build, new.build_number, &ref_name);
    report_triggered(&client, &workspace, &repo_slug, &new, wait, &message).await
}

async fn config(
    repo: String,
    enable: bool,
    disable: bool,
    next_build_number: Option<u64>,
) -> Result<()> {
    let (workspace, repo_slug) = parse_repo(&repo)?;
    let client = BitbucketClient::from_stored().await?;

    if super::output_json() {
        return config_json(
            &client,
            &workspace,
            &repo_slug,
            enable || disable,
            enable,
            next_build_number,
        )
        .await;
    }

    config_text(
        &client,
        &workspace,
        &repo_slug,
        &repo,
        enable || disable,
        enable,
        next_build_number,
    )
    .await
}

async fn config_json(
    client: &BitbucketClient,
    workspace: &str,
    repo_slug: &str,
    update_enabled: bool,
    enable: bool,
    next_build_number: Option<u64>,
) -> Result<()> {
    let mut config = None;
    if update_enabled {
        config = Some(
            client
                .update_pipelines_config_enabled(workspace, repo_slug, enable)
                .await?,
        );
    }
    if let Some(next) = next_build_number {
        client
            .set_pipelines_config_build_number(workspace, repo_slug, next)
            .await?;
    }
    match config {
        Some(config) => super::print_json(&config),
        None if next_build_number.is_some() => super::print_json(&serde_json::json!({"ok": true})),
        None => super::print_json(&client.get_pipelines_config(workspace, repo_slug).await?),
    }
}

/// The colored word for a pipelines enabled state.
fn enabled_word(enabled: bool) -> ColoredString {
    if enabled {
        "enabled".green()
    } else {
        "disabled".red()
    }
}

/// The line describing the current pipelines enabled state, which may be
/// unknown.
fn enabled_state_message(enabled: Option<bool>, repo: &str) -> String {
    match enabled {
        Some(enabled) => format!("Pipelines are {} for {}", enabled_word(enabled), repo),
        None => format!("Pipelines enabled state is unknown for {}", repo),
    }
}

async fn config_text(
    client: &BitbucketClient,
    workspace: &str,
    repo_slug: &str,
    repo: &str,
    update_enabled: bool,
    enable: bool,
    next_build_number: Option<u64>,
) -> Result<()> {
    let mut changed = false;

    if update_enabled {
        let config = client
            .update_pipelines_config_enabled(workspace, repo_slug, enable)
            .await?;
        let state = enabled_word(config.enabled.unwrap_or(enable));
        println!("{} Pipelines {} for {}", "✓".green(), state, repo);
        changed = true;
    }

    if let Some(next) = next_build_number {
        client
            .set_pipelines_config_build_number(workspace, repo_slug, next)
            .await?;
        println!("{} Next build number set to {}", "✓".green(), next);
        changed = true;
    }

    if !changed {
        let config = client.get_pipelines_config(workspace, repo_slug).await?;
        println!("{}", enabled_state_message(config.enabled, repo));
    }

    Ok(())
}

impl VariableCommands {
    pub async fn run(self) -> Result<()> {
        match self {
            VariableCommands::List { repo } => {
                let (workspace, repo_slug) = parse_repo(&repo)?;
                let client = BitbucketClient::from_stored().await?;

                let variables = client
                    .list_pipeline_variables(&workspace, &repo_slug)
                    .await?;

                if super::output_json() {
                    return super::print_json(&masked_variables(&variables));
                }

                if variables.is_empty() {
                    println!("No pipeline variables found");
                    return Ok(());
                }

                let table = Table::new(variable_rows(&variables)).to_string();
                println!("{}", table);

                Ok(())
            }

            VariableCommands::Set {
                repo,
                key,
                value,
                secured,
            } => {
                let (workspace, repo_slug) = parse_repo(&repo)?;
                let client = BitbucketClient::from_stored().await?;

                let input = PipelineVariableInput {
                    key: key.clone(),
                    value,
                    secured,
                };

                let variables = client
                    .list_pipeline_variables(&workspace, &repo_slug)
                    .await?;
                match find_variable_uuid(&variables, &key) {
                    Some(uuid) => {
                        let variable = client
                            .update_pipeline_variable(&workspace, &repo_slug, &uuid, &input)
                            .await
                            .with_context(|| {
                                format!(
                                    "updating variable '{}' (it may have been deleted \
                                     concurrently — re-run to create it)",
                                    key
                                )
                            })?;
                        if super::output_json() {
                            return super::print_json(&variable);
                        }
                        println!("{} Updated pipeline variable {}", "✓".green(), key.cyan());
                    }
                    None => {
                        let variable = client
                            .create_pipeline_variable(&workspace, &repo_slug, &input)
                            .await?;
                        if super::output_json() {
                            return super::print_json(&variable);
                        }
                        println!("{} Created pipeline variable {}", "✓".green(), key.cyan());
                    }
                }

                Ok(())
            }

            VariableCommands::Delete { repo, key, yes } => {
                let (workspace, repo_slug) = parse_repo(&repo)?;
                let client = BitbucketClient::from_stored().await?;

                let variables = client
                    .list_pipeline_variables(&workspace, &repo_slug)
                    .await?;
                let uuid = find_variable_uuid(&variables, &key).ok_or_else(|| {
                    anyhow::anyhow!("No pipeline variable named '{}' in {}", key, repo)
                })?;

                if !yes && !confirm_or_abort(format!("Delete pipeline variable {}?", key.red()))? {
                    return Ok(());
                }

                client
                    .delete_pipeline_variable(&workspace, &repo_slug, &uuid)
                    .await?;

                if super::output_json() {
                    return super::print_json(&serde_json::json!({"ok": true}));
                }

                println!("{} Deleted pipeline variable {}", "✓".green(), key);

                Ok(())
            }
        }
    }
}

impl WorkspaceVariableCommands {
    pub async fn run(self) -> Result<()> {
        match self {
            WorkspaceVariableCommands::List { workspace } => {
                let workspace = resolve_workspace(workspace)?;
                let client = BitbucketClient::from_stored().await?;

                let variables = client.list_workspace_pipeline_variables(&workspace).await?;

                if super::output_json() {
                    return super::print_json(&masked_variables(&variables));
                }

                if variables.is_empty() {
                    println!("No workspace pipeline variables found");
                    return Ok(());
                }

                let table = Table::new(variable_rows(&variables)).to_string();
                println!("{}", table);

                Ok(())
            }

            WorkspaceVariableCommands::Set {
                key,
                value,
                workspace,
                secured,
            } => {
                let workspace = resolve_workspace(workspace)?;
                let client = BitbucketClient::from_stored().await?;

                let input = PipelineVariableInput {
                    key: key.clone(),
                    value,
                    secured,
                };

                let variables = client.list_workspace_pipeline_variables(&workspace).await?;
                match find_variable_uuid(&variables, &key) {
                    Some(uuid) => {
                        let variable = client
                            .update_workspace_pipeline_variable(&workspace, &uuid, &input)
                            .await
                            .with_context(|| {
                                format!(
                                    "updating variable '{}' (it may have been deleted \
                                     concurrently — re-run to create it)",
                                    key
                                )
                            })?;
                        if super::output_json() {
                            return super::print_json(&variable);
                        }
                        println!(
                            "{} Updated workspace pipeline variable {}",
                            "✓".green(),
                            key.cyan()
                        );
                    }
                    None => {
                        let variable = client
                            .create_workspace_pipeline_variable(&workspace, &input)
                            .await?;
                        if super::output_json() {
                            return super::print_json(&variable);
                        }
                        println!(
                            "{} Created workspace pipeline variable {}",
                            "✓".green(),
                            key.cyan()
                        );
                    }
                }

                Ok(())
            }

            WorkspaceVariableCommands::Delete {
                key,
                workspace,
                yes,
            } => {
                let workspace = resolve_workspace(workspace)?;
                let client = BitbucketClient::from_stored().await?;

                let variables = client.list_workspace_pipeline_variables(&workspace).await?;
                let uuid = find_variable_uuid(&variables, &key).ok_or_else(|| {
                    anyhow::anyhow!(
                        "No workspace pipeline variable named '{}' in {}",
                        key,
                        workspace
                    )
                })?;

                if !yes
                    && !confirm_or_abort(format!(
                        "Delete workspace pipeline variable {}?",
                        key.red()
                    ))?
                {
                    return Ok(());
                }

                client
                    .delete_workspace_pipeline_variable(&workspace, &uuid)
                    .await?;

                if super::output_json() {
                    return super::print_json(&serde_json::json!({"ok": true}));
                }

                println!(
                    "{} Deleted workspace pipeline variable {}",
                    "✓".green(),
                    key
                );

                Ok(())
            }
        }
    }
}

impl ScheduleCommands {
    pub async fn run(self) -> Result<()> {
        match self {
            ScheduleCommands::List { repo } => {
                let (workspace, repo_slug) = parse_repo(&repo)?;
                let client = BitbucketClient::from_stored().await?;

                let schedules = client
                    .list_pipeline_schedules(&workspace, &repo_slug)
                    .await?;

                if super::output_json() {
                    return super::print_json(&schedules);
                }

                if schedules.is_empty() {
                    println!("No pipeline schedules found");
                    return Ok(());
                }

                let rows: Vec<ScheduleRow> = schedules
                    .iter()
                    .map(|s| ScheduleRow {
                        enabled: if s.enabled.unwrap_or(false) {
                            "yes".to_string()
                        } else {
                            "no".to_string()
                        },
                        cron: s.cron_pattern.clone().unwrap_or_else(|| "-".to_string()),
                        branch: schedule_branch(s.target.as_ref()),
                        uuid: s.uuid.clone(),
                    })
                    .collect();

                let table = Table::new(rows).to_string();
                println!("{}", table);

                Ok(())
            }

            ScheduleCommands::Create { repo, branch, cron } => {
                let (workspace, repo_slug) = parse_repo(&repo)?;
                let client = BitbucketClient::from_stored().await?;

                let schedule = client
                    .create_pipeline_schedule(&workspace, &repo_slug, &branch, &cron)
                    .await?;

                if super::output_json() {
                    return super::print_json(&schedule);
                }

                println!(
                    "{} Created schedule {} on branch {} ({})",
                    "✓".green(),
                    schedule.uuid,
                    branch.cyan(),
                    schedule.cron_pattern.as_deref().unwrap_or(&cron)
                );

                Ok(())
            }

            ScheduleCommands::Delete { repo, uuid, yes } => {
                let (workspace, repo_slug) = parse_repo(&repo)?;
                let client = BitbucketClient::from_stored().await?;
                let uuid = ensure_braced_uuid(&uuid);

                if !yes && !confirm_or_abort(format!("Delete pipeline schedule {}?", uuid.red()))? {
                    return Ok(());
                }

                client
                    .delete_pipeline_schedule(&workspace, &repo_slug, &uuid)
                    .await?;

                if super::output_json() {
                    return super::print_json(&serde_json::json!({"ok": true}));
                }

                println!("{} Deleted pipeline schedule {}", "✓".green(), uuid);

                Ok(())
            }
        }
    }
}

impl CacheCommands {
    pub async fn run(self) -> Result<()> {
        match self {
            CacheCommands::List { repo } => {
                let (workspace, repo_slug) = parse_repo(&repo)?;
                let client = BitbucketClient::from_stored().await?;

                let caches = client.list_pipeline_caches(&workspace, &repo_slug).await?;

                if super::output_json() {
                    return super::print_json(&caches);
                }

                if caches.is_empty() {
                    println!("No pipeline caches found");
                    return Ok(());
                }

                let rows: Vec<CacheRow> = caches
                    .iter()
                    .map(|c| CacheRow {
                        name: c.name.clone().unwrap_or_else(|| "-".to_string()),
                        path: c.path.clone().unwrap_or_else(|| "-".to_string()),
                        size: c
                            .file_size_bytes
                            .map(format_bytes)
                            .unwrap_or_else(|| "-".to_string()),
                        uuid: c.uuid.clone(),
                    })
                    .collect();

                let table = Table::new(rows).to_string();
                println!("{}", table);

                Ok(())
            }

            CacheCommands::Delete { repo, name, yes } => {
                let (workspace, repo_slug) = parse_repo(&repo)?;
                let client = BitbucketClient::from_stored().await?;

                let caches = client.list_pipeline_caches(&workspace, &repo_slug).await?;
                let matches: Vec<_> = caches
                    .into_iter()
                    .filter(|c| c.name.as_deref() == Some(name.as_str()))
                    .collect();

                if matches.is_empty() {
                    anyhow::bail!("No pipeline cache named '{}' in {}", name, repo);
                }

                if !yes
                    && !confirm_or_abort(format!(
                        "Delete {} pipeline cache(s) named {}?",
                        matches.len(),
                        name.red()
                    ))?
                {
                    return Ok(());
                }

                let total = matches.len();
                let json = super::output_json();
                let mut deleted = 0usize;
                let mut failures: Vec<(String, anyhow::Error)> = Vec::new();

                // Delete every match rather than aborting on the first error, so
                // a single bad cache doesn't leave the rest stranded.
                for cache in &matches {
                    match client
                        .delete_pipeline_cache(&workspace, &repo_slug, &cache.uuid)
                        .await
                    {
                        Ok(()) => {
                            deleted += 1;
                            if !json {
                                println!("{} Deleted pipeline cache {}", "✓".green(), cache.uuid);
                            }
                        }
                        Err(e) => failures.push((cache.uuid.clone(), e)),
                    }
                }

                let failed = failures.len();

                if failed > 0 {
                    for (uuid, e) in &failures {
                        eprintln!(
                            "{} could not delete pipeline cache {}: {}",
                            "⚠".yellow(),
                            uuid,
                            e
                        );
                    }
                    anyhow::bail!(
                        "Deleted {} of {} pipeline cache(s) named '{}' ({} failed)",
                        deleted,
                        total,
                        name,
                        failed
                    );
                }

                if json {
                    return super::print_json(&serde_json::json!({"ok": true, "deleted": deleted}));
                }

                println!(
                    "{} Deleted {} of {} pipeline cache(s) named {}",
                    "✓".green(),
                    deleted,
                    total,
                    name
                );

                Ok(())
            }
        }
    }
}

/// Poll a pipeline every 5 seconds until it reaches a terminal state (same
/// states as [`wait_for_pipeline`]) without any terminal output, and return
/// the final pipeline for JSON rendering.
async fn wait_for_pipeline_silently(
    client: &BitbucketClient,
    workspace: &str,
    repo_slug: &str,
    pipeline_uuid: &str,
) -> Result<Pipeline> {
    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        let current = client
            .get_pipeline(workspace, repo_slug, pipeline_uuid)
            .await?;

        match current.state.name {
            PipelineStateName::Completed
            | PipelineStateName::Halted
            | PipelineStateName::Paused => return Ok(current),
            _ => {}
        }
    }
}

/// Poll a pipeline every 5 seconds until it reaches a state where waiting is
/// pointless: completed, halted, or paused (paused pipelines never progress
/// without a manual resume in the Bitbucket UI, so treat them as terminal).
async fn wait_for_pipeline(
    client: &BitbucketClient,
    workspace: &str,
    repo_slug: &str,
    pipeline_uuid: &str,
) -> Result<()> {
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::default_spinner().template("{spinner:.blue} {msg}")?);
    pb.set_message("Waiting for pipeline to complete...");

    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;

        let current = client
            .get_pipeline(workspace, repo_slug, pipeline_uuid)
            .await?;

        match current.state.name {
            PipelineStateName::Completed => {
                pb.finish_and_clear();

                if let Some(result) = &current.state.result {
                    match result.name {
                        PipelineResultName::Successful => {
                            println!(
                                "{} Pipeline #{} completed successfully!",
                                "✓".green(),
                                current.build_number
                            );
                        }
                        PipelineResultName::Failed => {
                            println!("{} Pipeline #{} failed", "✗".red(), current.build_number);
                        }
                        _ => {
                            println!(
                                "Pipeline #{} completed with status: {:?}",
                                current.build_number, result.name
                            );
                        }
                    }
                }
                break;
            }
            PipelineStateName::Halted => {
                pb.finish_and_clear();
                println!(
                    "{} Pipeline #{} was halted",
                    "⚠".yellow(),
                    current.build_number
                );
                break;
            }
            PipelineStateName::Paused => {
                pb.finish_and_clear();
                println!(
                    "{} Pipeline #{} is PAUSED and requires manual resume",
                    "⚠".yellow(),
                    current.build_number
                );
                break;
            }
            _ => {
                pb.tick();
            }
        }
    }

    Ok(())
}

/// Fetch and print a step's log, indented and dimmed. Without `full`, output
/// is truncated to the first 50 lines.
///
/// Returns any fetch error to the caller instead of swallowing it. An empty
/// log is treated as success (logs may simply not exist yet for pending or
/// in-progress steps): callers iterating over every step ignore the error,
/// while a caller that explicitly asked for one step surfaces it.
async fn print_step_log(
    client: &BitbucketClient,
    workspace: &str,
    repo_slug: &str,
    pipeline_uuid: &str,
    step_uuid: &str,
    full: bool,
) -> Result<()> {
    let log = client
        .get_step_log(workspace, repo_slug, pipeline_uuid, step_uuid)
        .await?;

    if log.is_empty() {
        return Ok(());
    }

    println!();
    if full {
        for line in log.lines() {
            println!("    {}", line.dimmed());
        }
    } else {
        let (shown, truncated) = take_log_lines(&log, 50);
        for line in shown {
            println!("    {}", line.dimmed());
        }
        if truncated {
            println!(
                "    {}",
                "... (truncated; use --full-logs for the complete log)".dimmed()
            );
        }
    }
    println!();

    Ok(())
}

/// Print the detail block for a single pipeline step.
fn print_step_detail(step: &PipelineStep) {
    println!();
    println!("{}", "Step:".bold());
    println!(
        "  {} {}",
        step_status_icon(step),
        step.name.as_deref().unwrap_or("Step")
    );
    println!("    {} {}", "UUID:".dimmed(), step.uuid);

    if let Some(state) = &step.state {
        let result = state
            .result
            .as_ref()
            .map(|r| format!(" ({})", r.name))
            .unwrap_or_default();
        println!("    {} {}{}", "State:".dimmed(), state.name, result);
    }

    if let Some(started) = step.started_on {
        println!(
            "    {} {}",
            "Started:".dimmed(),
            started.format("%Y-%m-%d %H:%M:%S")
        );
    }

    if let Some(completed) = step.completed_on {
        println!(
            "    {} {}",
            "Completed:".dimmed(),
            completed.format("%Y-%m-%d %H:%M:%S")
        );
    }

    if let Some(image) = &step.image {
        println!("    {} {}", "Image:".dimmed(), image.name);
    }

    if let Some(commands) = &step.script_commands {
        println!("    {} {}", "Script commands:".dimmed(), commands.len());
    }
}

/// Status icon for a pipeline step based on its state and result.
fn step_status_icon(step: &PipelineStep) -> ColoredString {
    let status = step
        .state
        .as_ref()
        .map(|s| s.name.as_str())
        .unwrap_or("unknown");

    match status {
        "COMPLETED" => {
            let result = step
                .state
                .as_ref()
                .and_then(|s| s.result.as_ref())
                .map(|r| r.name.as_str())
                .unwrap_or("");
            match result {
                "SUCCESSFUL" => "✓".green(),
                "FAILED" => "✗".red(),
                _ => "○".normal(),
            }
        }
        "IN_PROGRESS" => "◉".blue(),
        "PENDING" => "○".dimmed(),
        _ => "○".normal(),
    }
}

pub(crate) fn format_status(
    state: &PipelineStateName,
    result: Option<&PipelineResultName>,
) -> String {
    match state {
        PipelineStateName::Pending => "PENDING".yellow().to_string(),
        PipelineStateName::InProgress => "RUNNING".blue().to_string(),
        PipelineStateName::Paused => "PAUSED".yellow().to_string(),
        PipelineStateName::Halted => "HALTED".red().to_string(),
        PipelineStateName::Completed => {
            if let Some(result) = result {
                match result {
                    PipelineResultName::Successful => "SUCCESS".green().to_string(),
                    PipelineResultName::Failed => "FAILED".red().to_string(),
                    PipelineResultName::Error => "ERROR".red().to_string(),
                    PipelineResultName::Stopped => "STOPPED".yellow().to_string(),
                    PipelineResultName::Expired => "EXPIRED".dimmed().to_string(),
                }
            } else {
                "COMPLETED".normal().to_string()
            }
        }
    }
}

pub(crate) fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{}s", seconds)
    } else if seconds < 3600 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    }
}

/// Split a `KEY=VALUE` argument on the FIRST '=' so values may contain '='.
fn parse_var(spec: &str) -> Result<(String, String)> {
    let (key, value) = spec
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("Invalid variable '{}': expected KEY=VALUE", spec))?;

    if key.is_empty() {
        anyhow::bail!("Invalid variable '{}': key must not be empty", spec);
    }

    Ok((key.to_string(), value.to_string()))
}

/// Resolve a `--step` selector against a pipeline's steps.
///
/// A selector that parses as a number is a 1-based index; anything else is
/// matched as a case-insensitive UUID prefix (braces optional). Returns the
/// zero-based index of the step and whether it was selected by UUID.
fn select_step(steps: &[PipelineStep], selector: &str) -> Result<(usize, bool)> {
    if let Ok(index) = selector.parse::<usize>() {
        if index == 0 || index > steps.len() {
            anyhow::bail!(
                "Step index {} out of range (pipeline has {} step(s))",
                index,
                steps.len()
            );
        }
        return Ok((index - 1, false));
    }

    let clean = |s: &str| {
        s.trim_start_matches('{')
            .trim_end_matches('}')
            .to_lowercase()
    };
    let needle = clean(selector);
    if needle.is_empty() {
        anyhow::bail!("--step must be a 1-based index or a step UUID prefix");
    }

    let matches: Vec<usize> = steps
        .iter()
        .enumerate()
        .filter(|(_, s)| clean(&s.uuid).starts_with(&needle))
        .map(|(i, _)| i)
        .collect();

    match matches.as_slice() {
        [] => anyhow::bail!("No step matches '{}'", selector),
        [only] => Ok((*only, true)),
        _ => anyhow::bail!(
            "Step UUID prefix '{}' is ambiguous ({} steps match)",
            selector,
            matches.len()
        ),
    }
}

/// Wrap a UUID in braces if the user omitted them; Bitbucket API paths
/// require the braced form.
fn ensure_braced_uuid(uuid: &str) -> String {
    let uuid = uuid.trim();
    let trimmed = uuid.trim_start_matches('{').trim_end_matches('}');
    format!("{{{}}}", trimmed)
}

/// Find the UUID of the variable whose key matches exactly.
fn find_variable_uuid(variables: &[PipelineVariable], key: &str) -> Option<String> {
    variables
        .iter()
        .find(|v| v.key.as_deref() == Some(key))
        .and_then(|v| v.uuid.clone())
}

/// Return a copy of `variables` with secured values nulled out.
///
/// The API already omits `value` for secured variables, but this defends the
/// JSON path so a future/API quirk can't leak a secret that the table view
/// would have masked.
fn masked_variables(variables: &[PipelineVariable]) -> Vec<PipelineVariable> {
    variables
        .iter()
        .map(|v| {
            let mut masked = v.clone();
            if masked.secured == Some(true) {
                masked.value = None;
            }
            masked
        })
        .collect()
}

/// Table rows for repository or workspace pipeline variables. Secured values
/// are never returned by the API, so they render masked.
fn variable_rows(variables: &[PipelineVariable]) -> Vec<VariableRow> {
    variables
        .iter()
        .map(|v| {
            let secured = v.secured.unwrap_or(false);
            VariableRow {
                key: v.key.clone().unwrap_or_else(|| "-".to_string()),
                value: if secured {
                    "••••••".to_string()
                } else {
                    v.value.clone().unwrap_or_else(|| "-".to_string())
                },
                secured: if secured {
                    "yes".to_string()
                } else {
                    "no".to_string()
                },
                uuid: v.uuid.clone().unwrap_or_else(|| "-".to_string()),
            }
        })
        .collect()
}

/// Extract the branch name from a schedule's raw target object.
fn schedule_branch(target: Option<&serde_json::Value>) -> String {
    target
        .and_then(|t| t.get("ref_name"))
        .and_then(|v| v.as_str())
        .unwrap_or("-")
        .to_string()
}

/// Human-readable byte size using binary units.
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;

    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

/// Take at most `max` display lines from a log in a single pass, reporting
/// whether any lines were left out.
fn take_log_lines(log: &str, max: usize) -> (Vec<&str>, bool) {
    let mut lines = log.lines();
    let shown: Vec<&str> = lines.by_ref().take(max).collect();
    let truncated = lines.next().is_some();
    (shown, truncated)
}

#[cfg(test)]
mod tests {
    use crate::models::{
        Pipeline, PipelineStep, PipelineStepResult, PipelineStepState, PipelineVariable,
        TriggerPipelineRequest,
    };

    use super::{
        CacheCommands, NestedPipelineCommands, PipelineCommands, ScheduleCommands,
        VariableCommands, WorkspaceVariableCommands, choose_step, duration_label,
        enabled_state_message, ensure_braced_uuid, find_variable_uuid, format_bytes,
        format_duration, is_json_summary, parse_var, pipeline_headline, pipeline_row,
        rerun_message, rerun_request, schedule_branch, select_step, step_status_icon,
        take_log_lines, trigger_request, triggered_message, variable_inputs, variable_rows,
    };

    fn log_with_lines(n: usize) -> String {
        (0..n).map(|i| format!("line {}\n", i)).collect()
    }

    #[test]
    fn under_the_limit_is_not_truncated() {
        let log = log_with_lines(49);
        let (shown, truncated) = take_log_lines(&log, 50);
        assert_eq!(shown.len(), 49);
        assert!(!truncated);
    }

    #[test]
    fn exactly_the_limit_is_not_truncated() {
        let log = log_with_lines(50);
        let (shown, truncated) = take_log_lines(&log, 50);
        assert_eq!(shown.len(), 50);
        assert!(!truncated);
    }

    #[test]
    fn one_over_the_limit_is_truncated() {
        let log = log_with_lines(51);
        let (shown, truncated) = take_log_lines(&log, 50);
        assert_eq!(shown.len(), 50);
        assert!(truncated);
    }

    #[test]
    fn parse_var_splits_on_first_equals() {
        let (key, value) = parse_var("KEY=a=b=c").unwrap();
        assert_eq!(key, "KEY");
        assert_eq!(value, "a=b=c");
    }

    #[test]
    fn parse_var_accepts_empty_value() {
        let (key, value) = parse_var("KEY=").unwrap();
        assert_eq!(key, "KEY");
        assert_eq!(value, "");
    }

    #[test]
    fn parse_var_rejects_missing_equals() {
        let err = parse_var("KEYVALUE").unwrap_err();
        assert!(err.to_string().contains("expected KEY=VALUE"));
    }

    #[test]
    fn parse_var_rejects_empty_key() {
        let err = parse_var("=value").unwrap_err();
        assert!(err.to_string().contains("key must not be empty"));
    }

    fn step(uuid: &str) -> PipelineStep {
        PipelineStep {
            uuid: uuid.to_string(),
            name: Some("Step".to_string()),
            started_on: None,
            completed_on: None,
            state: None,
            image: None,
            setup_commands: None,
            script_commands: None,
            links: None,
        }
    }

    #[test]
    fn select_step_by_one_based_index() {
        let steps = [step("{aaa-111}"), step("{bbb-222}")];
        let (index, by_uuid) = select_step(&steps, "2").unwrap();
        assert_eq!(index, 1);
        assert!(!by_uuid);
    }

    #[test]
    fn select_step_rejects_index_zero() {
        let steps = [step("{aaa-111}")];
        assert!(select_step(&steps, "0").is_err());
    }

    #[test]
    fn select_step_rejects_index_out_of_range() {
        let steps = [step("{aaa-111}")];
        assert!(select_step(&steps, "2").is_err());
    }

    #[test]
    fn select_step_by_uuid_prefix_without_braces() {
        let steps = [step("{aaa-111}"), step("{bbb-222}")];
        let (index, by_uuid) = select_step(&steps, "bbb").unwrap();
        assert_eq!(index, 1);
        assert!(by_uuid);
    }

    #[test]
    fn select_step_by_full_braced_uuid() {
        let steps = [step("{aaa-111}"), step("{bbb-222}")];
        let (index, by_uuid) = select_step(&steps, "{bbb-222}").unwrap();
        assert_eq!(index, 1);
        assert!(by_uuid);
    }

    #[test]
    fn select_step_uuid_prefix_is_case_insensitive() {
        let steps = [step("{ABC-123}")];
        let (index, _) = select_step(&steps, "abc").unwrap();
        assert_eq!(index, 0);
    }

    #[test]
    fn select_step_rejects_ambiguous_prefix() {
        let steps = [step("{abc-111}"), step("{abc-222}")];
        let err = select_step(&steps, "abc").unwrap_err();
        assert!(err.to_string().contains("ambiguous"));
    }

    #[test]
    fn select_step_rejects_unknown_prefix() {
        let steps = [step("{aaa-111}")];
        assert!(select_step(&steps, "zzz").is_err());
    }

    #[test]
    fn ensure_braced_uuid_wraps_bare_uuids() {
        assert_eq!(ensure_braced_uuid("abc-123"), "{abc-123}");
    }

    #[test]
    fn ensure_braced_uuid_keeps_braced_uuids() {
        assert_eq!(ensure_braced_uuid("{abc-123}"), "{abc-123}");
    }

    #[test]
    fn find_variable_uuid_matches_key_exactly() {
        let variables = vec![
            crate::models::PipelineVariable {
                uuid: Some("{u1}".to_string()),
                key: Some("FOO".to_string()),
                value: None,
                secured: None,
            },
            crate::models::PipelineVariable {
                uuid: Some("{u2}".to_string()),
                key: Some("FOOBAR".to_string()),
                value: None,
                secured: None,
            },
        ];

        assert_eq!(
            find_variable_uuid(&variables, "FOO").as_deref(),
            Some("{u1}")
        );
        assert_eq!(find_variable_uuid(&variables, "BAR"), None);
    }

    #[test]
    fn schedule_branch_reads_ref_name_from_target() {
        let target = serde_json::json!({"type": "pipeline_ref_target", "ref_name": "main"});
        assert_eq!(schedule_branch(Some(&target)), "main");
    }

    #[test]
    fn schedule_branch_falls_back_to_dash() {
        assert_eq!(schedule_branch(None), "-");
        let target = serde_json::json!({"type": "pipeline_ref_target"});
        assert_eq!(schedule_branch(Some(&target)), "-");
    }

    #[test]
    fn format_bytes_uses_binary_units() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KiB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MiB");
    }

    fn step_with_state(state: &str, result: Option<&str>) -> PipelineStep {
        PipelineStep {
            uuid: "{step}".to_string(),
            name: Some("Step".to_string()),
            started_on: None,
            completed_on: None,
            state: Some(PipelineStepState {
                name: state.to_string(),
                state_type: "pipeline_step_state".to_string(),
                result: result.map(|r| PipelineStepResult {
                    name: r.to_string(),
                    result_type: "pipeline_step_result".to_string(),
                }),
            }),
            image: None,
            setup_commands: None,
            script_commands: None,
            links: None,
        }
    }

    #[test]
    fn step_status_icon_per_state() {
        // Assert on the underlying glyph via `contains` so the result holds
        // whether or not ANSI colouring is active in the test environment.
        assert!(
            step_status_icon(&step_with_state("COMPLETED", Some("SUCCESSFUL")))
                .to_string()
                .contains('✓')
        );
        assert!(
            step_status_icon(&step_with_state("COMPLETED", Some("FAILED")))
                .to_string()
                .contains('✗')
        );
        assert!(
            step_status_icon(&step_with_state("COMPLETED", Some("STOPPED")))
                .to_string()
                .contains('○')
        );
        assert!(
            step_status_icon(&step_with_state("IN_PROGRESS", None))
                .to_string()
                .contains('◉')
        );
        assert!(
            step_status_icon(&step_with_state("PENDING", None))
                .to_string()
                .contains('○')
        );

        // No state at all falls back to the neutral marker.
        let mut stateless = step_with_state("PENDING", None);
        stateless.state = None;
        assert!(step_status_icon(&stateless).to_string().contains('○'));
    }

    fn variable(key: &str, value: Option<&str>, secured: Option<bool>) -> PipelineVariable {
        PipelineVariable {
            uuid: Some("{u}".to_string()),
            key: Some(key.to_string()),
            value: value.map(|v| v.to_string()),
            secured,
        }
    }

    #[test]
    fn variable_rows_masks_secured_values() {
        let vars = vec![
            variable("PLAIN", Some("hello"), Some(false)),
            variable("SECRET", Some("shh"), Some(true)),
        ];
        let rows = variable_rows(&vars);

        assert_eq!(rows[0].value, "hello");
        assert_eq!(rows[0].secured, "no");

        assert_eq!(rows[1].value, "••••••");
        assert_eq!(rows[1].secured, "yes");
    }

    fn pipeline_from(value: serde_json::Value) -> Pipeline {
        let mut base = serde_json::json!({
            "uuid": "{p-1}",
            "build_number": 7,
            "target": {"type": "pipeline_ref_target", "ref_type": "branch", "ref_name": "main"},
            "state": {"name": "COMPLETED", "type": "pipeline_state_completed"},
            "created_on": "2024-03-05T10:30:00Z",
        });
        for (key, v) in value.as_object().unwrap() {
            base[key] = v.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn variable_inputs_without_arguments_is_empty() {
        assert!(variable_inputs(&[], &[]).unwrap().is_empty());
    }

    #[test]
    fn variable_inputs_lists_plain_before_secured_with_their_flags() {
        let var = vec!["A=1".to_string(), "B=x=y".to_string()];
        let secured = vec!["TOKEN=s3".to_string()];
        let inputs = variable_inputs(&var, &secured).unwrap();

        let seen: Vec<(&str, &str, bool)> = inputs
            .iter()
            .map(|i| (i.key.as_str(), i.value.as_str(), i.secured))
            .collect();
        assert_eq!(
            seen,
            vec![
                ("A", "1", false),
                ("B", "x=y", false),
                ("TOKEN", "s3", true)
            ]
        );
    }

    #[test]
    fn variable_inputs_fails_on_a_malformed_secured_argument() {
        let err = variable_inputs(&["A=1".to_string()], &["BROKEN".to_string()]).unwrap_err();
        assert!(err.to_string().contains("Invalid variable 'BROKEN'"));
    }

    #[test]
    fn trigger_request_targets_the_branch_without_a_commit() {
        let request = trigger_request(None, "develop", None, vec![]);
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({
                "target": {
                    "type": "pipeline_ref_target",
                    "ref_type": "branch",
                    "ref_name": "develop"
                }
            })
        );
    }

    #[test]
    fn trigger_request_prefers_the_commit_over_the_branch() {
        let request = trigger_request(Some("abc123"), "develop", None, vec![]);
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({
                "target": {
                    "type": "pipeline_commit_target",
                    "commit": {"hash": "abc123", "type": "commit"}
                }
            })
        );
    }

    #[test]
    fn trigger_request_selects_a_custom_pipeline_and_carries_variables() {
        let variables = variable_inputs(&["A=1".to_string()], &["S=2".to_string()]).unwrap();
        let request = trigger_request(None, "main", Some("deploy"), variables);
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::json!({
                "target": {
                    "type": "pipeline_ref_target",
                    "ref_type": "branch",
                    "ref_name": "main",
                    "selector": {"type": "custom", "pattern": "deploy"}
                },
                "variables": [
                    {"key": "A", "value": "1", "secured": false},
                    {"key": "S", "value": "2", "secured": true}
                ]
            })
        );
    }

    #[test]
    fn triggered_message_names_the_branch_and_omits_the_variable_note_without_variables() {
        let message = triggered_message(12, None, "main", 0);
        assert!(message.contains("Triggered pipeline #12 on branch"));
        assert!(message.contains("main"));
        assert!(!message.contains("variable(s)"));
    }

    #[test]
    fn triggered_message_names_the_commit_and_counts_variables() {
        let message = triggered_message(12, Some("abc123"), "main", 3);
        assert!(message.contains("on commit"));
        assert!(message.contains("abc123"));
        assert!(!message.contains("branch"));
        assert!(message.ends_with(" with 3 variable(s)"));
    }

    #[test]
    fn rerun_request_retriggers_the_original_branch() {
        let original = pipeline_from(serde_json::json!({}));
        let (ref_name, request) = rerun_request(&original, 7).unwrap();

        assert_eq!(ref_name, "main");
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            serde_json::to_value(TriggerPipelineRequest::for_branch("main")).unwrap()
        );
    }

    #[test]
    fn rerun_request_keeps_the_original_selector_pattern() {
        let original = pipeline_from(serde_json::json!({
            "target": {
                "type": "pipeline_ref_target",
                "ref_type": "branch",
                "ref_name": "release",
                "selector": {"type": "custom", "pattern": "deploy"}
            }
        }));
        let (_, request) = rerun_request(&original, 7).unwrap();

        let selector = request.target.selector.unwrap();
        assert_eq!(selector.selector_type, "custom");
        assert_eq!(selector.pattern, "deploy");
    }

    #[test]
    fn rerun_request_drops_a_selector_without_pattern() {
        let original = pipeline_from(serde_json::json!({
            "target": {
                "type": "pipeline_ref_target",
                "ref_name": "main",
                "selector": {"type": "default"}
            }
        }));
        let (_, request) = rerun_request(&original, 7).unwrap();
        assert!(request.target.selector.is_none());
    }

    #[test]
    fn rerun_request_rejects_a_pipeline_without_a_branch() {
        let original = pipeline_from(serde_json::json!({
            "target": {"type": "pipeline_commit_target"}
        }));
        let err = rerun_request(&original, 9).unwrap_err().to_string();

        assert!(err.contains("Pipeline #9 has no branch to re-run from"));
        assert!(err.contains("target type 'pipeline_commit_target'"));
        assert!(err.contains("Use 'pipeline trigger' instead."));
    }

    #[test]
    fn rerun_message_links_old_and_new_builds() {
        let message = rerun_message(7, 8, "main");
        assert!(message.contains("Re-ran pipeline #7 as #8 on branch"));
        assert!(message.contains("main"));
    }

    #[test]
    fn json_summary_only_without_logs() {
        assert!(is_json_summary(true, false));
        assert!(!is_json_summary(true, true));
        assert!(!is_json_summary(false, false));
        assert!(!is_json_summary(false, true));
    }

    #[test]
    fn choose_step_by_index_is_not_refetched() {
        let steps = [step("{aaa-111}"), step("{bbb-222}")];
        let (chosen, by_uuid) = choose_step(&steps, "2").unwrap();
        assert_eq!(chosen.uuid, "{bbb-222}");
        assert!(!by_uuid);
    }

    #[test]
    fn choose_step_by_uuid_prefix_asks_for_a_refetch() {
        let steps = [step("{aaa-111}"), step("{bbb-222}")];
        let (chosen, by_uuid) = choose_step(&steps, "BBB").unwrap();
        assert_eq!(chosen.uuid, "{bbb-222}");
        assert!(by_uuid);
    }

    #[test]
    fn choose_step_reports_an_unknown_step() {
        let steps = [step("{aaa-111}")];
        assert!(choose_step(&steps, "3").is_err());
        assert!(choose_step(&steps, "zzz").is_err());
    }

    #[test]
    fn duration_label_prefers_recorded_seconds() {
        let pipeline = pipeline_from(serde_json::json!({"build_seconds_used": 75}));
        assert_eq!(duration_label(&pipeline), format_duration(75));
    }

    #[test]
    fn duration_label_marks_a_running_pipeline() {
        let pipeline = pipeline_from(serde_json::json!({
            "state": {"name": "IN_PROGRESS", "type": "pipeline_state_in_progress"}
        }));
        assert_eq!(duration_label(&pipeline), "running...");
    }

    #[test]
    fn duration_label_is_a_dash_when_unknown() {
        let pipeline = pipeline_from(serde_json::json!({
            "state": {"name": "PENDING", "type": "pipeline_state_pending"}
        }));
        assert_eq!(duration_label(&pipeline), "-");
    }

    #[test]
    fn pipeline_row_formats_the_columns() {
        let row = pipeline_row(&pipeline_from(serde_json::json!({"build_seconds_used": 5})));
        assert_eq!(row.build, 7);
        assert_eq!(row.branch, "main");
        assert_eq!(row.triggered, "2024-03-05 10:30");
        assert_eq!(row.duration, format_duration(5));
    }

    #[test]
    fn pipeline_row_uses_a_dash_for_a_missing_branch() {
        let row = pipeline_row(&pipeline_from(serde_json::json!({
            "target": {"type": "pipeline_commit_target"}
        })));
        assert_eq!(row.branch, "-");
    }

    #[test]
    fn pipeline_headline_shows_build_and_branch() {
        let headline = pipeline_headline(&pipeline_from(serde_json::json!({})));
        assert!(headline.contains("Pipeline #7 - main"));
    }

    #[test]
    fn pipeline_headline_falls_back_to_unknown_branch() {
        let headline = pipeline_headline(&pipeline_from(serde_json::json!({
            "target": {"type": "pipeline_commit_target"}
        })));
        assert!(headline.ends_with("Pipeline #7 - unknown"));
    }

    #[test]
    fn enabled_state_message_covers_every_state() {
        assert!(enabled_state_message(Some(true), "w/r").contains("Pipelines are"));
        assert!(enabled_state_message(Some(true), "w/r").contains("enabled"));
        assert!(enabled_state_message(Some(false), "w/r").contains("disabled"));
        assert_eq!(
            enabled_state_message(None, "w/r"),
            "Pipelines enabled state is unknown for w/r"
        );
    }

    fn parse_pipeline(args: &[&str]) -> PipelineCommands {
        use clap::Parser;
        let mut argv = vec!["bitbucket", "pipeline"];
        argv.extend_from_slice(args);
        match crate::cli::Cli::try_parse_from(argv) {
            Ok(crate::cli::Cli {
                command: crate::cli::Commands::Pipeline { command },
                ..
            }) => command,
            Ok(_) => panic!("not a pipeline command"),
            Err(e) => panic!("rejected by the CLI: {e}"),
        }
    }

    #[test]
    fn nested_subcommands_parse_with_their_flags_through_the_top_level_cli() {
        assert!(matches!(
            parse_pipeline(&["variable", "set", "acme/web", "K", "V", "--secured"]),
            PipelineCommands::Nested(NestedPipelineCommands::Variable {
                command: VariableCommands::Set { secured: true, .. }
            })
        ));
        assert!(matches!(
            parse_pipeline(&["workspace-variable", "delete", "K", "acme", "--yes"]),
            PipelineCommands::Nested(NestedPipelineCommands::WorkspaceVariable {
                command: WorkspaceVariableCommands::Delete { yes: true, .. }
            })
        ));
        assert!(matches!(
            parse_pipeline(&[
                "schedule",
                "create",
                "acme/web",
                "--branch",
                "main",
                "--cron",
                "0 0 12 * * ? *"
            ]),
            PipelineCommands::Nested(NestedPipelineCommands::Schedule {
                command: ScheduleCommands::Create { .. }
            })
        ));
        assert!(matches!(
            parse_pipeline(&["cache", "delete", "acme/web", "node", "-y"]),
            PipelineCommands::Nested(NestedPipelineCommands::Cache {
                command: CacheCommands::Delete { yes: true, .. }
            })
        ));
    }

    #[test]
    fn pipeline_help_lists_all_subcommands_in_declaration_order() {
        use clap::CommandFactory;
        let cli = crate::cli::Cli::command();
        let pipeline = cli.find_subcommand("pipeline").expect("pipeline command");
        let names: Vec<&str> = pipeline
            .get_subcommands()
            .map(|c| c.get_name())
            .filter(|n| *n != "help")
            .collect();
        assert_eq!(
            names,
            [
                "list",
                "view",
                "trigger",
                "stop",
                "rerun",
                "config",
                "variable",
                "workspace-variable",
                "schedule",
                "cache"
            ]
        );
    }
}
