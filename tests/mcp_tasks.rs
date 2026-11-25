//! Drives `bitbucket mcp` as a raw JSON-RPC client over an in-memory pipe, against a stub child
//! program: the task a long call becomes for a client that declares the tasks extension, the
//! synchronous calls, cancellation of both and the heartbeat. No Bitbucket API is involved.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use bitbucket_cli::mcp::BitbucketServer;
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};

const TASKS_EXTENSION: &str = "io.modelcontextprotocol/tasks";
const WAIT: Duration = Duration::from_secs(20);
const HEARTBEAT: Duration = Duration::from_millis(100);

/// A stand-in for the bitbucket binary. The scan limit picks what it does: 1 sleeps briefly and
/// echoes its arguments, 2 records its pid and sleeps until killed, 3 runs for several
/// heartbeats; every other call echoes its arguments at once. An argument saying `fail` exits 3.
struct Stub {
    dir: tempfile::TempDir,
    program: PathBuf,
}

impl Stub {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("a scratch folder");
        let pidfile = dir.path().join("pid");
        let program = dir.path().join("stub.sh");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\n\
                 case \"$*\" in *fail*) echo boom >&2; exit 3;; esac\n\
                 case \"$*\" in *--scan-limit=2*) echo $$ > '{pid}'; exec sleep 30;; esac\n\
                 case \"$*\" in *--scan-limit=1*) sleep 0.3;; esac\n\
                 case \"$*\" in *--scan-limit=3*) sleep 0.8;; esac\n\
                 printf 'arg:%s\\n' \"$@\"\n",
                pid = pidfile.display()
            ),
        )
        .expect("the stub is written");
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            .expect("the stub is executable");
        Self { dir, program }
    }

    fn pidfile(&self) -> PathBuf {
        self.dir.path().join("pid")
    }

    /// The pid the stub recorded, once it has started.
    async fn pid(&self) -> u32 {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Ok(text) = std::fs::read_to_string(self.pidfile())
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            assert!(Instant::now() < deadline, "the stub never started");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Whether the process is gone within five seconds.
async fn gone_within_five_seconds(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(pid) {
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    true
}

/// A raw line-delimited JSON-RPC session; every message the server sends is kept, in order.
struct Session {
    writer: WriteHalf<DuplexStream>,
    reader: Lines<BufReader<ReadHalf<DuplexStream>>>,
    inbox: Vec<Value>,
    next_id: u64,
    server: BitbucketServer,
}

impl Session {
    /// A session whose client declares the tasks extension.
    async fn declaring(program: &Path) -> Self {
        Self::start(program, true).await
    }

    /// A session whose client does not declare the tasks extension.
    async fn plain(program: &Path) -> Self {
        Self::start(program, false).await
    }

    async fn start(program: &Path, tasks: bool) -> Self {
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        let server = BitbucketServer::with_heartbeat(program.to_path_buf(), HEARTBEAT);
        let serving = server.clone();
        tokio::spawn(async move {
            use rmcp::ServiceExt;
            if let Ok(running) = serving.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        let (reader, writer) = tokio::io::split(client_io);
        let mut session = Self {
            writer,
            reader: BufReader::new(reader).lines(),
            inbox: Vec::new(),
            next_id: 1,
            server,
        };
        let capabilities = if tasks {
            json!({"extensions": {TASKS_EXTENSION: {}}})
        } else {
            json!({})
        };
        let id = session
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": capabilities,
                    "clientInfo": {"name": "mcp-tasks-test", "version": "0"},
                }),
            )
            .await;
        let initialized = session.answer(id).await;
        assert_eq!(
            initialized["result"]["capabilities"]["extensions"][TASKS_EXTENSION],
            json!({}),
            "the server declares the tasks extension: {initialized}"
        );
        session
            .send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        session
    }

    async fn send(&mut self, message: &Value) {
        let line = format!("{message}\n");
        self.writer
            .write_all(line.as_bytes())
            .await
            .expect("the server reads");
    }

    async fn request(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        id
    }

    /// Starts a tool call; the progress token is named after the call when asked for.
    async fn start_call(&mut self, tool: &str, arguments: Value, progress: bool) -> u64 {
        let mut params = json!({"name": tool, "arguments": arguments});
        if progress {
            params["_meta"] = json!({"progressToken": format!("token-{}", self.next_id)});
        }
        self.request("tools/call", params).await
    }

    async fn pump(&mut self, wait: Duration) -> bool {
        match tokio::time::timeout(wait, self.reader.next_line()).await {
            Ok(Ok(Some(line))) => {
                let message = serde_json::from_str(&line)
                    .unwrap_or_else(|error| panic!("not JSON ({error}): {line:?}"));
                self.inbox.push(message);
                true
            }
            _ => false,
        }
    }

    fn find(&self, id: u64) -> Option<Value> {
        self.inbox
            .iter()
            .find(|message| message["id"] == json!(id) && message.get("method").is_none())
            .cloned()
    }

    async fn answer(&mut self, id: u64) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(found) = self.find(id) {
                return found;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero() && self.pump(left).await,
                "request {id} was not answered within {WAIT:?}"
            );
        }
    }

    /// Reads whatever arrives for the given time.
    async fn listen(&mut self, wait: Duration) {
        let deadline = Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || !self.pump(left).await {
                return;
            }
        }
    }

    async fn call(&mut self, tool: &str, arguments: Value) -> Value {
        let id = self.start_call(tool, arguments, false).await;
        self.answer(id).await["result"].clone()
    }

    /// Calls `tool` and asserts the answer is a working task; returns its id.
    async fn start_task(&mut self, tool: &str, arguments: Value, progress: bool) -> String {
        let id = self.start_call(tool, arguments, progress).await;
        self.task_id_of(id).await
    }

    async fn task_id_of(&mut self, id: u64) -> String {
        let response = self.answer(id).await;
        let result = &response["result"];
        assert_eq!(result["resultType"], "task", "not a task: {response}");
        assert_eq!(result["status"], "working", "{response}");
        assert!(result["ttlMs"].is_null(), "the task expires: {response}");
        result["taskId"].as_str().expect("a task id").to_owned()
    }

    async fn get_task(&mut self, task: &str) -> Value {
        let id = self.request("tasks/get", json!({"taskId": task})).await;
        let response = self.answer(id).await;
        assert!(response["error"].is_null(), "tasks/get failed: {response}");
        response["result"].clone()
    }

    /// Polls `tasks/get` until the task is no longer working.
    async fn terminal(&mut self, task: &str) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let state = self.get_task(task).await;
            if state["status"] != "working" {
                return state;
            }
            assert!(Instant::now() < deadline, "task {task} never ended");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    fn progress(&self) -> Vec<f64> {
        self.inbox
            .iter()
            .filter(|message| message["method"] == "notifications/progress")
            .filter_map(|message| message["params"]["progress"].as_f64())
            .collect()
    }

    /// Ends the server's tasks, killing any child still running.
    fn end(&self) {
        self.server.end_tasks();
    }
}

