//! MCP server over stdio. Two tools, `bitbucket_read` and `bitbucket_write`,
//! select a curated operation and run this same executable as a child process
//! with `--output json`, returning what it printed. The child, not this
//! process, runs the command handlers, so their output never reaches the
//! protocol stream on stdout.

mod skills;

use std::borrow::Cow;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, LazyLock};

use anyhow::Context;
use rmcp::handler::server::common::{schema_for_empty_input, schema_for_input};
use rmcp::handler::server::tool::{ToolCallContext, ToolRouter};
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ConstString, ContentBlock,
    CustomRequest, CustomResult, ErrorCode, GetSkillRequestMethod, GetSkillRequestParams,
    GetSkillResult, Implementation, JsonObject, ListResourcesResult, ListSkillsResult,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ReadResourceRequestParams,
    ReadResourceResponse, ReadResourceResult, ResourceContents, ResultType, ServerCapabilities,
    ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::stdio;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt, tool, tool_router};
use serde_json::{Value, json};
use skills::SkillCatalog;

/// Operations of the read-only tool, each the `bitbucket` subcommand of the
/// same name with the first underscore read as a space.
pub const READ_OPERATIONS: &[&str] = &[
    "pr_list",
    "pr_view",
    "pr_diff",
    "pr_diffstat",
    "pr_commits",
    "pr_statuses",
    "pr_activity",
    "pr_list_comments",
    "pr_view_comment",
    "pr_pipelines",
    "pipeline_list",
    "pipeline_view",
    "repo_list",
    "repo_view",
];

/// Operations of the tool that changes Bitbucket data.
pub const WRITE_OPERATIONS: &[&str] = &[
    "pr_create",
    "pr_edit",
    "pr_comment",
    "pr_approve",
    "pr_unapprove",
    "pr_request_changes",
    "pr_merge",
    "pr_decline",
    "pipeline_trigger",
    "pipeline_stop",
    "pipeline_rerun",
];

/// The fields of a tool call. One flat object serves every operation of a
/// tool: Claude drops a tool whose input schema is a top-level `oneOf`.
/// A field an operation does not take is ignored.
#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Arguments {
    /// The command to run.
    pub operation: String,
    /// Stored profile to authenticate as.
    pub profile: Option<String>,
    /// Workspace that repo_list lists; also forwarded as the global `--workspace` flag.
    pub workspace: Option<String>,
    /// Repository as `workspace/repo-slug`; every pr_* and pipeline_* operation and repo_view.
    pub repo: Option<String>,
    /// Pull request id.
    pub id: Option<u64>,
    /// Comment id of pr_view_comment.
    pub comment_id: Option<u64>,
    /// Pipeline build number of pipeline_view, pipeline_rerun and pipeline_stop.
    pub build: Option<u64>,
    /// Pipeline UUID of pipeline_stop, instead of `build`.
    pub uuid: Option<String>,
    /// Pull request states to list (open, merged, declined, superseded) of pr_list.
    #[serde(default)]
    pub state: Vec<String>,
    /// Bitbucket query filter of pr_list and repo_list.
    pub query: Option<String>,
    /// Sort field of the list operations.
    pub sort: Option<String>,
    /// Page number of the list operations.
    pub page: Option<u32>,
    /// Maximum number of results of the list operations.
    pub limit: Option<u32>,
    /// Number of pipelines pr_pipelines scans.
    pub scan_limit: Option<u32>,
    /// Role filter of repo_list (member, contributor, admin, owner).
    pub role: Option<String>,
    /// Pipeline status filter of pipeline_list.
    pub status: Option<String>,
    /// Target branch filter of pipeline_list.
    pub target_branch: Option<String>,
    /// Show logs of pipeline_view.
    #[serde(default)]
    pub logs: bool,
    /// Show full, untruncated logs of pipeline_view.
    #[serde(default)]
    pub full_logs: bool,
    /// Step name or UUID of pipeline_view.
    pub step: Option<String>,
    /// Title of pr_create (required) and pr_edit.
    pub title: Option<String>,
    /// Source branch of pr_create (required).
    pub source: Option<String>,
    /// Destination branch of pr_create and pr_edit.
    pub destination: Option<String>,
    /// Description of pr_create and pr_edit, text of pr_comment (required).
    pub body: Option<String>,
    /// Comma-separated reviewers of pr_create and pr_edit.
    pub reviewers: Option<String>,
    /// Delete the source branch after merging, for pr_create and pr_merge.
    #[serde(default)]
    pub close_source_branch: bool,
    /// Merge strategy of pr_merge: merge-commit, squash or fast-forward.
    pub strategy: Option<String>,
    /// Merge commit message of pr_merge.
    pub message: Option<String>,
    /// File path of an inline pr_comment.
    pub path: Option<String>,
    /// Line of an inline pr_comment; needs `path`.
    pub line: Option<u32>,
    /// Parent comment id of a pr_comment reply.
    pub parent: Option<u64>,
    /// Branch pipeline_trigger runs on.
    pub branch: Option<String>,
    /// Commit hash of pipeline_trigger.
    pub commit: Option<String>,
    /// Custom pipeline name of pipeline_trigger.
    pub pipeline: Option<String>,
    /// `KEY=value` variables of pipeline_trigger.
    #[serde(default)]
    pub var: Vec<String>,
    /// `KEY=value` secured variables of pipeline_trigger.
    #[serde(default)]
    pub secured_var: Vec<String>,
}

