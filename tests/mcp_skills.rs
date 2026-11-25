//! Drives `bitbucket mcp` as a client of the skills extension: the bitbucket skill is
//! discoverable on both protocol revisions, listed, readable as a resource, and nothing
//! outside it is served.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use rmcp::ServiceExt;
use rmcp::model::{
    CacheScope, ClientCapabilities, ClientConfig, GetSkillRequestParams, Implementation,
    ProtocolVersion, ReadResourceRequestParams, SKILLS_EXTENSION_ID, ServerCapabilities,
};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::{Value, json};

const SKILL_URI: &str = "skill://bitbucket/SKILL.md";
const SKILL_MD: &str = include_str!("../plugins/bitbucket/skills/bitbucket/SKILL.md");
const INVALID_PARAMS: i64 = -32602;

fn transport(home: &std::path::Path) -> TokioChildProcess {
    let cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_bitbucket"));
    TokioChildProcess::new(cmd.configure(|c| {
        c.arg("mcp")
            .env_clear()
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .current_dir(home);
    }))
    .expect("spawn bitbucket mcp")
}

/// A line-delimited JSON-RPC session that shows the exact wire answers.
struct Raw {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Raw {
    fn start(home: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_bitbucket"))
            .arg("mcp")
            .env_clear()
            .env("HOME", home)
            .current_dir(home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the server starts");
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let mut raw = Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        let answer = raw.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "raw", "version": "0"},
            }),
        );
        assert!(answer["result"].is_object(), "{answer}");
        raw.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        raw
    }

    fn send(&mut self, message: &Value) {
        writeln!(self.stdin, "{message}").expect("the server reads stdin");
        self.stdin.flush().expect("stdin flushes");
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let mut line = String::new();
            let read = self.stdout.read_line(&mut line).expect("stdout reads");
            assert!(read > 0, "the server closed before answering {method}");
            let message: Value = serde_json::from_str(line.trim_end()).expect("a JSON line");
            if message["id"] == json!(id) {
                return message;
            }
        }
    }

    fn result(&mut self, method: &str, params: Value) -> Value {
        let answer = self.request(method, params);
        assert!(answer["result"].is_object(), "{method} failed: {answer}");
        answer["result"].clone()
    }

    fn error_code(&mut self, method: &str, params: Value) -> i64 {
        let answer = self.request(method, params.clone());
        answer["error"]["code"]
            .as_i64()
            .unwrap_or_else(|| panic!("{method} {params} did not fail: {answer}"))
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn frontmatter_field(key: &str) -> String {
    SKILL_MD
        .lines()
        .skip(1)
        .take_while(|line| *line != "---")
        .find_map(|line| line.strip_prefix(&format!("{key}: ")))
        .unwrap_or_else(|| panic!("SKILL.md has no {key}"))
        .to_owned()
}

fn assert_skill_discovery(capabilities: &ServerCapabilities, instructions: Option<&str>) {
    assert!(capabilities.resources.is_some(), "{capabilities:?}");
    assert!(capabilities.tools.is_some(), "{capabilities:?}");
    assert!(
        capabilities
            .extensions
            .as_ref()
            .is_some_and(|extensions| extensions.contains_key(SKILLS_EXTENSION_ID)),
        "no skills extension: {capabilities:?}"
    );
    let instructions = instructions.expect("the server carries instructions");
    assert!(instructions.contains(SKILL_URI), "{instructions}");
}

#[tokio::test]
async fn instructions_and_the_skills_extension_are_declared_on_both_revisions() {
    let home = tempfile::tempdir().unwrap();

    let client = ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("mcp-skills-test", "0"),
    )
    .with_protocol_version(ProtocolVersion::V_2025_11_25)
    .serve(transport(home.path()))
    .await
    .expect("a 2025-11-25 client completes initialize");
    let info = client.peer_info().expect("initialize recorded the server");
    assert_skill_discovery(&info.capabilities, info.instructions.as_deref());
    client.cancel().await.unwrap();

    let client = ()
        .serve_with_lifecycle(
            transport(home.path()),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("a 2026-07-28 client completes server/discover");
    let info = client.peer_info().expect("discover recorded the server");
    assert_skill_discovery(&info.capabilities, info.instructions.as_deref());
    let tools = client.list_all_tools().await.expect("tools/list answers");
    let mut names: Vec<&str> = tools.iter().map(|tool| &*tool.name).collect();
    names.sort_unstable();
    assert_eq!(names, ["bitbucket_read", "bitbucket_write"]);
    client.cancel().await.unwrap();
}

#[test]
fn the_skill_is_listed_and_read_as_one_resource() {
    let home = tempfile::tempdir().unwrap();
    let mut raw = Raw::start(home.path());

    let listing = raw.result("resources/list", json!({}));
    let resources = listing["resources"].as_array().expect("a resource list");
    assert_eq!(resources.len(), 1, "{listing}");
    assert_eq!(resources[0]["uri"], SKILL_URI);
    assert_eq!(resources[0]["mimeType"], "text/markdown");
    assert_eq!(resources[0]["name"], frontmatter_field("name"));
    assert_eq!(
        resources[0]["description"],
        frontmatter_field("description")
    );

    let read = raw.result("resources/read", json!({"uri": SKILL_URI}));
    assert_eq!(read["contents"][0]["uri"], SKILL_URI);
    assert_eq!(read["contents"][0]["text"], SKILL_MD);
}

#[test]
fn the_skill_is_listed_and_fetched_through_the_skills_extension() {
    let home = tempfile::tempdir().unwrap();
    let mut raw = Raw::start(home.path());

    let listed = raw.result("skills/list", json!({}));
    let skills = listed["skills"].as_array().expect("a skill list");
    assert_eq!(skills.len(), 1, "{listed}");
    assert_eq!(skills[0]["uri"], SKILL_URI);
    assert_eq!(skills[0]["frontmatter"]["name"], "bitbucket");

    let fetched = raw.result("skills/get", json!({"uri": SKILL_URI}));
    assert_eq!(fetched["skill"]["uri"], SKILL_URI, "{fetched}");
    assert_eq!(
        fetched["skill"]["frontmatter"]["name"], "bitbucket",
        "{fetched}"
    );
}

#[test]
fn a_skill_uri_outside_the_catalog_is_invalid_params_and_the_server_keeps_answering() {
    let home = tempfile::tempdir().unwrap();
    let mut raw = Raw::start(home.path());

    for uri in [
        "skill://bitbucket/../SKILL.md",
        "skill://bitbucket/other.md",
        "file:///etc/hosts",
        "skill://bitbucket/",
    ] {
        assert_eq!(
            raw.error_code("resources/read", json!({"uri": uri})),
            INVALID_PARAMS,
            "resources/read {uri}"
        );
        assert_eq!(
            raw.error_code("skills/get", json!({"uri": uri})),
            INVALID_PARAMS,
            "skills/get {uri}"
        );
    }
    for params in [json!({}), json!({"uri": 7})] {
        assert_eq!(
            raw.error_code("skills/get", params.clone()),
            INVALID_PARAMS,
            "skills/get {params}"
        );
    }
    let read = raw.result("resources/read", json!({"uri": SKILL_URI}));
    assert_eq!(read["contents"][0]["text"], SKILL_MD);
}

#[tokio::test]
async fn skill_answers_carry_cache_hints_on_the_2026_revision() {
    let home = tempfile::tempdir().unwrap();
    let client = ()
        .serve_with_lifecycle(
            transport(home.path()),
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .expect("a 2026-07-28 client completes server/discover");

    let listed = client.list_resources(None).await.expect("resources/list");
    let read = client
        .read_resource(ReadResourceRequestParams::new(SKILL_URI))
        .await
        .expect("resources/read");
    let skills = client.list_skills(None).await.expect("skills/list");
    let skill = client
        .get_skill(GetSkillRequestParams::new(SKILL_URI))
        .await
        .expect("skills/get");
    for (method, ttl_ms, scope) in [
        ("resources/list", listed.ttl_ms, listed.cache_scope),
        ("resources/read", read.ttl_ms, read.cache_scope),
        ("skills/list", skills.ttl_ms, skills.cache_scope),
        ("skills/get", skill.ttl_ms, skill.cache_scope),
    ] {
        assert_eq!(ttl_ms, Some(0), "{method}");
        assert_eq!(scope, Some(CacheScope::Public), "{method}");
    }
    client.cancel().await.unwrap();
}