fn text_of(result: &Value) -> String {
    result["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn scan(limit: u32) -> Value {
    json!({"operation": "pr_pipelines", "repo": "acme/web", "id": 7, "scan_limit": limit})
}

fn strictly_increasing(values: &[f64]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

#[tokio::test]
async fn task_long_call_answers_at_once_and_completes_with_the_child_stdout() {
    let stub = Stub::new();
    let mut session = Session::declaring(&stub.program).await;

    let started = Instant::now();
    let id = session.start_call("bitbucket_read", scan(1), false).await;
    let created = session.answer(id).await;
    assert!(started.elapsed() < Duration::from_millis(250), "{created}");
    assert_eq!(created["result"]["resultType"], "task", "{created}");
    assert_eq!(created["result"]["status"], "working", "{created}");

    let task = created["result"]["taskId"].as_str().expect("a task id");
    let state = session.terminal(task).await;
    assert_eq!(state["status"], "completed", "{state}");
    assert_ne!(state["result"]["isError"], json!(true), "{state}");
    assert_eq!(
        text_of(&state["result"]),
        "arg:--output\narg:json\narg:pr\narg:pipelines\narg:acme/web\narg:7\narg:--scan-limit=1\n"
    );
    session.end();
}

#[tokio::test]
async fn task_whose_child_fails_completes_with_an_error_result_carrying_stderr() {
    let stub = Stub::new();
    let mut session = Session::declaring(&stub.program).await;

    let task = session
        .start_task(
            "bitbucket_read",
            json!({"operation": "pr_pipelines", "repo": "acme/web", "id": 7, "scan_limit": 1,
                   "profile": "fail"}),
            false,
        )
        .await;
    let state = session.terminal(&task).await;

    assert_eq!(state["status"], "completed", "{state}");
    assert_eq!(state["result"]["isError"], json!(true), "{state}");
    assert_eq!(text_of(&state["result"]), "boom\n");
    session.end();
}

#[tokio::test]
async fn synchronous_short_call_with_the_extension_declared_returns_the_output_in_the_call() {
    let stub = Stub::new();
    let mut session = Session::declaring(&stub.program).await;

    let result = session
        .call(
            "bitbucket_read",
            json!({"operation": "pr_view", "repo": "acme/web", "id": 7}),
        )
        .await;

    assert_ne!(result["resultType"], "task", "{result}");
    assert_eq!(
        text_of(&result),
        "arg:--output\narg:json\narg:pr\narg:view\narg:acme/web\narg:7\n"
    );
    session.end();
}

#[tokio::test]
async fn synchronous_long_call_without_the_extension_returns_the_output_in_the_call() {
    let stub = Stub::new();
    let mut session = Session::plain(&stub.program).await;

    let result = session.call("bitbucket_read", scan(1)).await;

    assert_ne!(result["resultType"], "task", "{result}");
    assert!(
        text_of(&result).ends_with("arg:--scan-limit=1\n"),
        "{result}"
    );
    session.end();
}

#[tokio::test]
async fn synchronous_validation_errors_come_back_at_once_even_for_a_long_declared_call() {
    let stub = Stub::new();
    let mut session = Session::declaring(&stub.program).await;

    let calls = [
        ("bitbucket_read", json!({"operation": "nope"})),
        (
            "bitbucket_read",
            json!({"operation": "pr_pipelines", "repo": "acme/web"}),
        ),
        (
            "bitbucket_write",
            json!({"operation": "pipeline_trigger", "wait": true}),
        ),
    ];
    for (tool, arguments) in calls {
        let result = session.call(tool, arguments.clone()).await;
        assert_ne!(result["resultType"], "task", "{arguments}: {result}");
        assert_eq!(result["isError"], json!(true), "{arguments}: {result}");
    }
    session.end();
}

#[tokio::test]
async fn cancel_task_kills_the_child_and_the_task_ends_cancelled() {
    let stub = Stub::new();
    let mut session = Session::declaring(&stub.program).await;
    let task = session.start_task("bitbucket_read", scan(2), false).await;
    let pid = stub.pid().await;
    assert!(alive(pid));

    let id = session
        .request("tasks/cancel", json!({"taskId": task}))
        .await;
    let cancelled = session.answer(id).await;
    assert!(cancelled["error"].is_null(), "{cancelled}");

    let state = session.terminal(&task).await;
    assert_eq!(state["status"], "cancelled", "{state}");
    assert!(gone_within_five_seconds(pid).await, "child {pid} survived");

    let next = session
        .call(
            "bitbucket_read",
            json!({"operation": "pr_view", "repo": "acme/web", "id": 7}),
        )
        .await;
    assert_ne!(next["isError"], json!(true), "{next}");
    session.end();
}

#[tokio::test]
async fn cancel_of_a_synchronous_call_kills_the_child() {
    let stub = Stub::new();
    let mut session = Session::plain(&stub.program).await;
    let id = session.start_call("bitbucket_read", scan(2), false).await;
    let pid = stub.pid().await;
    assert!(alive(pid));

    session
        .send(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": id, "reason": "test"},
        }))
        .await;

    assert!(gone_within_five_seconds(pid).await, "child {pid} survived");
    let next = session
        .call(
            "bitbucket_read",
            json!({"operation": "pr_view", "repo": "acme/web", "id": 7}),
        )
        .await;
    assert_ne!(next["isError"], json!(true), "{next}");
    session.end();
}