/// Which tool a call arrived on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Write,
}

/// The child's argument vector for a tool call, or the message of the tool
/// error that rejects the call before any process starts.
///
/// Every free-text value is passed as `--flag=value`, so a value that looks
/// like a flag stays the value of its own flag; the one positional, the
/// repository, is validated to start with an alphanumeric or underscore.
pub fn build_argv(tool: ToolKind, arguments: &Arguments) -> Result<Vec<String>, String> {
    let operations = match tool {
        ToolKind::Read => READ_OPERATIONS,
        ToolKind::Write => WRITE_OPERATIONS,
    };
    let operation = arguments.operation.as_str();
    if !operations.contains(&operation) {
        return Err(format!(
            "unknown `operation` {operation:?}; expected one of: {}",
            operations.join(", ")
        ));
    }
    let Some((group, command)) = operation.split_once('_') else {
        return Err(format!("`operation` {operation:?} has no command"));
    };
    let mut call = Call {
        arguments,
        argv: vec!["--output".to_owned(), "json".to_owned()],
    };
    call.text("profile", &arguments.profile)?;
    call.text("workspace", &arguments.workspace)?;
    call.argv.push(group.to_owned());
    call.argv.push(command.replace('_', "-"));
    call.operation_arguments(operation)?;
    Ok(call.argv)
}

/// One argument an operation takes: appends itself to the call's argument
/// vector, or rejects the call.
type Field = Box<dyn Fn(&mut Call) -> Step + Send + Sync>;

/// The `repo` positional.
fn repo() -> Field {
    Box::new(|call| call.repo())
}

/// A required numeric positional.
fn positional(name: &'static str, get: fn(&Arguments) -> Option<u64>) -> Field {
    Box::new(move |call| {
        call.argv
            .push(required(name, get(call.arguments))?.to_string());
        Ok(())
    })
}

/// An optional `--flag=value`.
fn text(name: &'static str, get: fn(&Arguments) -> &Option<String>) -> Field {
    Box::new(move |call| call.text(name, get(call.arguments)))
}

/// A required `--flag=value`.
fn required_text(name: &'static str, get: fn(&Arguments) -> &Option<String>) -> Field {
    Box::new(move |call| call.required_text(name, get(call.arguments)))
}

