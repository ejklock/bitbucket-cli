use std::collections::BTreeSet;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, Tool};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

type Client = RunningService<RoleClient, ()>;

const READ_OPERATIONS: &[&str] = &[
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

const WRITE_OPERATIONS: &[&str] = &[
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

/// Spawn the built binary as `bitbucket mcp` with an empty home, so no stored
/// profile, config or keyring entry of the machine is reachable.
async fn connect(home: &std::path::Path) -> Client {
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_bitbucket"));
    let transport = TokioChildProcess::new(cmd.configure(|c| {
        c.arg("mcp")
            .env_clear()
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .current_dir(home);
    }))
    .expect("spawn bitbucket mcp");
    ().serve(transport).await.expect("initialize")
}

fn object(value: Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("expected a JSON object, got {other}"),
    }
}

async fn call(client: &Client, tool: &'static str, args: Value) -> CallToolResult {
    client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(object(args)))
        .await
        .expect("tool call answered")
}

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|t| t.text.clone()))
        .collect()
}

fn operations_of(tool: &Tool) -> BTreeSet<String> {
    let schema = Value::Object((*tool.input_schema).clone());
    schema["properties"]["operation"]["enum"]
        .as_array()
        .unwrap_or_else(|| panic!("no operation enum in {schema}"))
        .iter()
        .map(|v| v.as_str().expect("operation string").to_owned())
        .collect()
}

#[test]
fn lists_the_mcp_subcommand_in_the_top_level_help() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_bitbucket"))
        .arg("--help")
        .output()
        .expect("run bitbucket --help");
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(
        help.lines().any(|l| l.trim_start().starts_with("mcp ")),
        "no mcp subcommand in:\n{help}"
    );
}