#[tokio::test]
async fn cancel_of_an_unknown_task_is_an_error_and_the_server_keeps_answering() {
    let stub = Stub::new();
    let mut session = Session::declaring(&stub.program).await;

    let id = session
        .request("tasks/get", json!({"taskId": "no-such-task"}))
        .await;
    let response = session.answer(id).await;
    assert!(response["error"].is_object(), "{response}");

    let id = session
        .request("tasks/cancel", json!({"taskId": "no-such-task"}))
        .await;
    let response = session.answer(id).await;
    assert!(response["error"].is_object(), "{response}");

    let next = session
        .call(
            "bitbucket_read",
            json!({"operation": "pr_view", "repo": "acme/web", "id": 7}),
        )
        .await;
    assert_ne!(next["isError"], json!(true), "{next}");
    session.end();
}

#[tokio::test]
async fn heartbeat_task_sends_increasing_progress_and_sets_a_status_message() {
    let stub = Stub::new();
    let mut session = Session::declaring(&stub.program).await;
    let task = session.start_task("bitbucket_read", scan(3), true).await;

    let deadline = Instant::now() + WAIT;
    let message = loop {
        let state = session.get_task(&task).await;
        if let Some(message) = state["statusMessage"].as_str() {
            assert_eq!(state["status"], "working", "{state}");
            break message.to_owned();
        }
        assert!(Instant::now() < deadline, "no status message: {state}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    assert!(message.contains("running for"), "{message}");

    let state = session.terminal(&task).await;
    assert_eq!(state["status"], "completed", "{state}");
    session.listen(Duration::from_millis(100)).await;
    let progress = session.progress();
    assert!(progress.len() >= 2, "{progress:?}");
    assert!(strictly_increasing(&progress), "{progress:?}");
    session.end();
}

#[tokio::test]
async fn heartbeat_synchronous_call_sends_increasing_progress() {
    let stub = Stub::new();
    let mut session = Session::plain(&stub.program).await;

    let id = session.start_call("bitbucket_read", scan(3), true).await;
    let result = session.answer(id).await;

    assert_ne!(result["result"]["isError"], json!(true), "{result}");
    let progress = session.progress();
    assert!(progress.len() >= 2, "{progress:?}");
    assert!(strictly_increasing(&progress), "{progress:?}");
    session.end();
}

#[tokio::test]
async fn heartbeat_without_a_progress_token_sends_no_progress() {
    let stub = Stub::new();
    let mut declaring = Session::declaring(&stub.program).await;
    let task = declaring.start_task("bitbucket_read", scan(3), false).await;
    declaring.terminal(&task).await;
    declaring.listen(Duration::from_millis(100)).await;
    assert!(declaring.progress().is_empty());
    declaring.end();

    let mut plain = Session::plain(&stub.program).await;
    plain.call("bitbucket_read", scan(3)).await;
    plain.listen(Duration::from_millis(100)).await;
    assert!(plain.progress().is_empty());
    plain.end();
}

#[tokio::test]
async fn pipeline_wait_task_hands_the_wait_switch_to_the_child() {
    let stub = Stub::new();
    let mut session = Session::declaring(&stub.program).await;

    let task = session
        .start_task(
            "bitbucket_write",
            json!({"operation": "pipeline_trigger", "repo": "acme/web", "wait": true}),
            false,
        )
        .await;
    let state = session.terminal(&task).await;

    assert_eq!(state["status"], "completed", "{state}");
    assert!(
        text_of(&state["result"]).contains("arg:--wait\n"),
        "{state}"
    );
    session.end();
}