/// A repeated `--flag=value`.
fn texts(name: &'static str, get: fn(&Arguments) -> &[String]) -> Field {
    Box::new(move |call| call.texts(name, get(call.arguments)))
}

/// An optional numeric `--flag=value`.
fn number(name: &'static str, get: fn(&Arguments) -> Option<u64>) -> Field {
    Box::new(move |call| {
        call.number(name, get(call.arguments));
        Ok(())
    })
}

/// A required numeric `--flag=value`.
fn required_number(name: &'static str, get: fn(&Arguments) -> Option<u64>) -> Field {
    Box::new(move |call| {
        call.number(name, Some(required(name, get(call.arguments))?));
        Ok(())
    })
}

/// A `--flag` present only when on.
fn switch(name: &'static str, get: fn(&Arguments) -> bool) -> Field {
    Box::new(move |call| {
        call.switch(name, get(call.arguments));
        Ok(())
    })
}

/// Rejects a call with neither `build` nor `uuid`.
fn build_or_uuid() -> Field {
    Box::new(|call| {
        if call.arguments.build.is_none() && call.arguments.uuid.is_none() {
            return Err("`build` or `uuid` is required".to_owned());
        }
        Ok(())
    })
}

fn id() -> Field {
    positional("id", |a| a.id)
}
fn limit() -> Field {
    number("limit", |a| a.limit.map(u64::from))
}
fn page() -> Field {
    number("page", |a| a.page.map(u64::from))
}
fn sort() -> Field {
    text("sort", |a| &a.sort)
}
fn query() -> Field {
    text("query", |a| &a.query)
}
fn build() -> Field {
    required_number("build", |a| a.build)
}
fn body() -> Field {
    text("body", |a| &a.body)
}
fn reviewers() -> Field {
    text("reviewers", |a| &a.reviewers)
}
fn destination() -> Field {
    text("destination", |a| &a.destination)
}
fn close_source_branch() -> Field {
    switch("close_source_branch", |a| a.close_source_branch)
}

/// The arguments of each operation: the command line it builds.
static OPERATIONS: LazyLock<Vec<(&str, Vec<Field>)>> = LazyLock::new(|| {
    vec![
        (
            "pr_list",
            vec![
                repo(),
                texts("state", |a| &a.state),
                query(),
                sort(),
                page(),
                limit(),
            ],
        ),
        ("pr_view", vec![repo(), id()]),
        ("pr_diff", vec![repo(), id()]),
        ("pr_statuses", vec![repo(), id()]),
        ("pr_approve", vec![repo(), id()]),
        ("pr_unapprove", vec![repo(), id()]),
        ("pr_request_changes", vec![repo(), id()]),
        ("pr_decline", vec![repo(), id()]),
        ("pr_diffstat", vec![repo(), id(), limit()]),
        ("pr_commits", vec![repo(), id(), limit()]),
        ("pr_activity", vec![repo(), id(), limit()]),
        ("pr_list_comments", vec![repo(), id(), limit()]),
        (
            "pr_view_comment",
            vec![repo(), id(), positional("comment_id", |a| a.comment_id)],
        ),
        (
            "pr_pipelines",
            vec![
                repo(),
                id(),
                number("scan_limit", |a| a.scan_limit.map(u64::from)),
            ],
        ),
        (
            "pipeline_list",
            vec![
                repo(),
                limit(),
                text("status", |a| &a.status),
                text("target_branch", |a| &a.target_branch),
                sort(),
                page(),
            ],
        ),
        (
            "pipeline_view",
            vec![
                repo(),
                build(),
                switch("logs", |a| a.logs),
                text("step", |a| &a.step),
                switch("full_logs", |a| a.full_logs),
            ],
        ),
        (
            "repo_list",
            vec![limit(), text("role", |a| &a.role), query(), sort(), page()],
        ),
        ("repo_view", vec![repo()]),
        (
            "pr_create",
            vec![
                repo(),
                required_text("title", |a| &a.title),
                required_text("source", |a| &a.source),
                destination(),
                body(),
                reviewers(),
                close_source_branch(),
            ],
        ),
        (
            "pr_edit",
            vec![
                repo(),
                id(),
                text("title", |a| &a.title),
                body(),
                reviewers(),
                destination(),
            ],
        ),
        (
            "pr_comment",
            vec![
                repo(),
                id(),
                required_text("body", |a| &a.body),
                text("path", |a| &a.path),
                number("line", |a| a.line.map(u64::from)),
                number("parent", |a| a.parent),
            ],
        ),
        (
            "pr_merge",
            vec![
                repo(),
                id(),
                text("strategy", |a| &a.strategy),
                text("message", |a| &a.message),
                close_source_branch(),
            ],
        ),
        (
            "pipeline_trigger",
            vec![
                repo(),
                text("branch", |a| &a.branch),
                text("commit", |a| &a.commit),
                text("pipeline", |a| &a.pipeline),
                texts("var", |a| &a.var),
                texts("secured_var", |a| &a.secured_var),
            ],
        ),
        (
            "pipeline_stop",
            vec![
                repo(),
                build_or_uuid(),
                number("build", |a| a.build),
                text("uuid", |a| &a.uuid),
            ],
        ),
        ("pipeline_rerun", vec![repo(), build()]),
    ]
});