#[tokio::test]
async fn lists_exactly_the_read_and_write_tools_with_their_annotations_and_operations() {
    let home = tempfile::tempdir().unwrap();
    let client = connect(home.path()).await;
    let tools = client.list_all_tools().await.unwrap();

    let names: BTreeSet<&str> = tools.iter().map(|t| &*t.name).collect();
    assert_eq!(names, BTreeSet::from(["bitbucket_read", "bitbucket_write"]));

    let read = tools.iter().find(|t| t.name == "bitbucket_read").unwrap();
    let write = tools.iter().find(|t| t.name == "bitbucket_write").unwrap();
    let read_annotations = read.annotations.as_ref().expect("read annotations");
    assert_eq!(read_annotations.read_only_hint, Some(true));
    let write_annotations = write.annotations.as_ref().expect("write annotations");
    assert_eq!(write_annotations.destructive_hint, Some(true));
    assert_eq!(write_annotations.read_only_hint, Some(false));

    for tool in [read, write] {
        let schema = Value::Object((*tool.input_schema).clone());
        assert_eq!(schema["type"], "object", "{}", tool.name);
        assert!(schema.get("oneOf").is_none(), "{} has oneOf", tool.name);
        assert!(schema.get("anyOf").is_none(), "{} has anyOf", tool.name);
        assert_eq!(schema["required"], json!(["operation"]), "{}", tool.name);
        assert_eq!(schema["properties"]["operation"]["type"], "string");
    }
    let expected = |ops: &[&str]| ops.iter().map(|s| (*s).to_owned()).collect::<BTreeSet<_>>();
    assert_eq!(operations_of(read), expected(READ_OPERATIONS));
    assert_eq!(operations_of(write), expected(WRITE_OPERATIONS));
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn child_failure_is_an_error_result_with_stderr_and_the_server_keeps_answering() {
    let home = tempfile::tempdir().unwrap();
    let client = connect(home.path()).await;

    let rejected = call(
        &client,
        "bitbucket_write",
        json!({"operation": "pr_merge", "repo": "acme/web", "id": 7, "strategy": "bogus"}),
    )
    .await;
    assert_eq!(rejected.is_error, Some(true), "{}", text_of(&rejected));
    assert!(
        text_of(&rejected).contains("bogus"),
        "{}",
        text_of(&rejected)
    );

    let invalid = call(
        &client,
        "bitbucket_read",
        json!({"operation": "pr_view", "repo": "acme/web"}),
    )
    .await;
    assert_eq!(invalid.is_error, Some(true));
    assert!(text_of(&invalid).contains("id"), "{}", text_of(&invalid));

    let again = call(
        &client,
        "bitbucket_write",
        json!({"operation": "pr_merge", "repo": "acme/web", "id": 7, "strategy": "nope"}),
    )
    .await;
    assert_eq!(again.is_error, Some(true));
    assert!(text_of(&again).contains("nope"), "{}", text_of(&again));
    client.cancel().await.unwrap();
}

#[cfg(unix)]
mod stub_child {
    use super::*;
    use bitbucket_cli::mcp::BitbucketServer;
    use std::os::unix::fs::PermissionsExt;

    /// A stand-in for the bitbucket binary: echoes its arguments and the byte
    /// count of its stdin, fails with a message when an argument says `fail`.
    fn stub_program(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("stub.sh");
        std::fs::write(
            &path,
            "#!/bin/sh\n\
             case \"$*\" in *fail*) echo boom >&2; exit 3;; esac\n\
             case \"$*\" in *large*) head -c 1048576 /dev/zero | tr '\\0' a; printf '\\377'; exit 0;; esac\n\
             printf 'stdin:%s\\n' \"$(cat | wc -c | tr -d ' ')\"\n\
             printf 'arg:%s\\n' \"$@\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    async fn connect_stub(program: std::path::PathBuf) -> Client {
        let (server_io, client_io) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            let running = BitbucketServer::new(program)
                .serve(server_io)
                .await
                .expect("serve");
            let _ = running.waiting().await;
        });
        ().serve(client_io).await.expect("initialize")
    }

    const PROBE_ENV: &str = "BITBUCKET_MCP_STDIN_PROBE";

    /// Runs the stub-backed server and checks the child sees an empty stdin.
    /// It only does work inside the process spawned by the test below.
    #[tokio::test]
    async fn stdin_probe_child_sees_no_input() {
        if std::env::var_os(PROBE_ENV).is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let client = connect_stub(stub_program(dir.path())).await;
        let result = call(
            &client,
            "bitbucket_read",
            json!({"operation": "repo_view", "repo": "acme/web"}),
        )
        .await;
        assert_eq!(
            text_of(&result),
            "stdin:0\narg:--output\narg:json\narg:repo\narg:view\narg:acme/web\n"
        );
    }

    #[tokio::test]
    async fn child_stdin_is_empty_even_when_the_server_process_has_piped_input() {
        use std::io::Write;
        let mut probe = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "stub_child::stdin_probe_child_sees_no_input"])
            .env(PROBE_ENV, "1")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn probe process");
        let mut input = probe.stdin.take().unwrap();
        input.write_all(b"piped bytes").unwrap();
        drop(input);
        let output = probe.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }

    #[tokio::test]
    async fn child_stdout_is_the_text_result() {
        let dir = tempfile::tempdir().unwrap();
        let client = connect_stub(stub_program(dir.path())).await;
        let result = call(
            &client,
            "bitbucket_read",
            json!({"operation": "pr_view", "repo": "acme/web", "id": 7, "profile": "base"}),
        )
        .await;
        assert_ne!(result.is_error, Some(true), "{}", text_of(&result));
        assert_eq!(
            text_of(&result),
            "stdin:0\narg:--output\narg:json\narg:--profile=base\narg:pr\narg:view\narg:acme/web\narg:7\n"
        );
    }

    #[tokio::test]
    async fn large_non_utf8_child_output_is_returned_lossily_in_full() {
        let dir = tempfile::tempdir().unwrap();
        let client = connect_stub(stub_program(dir.path())).await;
        let result = call(
            &client,
            "bitbucket_read",
            json!({"operation": "pr_view", "repo": "acme/web", "id": 7, "profile": "large"}),
        )
        .await;
        assert_ne!(result.is_error, Some(true), "{}", text_of(&result));
        let text = text_of(&result);
        assert_eq!(text.len(), 1024 * 1024 + '\u{FFFD}'.len_utf8());
        assert!(text.ends_with('\u{FFFD}'));
    }

    #[tokio::test]
    async fn child_nonzero_exit_is_an_error_with_stderr_and_the_next_call_still_works() {
        let dir = tempfile::tempdir().unwrap();
        let client = connect_stub(stub_program(dir.path())).await;
        let failed = call(
            &client,
            "bitbucket_write",
            json!({"operation": "pr_comment", "repo": "acme/web", "id": 7, "body": "fail"}),
        )
        .await;
        assert_eq!(failed.is_error, Some(true));
        assert_eq!(text_of(&failed), "boom\n");
        let next = call(
            &client,
            "bitbucket_read",
            json!({"operation": "repo_view", "repo": "acme/web"}),
        )
        .await;
        assert_ne!(next.is_error, Some(true), "{}", text_of(&next));
        assert!(text_of(&next).ends_with("arg:repo\narg:view\narg:acme/web\n"));
    }

    #[tokio::test]
    async fn child_that_cannot_start_is_an_error_result() {
        let client = connect_stub("/nonexistent/bitbucket".into()).await;
        let result = call(
            &client,
            "bitbucket_read",
            json!({"operation": "repo_view", "repo": "acme/web"}),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert!(
            text_of(&result).contains("could not run"),
            "{}",
            text_of(&result)
        );
    }
}

#[cfg(unix)]
#[test]
fn help_runs_within_a_one_mebibyte_main_stack() {
    let output = std::process::Command::new("sh")
        .args([
            "-c",
            "ulimit -s 1024 && exec \"$0\" --help",
            env!("CARGO_BIN_EXE_bitbucket"),
        ])
        .output()
        .expect("run the binary under a 1 MiB stack");
    assert!(
        output.status.success(),
        "status {:?}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("mcp"));
}

#[tokio::test]
async fn tools_list_is_compact_and_points_to_the_skill() {
    let home = tempfile::tempdir().unwrap();
    let client = connect(home.path()).await;
    let tools = client.list_all_tools().await.unwrap();
    let size = serde_json::to_string(&tools).unwrap().len();
    assert!(size <= 4000, "tools array is {size} bytes, over 4000");
    for tool in &tools {
        let description = tool.description.as_deref().unwrap_or_default();
        assert!(
            description.chars().count() <= 160,
            "{}: {description}",
            tool.name
        );
        assert!(
            description.contains("skill://bitbucket/SKILL.md"),
            "{}: {description}",
            tool.name
        );
    }
    client.cancel().await.unwrap();
}