/// A tool call being turned into the child's arguments.
struct Call<'a> {
    arguments: &'a Arguments,
    argv: Vec<String>,
}

type Step = Result<(), String>;

impl Call<'_> {
    fn operation_arguments(&mut self, operation: &str) -> Step {
        let Some((_, fields)) = OPERATIONS.iter().find(|(name, _)| *name == operation) else {
            return Err(format!("unsupported `operation` {operation:?}"));
        };
        fields.iter().try_for_each(|field| field(self))
    }

    /// The `repo` positional, `workspace/repo-slug`.
    fn repo(&mut self) -> Step {
        let repo = required("repo", self.arguments.repo.as_deref())?;
        if !is_repo_slug(repo) {
            return Err(format!(
                "`repo` must be `workspace/repo-slug` with letters, digits, `-`, `_` or `.`, got {repo:?}"
            ));
        }
        self.argv.push(repo.to_owned());
        Ok(())
    }

    fn text(&mut self, field: &str, value: &Option<String>) -> Step {
        if let Some(value) = value {
            self.flag(field, value)?;
        }
        Ok(())
    }

    fn required_text(&mut self, field: &str, value: &Option<String>) -> Step {
        self.flag(field, required(field, value.as_deref())?)
    }

    fn texts(&mut self, field: &str, values: &[String]) -> Step {
        values.iter().try_for_each(|value| self.flag(field, value))
    }

    fn number(&mut self, field: &str, value: Option<impl std::fmt::Display>) {
        if let Some(value) = value {
            self.argv.push(format!("--{}={value}", flag_name(field)));
        }
    }

    fn switch(&mut self, field: &str, on: bool) {
        if on {
            self.argv.push(format!("--{}", flag_name(field)));
        }
    }

    fn flag(&mut self, field: &str, value: &str) -> Step {
        if value.contains('\0') {
            return Err(format!("`{field}` must not contain a NUL character"));
        }
        self.argv.push(format!("--{}={value}", flag_name(field)));
        Ok(())
    }
}

fn flag_name(field: &str) -> String {
    field.replace('_', "-")
}

fn required<T>(field: &str, value: Option<T>) -> Result<T, String> {
    value.ok_or_else(|| format!("missing required field `{field}`"))
}

/// Two non-empty segments of letters, digits, `-`, `_` and `.`, each starting
/// with a letter, digit or `_`, so no segment is a flag, `.` or `..`.
fn is_repo_slug(repo: &str) -> bool {
    let segment = |s: &str| {
        s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    matches!(repo.split('/').collect::<Vec<_>>().as_slice(), [w, r] if segment(w) && segment(r))
}

/// The server: it runs `program` with the argument vector of each tool call.
#[derive(Clone)]
pub struct BitbucketServer {
    program: Arc<PathBuf>,
    router: ToolRouter<Self>,
    skills: Arc<SkillCatalog>,
}

impl BitbucketServer {
    /// A server whose tool calls run `program`.
    pub fn new(program: PathBuf) -> Self {
        Self::with_skills(program, SkillCatalog::embedded().unwrap_or_default())
    }

    fn with_skills(program: PathBuf, skills: SkillCatalog) -> Self {
        Self {
            program: Arc::new(program),
            router: Self::tool_router(),
            skills: Arc::new(skills),
        }
    }

    async fn dispatch(
        &self,
        tool: ToolKind,
        arguments: JsonObject,
    ) -> Result<CallToolResult, McpError> {
        let arguments: Arguments = serde_json::from_value(Value::Object(arguments))
            .map_err(|e| McpError::invalid_params(format!("invalid arguments: {e}"), None))?;
        let argv = match build_argv(tool, &arguments) {
            Ok(argv) => argv,
            Err(message) => return Ok(tool_error(message)),
        };
        let output = tokio::process::Command::new(self.program.as_path())
            .args(&argv)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output()
            .await;
        Ok(match output {
            Ok(output) => child_result(&output),
            Err(e) => tool_error(format!("could not run {}: {e}", self.program.display())),
        })
    }
}

#[tool_router]
impl BitbucketServer {
    #[tool(
        description = "Read Bitbucket Cloud data (pull requests, pipelines, repositories) as \
                       the `bitbucket --output json` commands print it. Select the command \
                       with `operation`.",
        input_schema = input_schema(ToolKind::Read),
        annotations(read_only_hint = true)
    )]
    async fn bitbucket_read(&self, arguments: JsonObject) -> Result<CallToolResult, McpError> {
        self.dispatch(ToolKind::Read, arguments).await
    }

    #[tool(
        description = "Change Bitbucket Cloud data (create, edit, comment on, approve, merge or \
                       decline a pull request; trigger, stop or rerun a pipeline). Select the \
                       command with `operation`.",
        input_schema = input_schema(ToolKind::Write),
        annotations(destructive_hint = true, read_only_hint = false)
    )]
    async fn bitbucket_write(&self, arguments: JsonObject) -> Result<CallToolResult, McpError> {
        self.dispatch(ToolKind::Write, arguments).await
    }
}

/// Sent with the server's capabilities so the model reads the skill before the tools.
const INSTRUCTIONS: &str = "Read the resource skill://bitbucket/SKILL.md before using the \
                            bitbucket tools; it says how to use them.";

/// Whether the negotiated revision carries cache hints on list and read answers.
fn supports_cache_hints(context: &RequestContext<RoleServer>) -> bool {
    context
        .protocol_version()
        .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28)
}

impl ServerHandler for BitbucketServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_skills()
                .build(),
        )
        .with_server_info(Implementation::from_build_env())
        .with_instructions(INSTRUCTIONS)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Owned(vec![
            ProtocolVersion::V_2025_11_25,
            ProtocolVersion::V_2026_07_28,
        ])
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.router
            .call(ToolCallContext::new(self, request, context))
            .await
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let hints = supports_cache_hints(&context);
        Ok(ListToolsResult {
            result_type: Some(ResultType::COMPLETE),
            tools: self.router.list_all(),
            ttl_ms: hints.then_some(0),
            cache_scope: hints.then_some(CacheScope::Public),
            ..ListToolsResult::default()
        })
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.router.get(name).cloned()
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let mut result = ListResourcesResult::with_all_items(self.skills.resources());
        if supports_cache_hints(&context) {
            result = result.with_ttl_ms(0).with_cache_scope(CacheScope::Public);
        }
        Ok(result)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let text = self.skills.read(&request.uri).ok_or_else(|| {
            McpError::invalid_params(format!("no such resource: {}", request.uri), None)
        })?;
        let mut result = ReadResourceResult::new(vec![ResourceContents::TextResourceContents {
            uri: request.uri,
            mime_type: Some("text/markdown".to_owned()),
            text: text.to_owned(),
            meta: None,
        }]);
        if supports_cache_hints(&context) {
            result = result.with_ttl_ms(0).with_cache_scope(CacheScope::Public);
        }
        Ok(result.into())
    }

    async fn list_skills(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListSkillsResult, McpError> {
        let mut result = ListSkillsResult::with_all_items(self.skills.skills());
        if supports_cache_hints(&context) {
            result = result.with_ttl_ms(0).with_cache_scope(CacheScope::Public);
        }
        Ok(result)
    }

    async fn get_skill(
        &self,
        request: GetSkillRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetSkillResult, McpError> {
        let skill = self.skills.skill(&request.uri).ok_or_else(|| {
            McpError::invalid_params(format!("no such skill: {}", request.uri), None)
        })?;
        let mut result = GetSkillResult::new(skill.clone());
        if supports_cache_hints(&context) {
            result = result.with_ttl_ms(0).with_cache_scope(CacheScope::Public);
        }
        Ok(result)
    }

    /// A `skills/get` whose parameters did not parse (no `uri`, or one that is not text)
    /// reaches here instead of `get_skill`: a malformed request, not an unknown method.
    async fn on_custom_request(
        &self,
        request: CustomRequest,
        _context: RequestContext<RoleServer>,
    ) -> Result<CustomResult, McpError> {
        if request.method == GetSkillRequestMethod::VALUE {
            return Err(McpError::invalid_params(
                "skills/get needs a text `uri`",
                None,
            ));
        }
        Err(McpError::new(
            ErrorCode::METHOD_NOT_FOUND,
            request.method,
            None,
        ))
    }
}

/// The published input schema of a tool: the derived flat object with
/// `operation` narrowed to the operations of that tool.
fn input_schema(tool: ToolKind) -> Arc<JsonObject> {
    let operations = match tool {
        ToolKind::Read => READ_OPERATIONS,
        ToolKind::Write => WRITE_OPERATIONS,
    };
    let mut schema = schema_for_input::<Arguments>().unwrap_or_else(|_| schema_for_empty_input());
    let flat = Arc::make_mut(&mut schema);
    if let Some(Value::Object(properties)) = flat.get_mut("properties") {
        properties.insert(
            "operation".to_owned(),
            json!({
                "type": "string",
                "enum": operations,
                "description": "The command to run."
            }),
        );
    }
    flat.insert("required".to_owned(), json!(["operation"]));
    schema
}

/// A failed call: the message becomes an `isError` result.
fn tool_error(message: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message)])
}

/// The child's stdout when it exited 0; otherwise its stderr (or stdout when
/// stderr is empty) as an error result. Output is decoded lossily.
fn child_result(output: &std::process::Output) -> CallToolResult {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return CallToolResult::success(vec![ContentBlock::text(stdout)]);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let message = if !stderr.trim().is_empty() {
        stderr
    } else if !stdout.trim().is_empty() {
        stdout
    } else {
        format!("command failed: {}", output.status)
    };
    tool_error(message)
}

/// Serve MCP on stdin/stdout until the client closes the connection, running
/// this executable for each tool call.
pub async fn serve() -> anyhow::Result<()> {
    let program = std::env::current_exe().context("locating the bitbucket executable")?;
    let skills = SkillCatalog::embedded().map_err(anyhow::Error::msg)?;
    let running = BitbucketServer::with_skills(program, skills)
        .serve(stdio())
        .await
        .context("starting the MCP server")?;
    running.waiting().await.context("serving MCP")?;
    Ok(())
}

#[cfg(test)]
mod tests;
