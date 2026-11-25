use super::*;
use crate::cli::{Cli, Commands, pr::PrCommands};
use clap::Parser;
use serde_json::json;

fn call(operation: &str) -> Arguments {
    Arguments {
        operation: operation.to_owned(),
        ..Arguments::default()
    }
}

fn pr(operation: &str) -> Arguments {
    Arguments {
        repo: Some("acme/web".into()),
        id: Some(7),
        ..call(operation)
    }
}

fn read(arguments: &Arguments) -> Result<Vec<String>, String> {
    build_argv(ToolKind::Read, arguments)
}

fn write(arguments: &Arguments) -> Result<Vec<String>, String> {
    build_argv(ToolKind::Write, arguments)
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn parse(argv: Vec<String>) -> Cli {
    let mut full = vec!["bitbucket".to_owned()];
    full.extend(argv);
    match Cli::try_parse_from(&full) {
        Ok(cli) => cli,
        Err(e) => panic!("{full:?} rejected by the CLI: {e}"),
    }
}

#[test]
fn argv_pr_view_is_json_output_then_subcommand_then_positionals() {
    assert_eq!(
        read(&pr("pr_view")).unwrap(),
        strings(&["--output", "json", "pr", "view", "acme/web", "7"])
    );
}

#[test]
fn argv_profile_is_forwarded_as_global_flag() {
    let arguments = Arguments {
        profile: Some("base".into()),
        ..pr("pr_view")
    };
    assert_eq!(
        read(&arguments).unwrap(),
        strings(&[
            "--output",
            "json",
            "--profile=base",
            "pr",
            "view",
            "acme/web",
            "7"
        ])
    );
}

#[test]
fn argv_absent_workspace_adds_no_workspace_flag() {
    let argv = read(&pr("pr_view")).unwrap();
    assert!(
        !argv.iter().any(|a| a.starts_with("--workspace")),
        "{argv:?}"
    );
    let arguments = Arguments {
        workspace: Some("acme".into()),
        ..pr("pr_view")
    };
    assert!(
        read(&arguments)
            .unwrap()
            .contains(&"--workspace=acme".to_owned())
    );
}

#[test]
fn argv_pr_list_repeats_state_flag_per_state() {
    let arguments = Arguments {
        repo: Some("acme/web".into()),
        state: vec!["open".into(), "merged".into()],
        ..call("pr_list")
    };
    assert_eq!(
        read(&arguments).unwrap(),
        strings(&[
            "--output",
            "json",
            "pr",
            "list",
            "acme/web",
            "--state=open",
            "--state=merged"
        ])
    );
}

#[test]
fn argv_pr_merge_without_strategy_omits_strategy_flag() {
    let argv = write(&pr("pr_merge")).unwrap();
    assert!(
        !argv.iter().any(|a| a.starts_with("--strategy")),
        "{argv:?}"
    );
    let arguments = Arguments {
        strategy: Some("squash".into()),
        ..pr("pr_merge")
    };
    assert!(
        write(&arguments)
            .unwrap()
            .contains(&"--strategy=squash".to_owned())
    );
}

#[test]
fn argv_every_operation_with_its_required_fields_parses_in_the_cli() {
    for op in READ_OPERATIONS {
        let arguments = Arguments {
            comment_id: Some(3),
            build: Some(4),
            ..pr(op)
        };
        parse(read(&arguments).unwrap());
    }
    for op in WRITE_OPERATIONS {
        let arguments = Arguments {
            build: Some(4),
            title: Some("t".into()),
            source: Some("feature".into()),
            body: Some("b".into()),
            ..pr(op)
        };
        parse(write(&arguments).unwrap());
    }
}

#[test]
fn argv_optional_fields_parse_in_the_cli() {
    let list = Arguments {
        repo: Some("a/b".into()),
        state: vec!["open".into()],
        query: Some("title ~ \"x\"".into()),
        sort: Some("-created_on".into()),
        page: Some(2),
        limit: Some(5),
        ..call("pr_list")
    };
    parse(read(&list).unwrap());
    let trigger = Arguments {
        repo: Some("a/b".into()),
        branch: Some("main".into()),
        commit: Some("abc".into()),
        pipeline: Some("deploy".into()),
        var: vec!["A=1".into()],
        secured_var: vec!["B=2".into()],
        ..call("pipeline_trigger")
    };
    parse(write(&trigger).unwrap());
    let comment = Arguments {
        body: Some("hi".into()),
        path: Some("src/a.rs".into()),
        line: Some(3),
        parent: Some(9),
        ..pr("pr_comment")
    };
    parse(write(&comment).unwrap());
    let stop = Arguments {
        repo: Some("a/b".into()),
        uuid: Some("{u}".into()),
        ..call("pipeline_stop")
    };
    parse(write(&stop).unwrap());
    let repos = Arguments {
        role: Some("admin".into()),
        ..call("repo_list")
    };
    parse(read(&repos).unwrap());
}

#[test]
fn hostile_text_values_reach_the_child_as_the_value_of_their_own_flag() {
    let comment = Arguments {
        body: Some("--delete".into()),
        ..pr("pr_comment")
    };
    let argv = write(&comment).unwrap();
    assert!(argv.contains(&"--body=--delete".to_owned()), "{argv:?}");
    match parse(argv).command {
        Commands::Pr {
            command: PrCommands::Comment { body, .. },
        } => assert_eq!(body, "--delete"),
        _ => panic!("not parsed as pr comment"),
    }

    let create = Arguments {
        repo: Some("acme/web".into()),
        title: Some("--delete".into()),
        source: Some("-x".into()),
        ..call("pr_create")
    };
    let argv = write(&create).unwrap();
    assert!(argv.contains(&"--title=--delete".to_owned()), "{argv:?}");
    assert!(argv.contains(&"--source=-x".to_owned()), "{argv:?}");
    match parse(argv).command {
        Commands::Pr {
            command: PrCommands::Create { title, source, .. },
        } => {
            assert_eq!(title, "--delete");
            assert_eq!(source, "-x");
        }
        _ => panic!("not parsed as pr create"),
    }
}

#[test]
fn hostile_repo_is_rejected_naming_the_field() {
    for repo in [
        "../x",
        "acme",
        "acme/web/extra",
        "-x/y",
        "x/-y",
        "./y",
        "a b/c",
        "",
        "a/..",
    ] {
        let arguments = Arguments {
            repo: Some(repo.into()),
            ..pr("pr_view")
        };
        let error = read(&arguments).unwrap_err();
        assert!(error.contains("repo"), "{repo:?} gave {error}");
    }
}

#[test]
fn hostile_missing_required_field_is_rejected_naming_the_field() {
    let no_id = Arguments {
        repo: Some("acme/web".into()),
        ..call("pr_view")
    };
    assert!(read(&no_id).unwrap_err().contains("id"));
    assert!(read(&call("pr_view")).unwrap_err().contains("repo"));
    assert!(write(&pr("pr_comment")).unwrap_err().contains("body"));
    assert!(write(&pr("pr_create")).unwrap_err().contains("title"));
    let stop = Arguments {
        repo: Some("a/b".into()),
        ..call("pipeline_stop")
    };
    assert!(write(&stop).unwrap_err().contains("build"));
}

#[test]
fn hostile_unknown_or_wrong_tool_operation_is_rejected_naming_the_field() {
    assert!(read(&call("auth_login")).unwrap_err().contains("operation"));
    assert!(read(&pr("pr_merge")).unwrap_err().contains("operation"));
    assert!(write(&pr("pr_view")).unwrap_err().contains("operation"));
}

#[test]
fn hostile_nul_is_rejected_and_newline_stays_one_argument() {
    let nul = Arguments {
        body: Some("a\0b".into()),
        ..pr("pr_comment")
    };
    assert!(write(&nul).unwrap_err().contains("body"));
    let profile = Arguments {
        profile: Some("a\0b".into()),
        ..pr("pr_view")
    };
    assert!(read(&profile).unwrap_err().contains("profile"));
    let newline = Arguments {
        body: Some("line1\nline2".into()),
        ..pr("pr_comment")
    };
    let argv = write(&newline).unwrap();
    assert!(argv.contains(&"--body=line1\nline2".to_owned()), "{argv:?}");
}

fn pipeline_commands(argv: Vec<String>) -> crate::cli::pipeline::PipelineCommands {
    match parse(argv).command {
        Commands::Pipeline { command } => command,
        _ => panic!("not parsed as a pipeline command"),
    }
}

#[test]
fn wait_on_pipeline_trigger_adds_the_switch_and_parses_in_the_cli() {
    use crate::cli::pipeline::PipelineCommands;
    let arguments = Arguments {
        repo: Some("a/b".into()),
        wait: true,
        ..call("pipeline_trigger")
    };
    let argv = write(&arguments).unwrap();
    assert!(argv.contains(&"--wait".to_owned()), "{argv:?}");
    match pipeline_commands(argv) {
        PipelineCommands::Trigger { wait, .. } => assert!(wait),
        _ => panic!("not parsed as pipeline trigger"),
    }
}

#[test]
fn wait_on_pipeline_rerun_adds_the_switch_and_parses_in_the_cli() {
    use crate::cli::pipeline::PipelineCommands;
    let arguments = Arguments {
        repo: Some("a/b".into()),
        build: Some(4),
        wait: true,
        ..call("pipeline_rerun")
    };
    let argv = write(&arguments).unwrap();
    assert!(argv.contains(&"--wait".to_owned()), "{argv:?}");
    match pipeline_commands(argv) {
        PipelineCommands::Rerun { wait, .. } => assert!(wait),
        _ => panic!("not parsed as pipeline rerun"),
    }
}

#[test]
fn wait_false_or_absent_adds_nothing_and_is_ignored_by_other_operations() {
    let trigger = Arguments {
        repo: Some("a/b".into()),
        ..call("pipeline_trigger")
    };
    assert!(!write(&trigger).unwrap().contains(&"--wait".to_owned()));
    let rerun = Arguments {
        repo: Some("a/b".into()),
        build: Some(4),
        ..call("pipeline_rerun")
    };
    assert!(!write(&rerun).unwrap().contains(&"--wait".to_owned()));
    let view = Arguments {
        wait: true,
        ..pr("pr_view")
    };
    assert_eq!(read(&view).unwrap(), read(&pr("pr_view")).unwrap());
}

fn limited(operation: &str, limit: Option<u32>) -> Arguments {
    Arguments {
        limit,
        ..pr(operation)
    }
}

#[test]
fn long_call_pipeline_wait_is_long_only_when_waiting() {
    let trigger = |wait| Arguments {
        wait,
        ..call("pipeline_trigger")
    };
    assert!(is_long_call(ToolKind::Write, &trigger(true)));
    assert!(!is_long_call(ToolKind::Write, &trigger(false)));
    let rerun = Arguments {
        wait: true,
        ..call("pipeline_rerun")
    };
    assert!(is_long_call(ToolKind::Write, &rerun));
}

#[test]
fn long_call_pipeline_scans_and_logs_are_long() {
    assert!(is_long_call(ToolKind::Read, &pr("pr_pipelines")));
    let view = |logs, full_logs| Arguments {
        logs,
        full_logs,
        ..call("pipeline_view")
    };
    assert!(is_long_call(ToolKind::Read, &view(true, false)));
    assert!(is_long_call(ToolKind::Read, &view(false, true)));
    assert!(!is_long_call(ToolKind::Read, &view(false, false)));
}

#[test]
fn long_call_listing_is_long_only_above_the_default_page() {
    assert!(!is_long_call(ToolKind::Read, &limited("pr_list", Some(25))));
    assert!(is_long_call(ToolKind::Read, &limited("pr_list", Some(26))));
    assert!(!is_long_call(ToolKind::Read, &limited("pr_list", None)));
    assert!(is_long_call(
        ToolKind::Read,
        &limited("repo_list", Some(100))
    ));
    for operation in [
        "pipeline_list",
        "pr_diffstat",
        "pr_commits",
        "pr_activity",
        "pr_list_comments",
    ] {
        assert!(is_long_call(ToolKind::Read, &limited(operation, Some(26))));
        assert!(!is_long_call(ToolKind::Read, &limited(operation, Some(25))));
    }
}

#[test]
fn long_call_short_operations_are_never_long() {
    assert!(!is_long_call(
        ToolKind::Read,
        &limited("pr_view", Some(500))
    ));
    assert!(!is_long_call(
        ToolKind::Write,
        &limited("pr_merge", Some(500))
    ));
    assert!(!is_long_call(
        ToolKind::Write,
        &limited("pipeline_stop", Some(500))
    ));
    assert!(!is_long_call(ToolKind::Read, &call("no_such_operation")));
}

fn published(tool: ToolKind) -> serde_json::Map<String, Value> {
    let schema = schema::input_schema(tool);
    match schema.get("properties") {
        Some(Value::Object(properties)) => properties.clone(),
        other => panic!("no properties object: {other:?}"),
    }
}

fn names(properties: &serde_json::Map<String, Value>) -> std::collections::BTreeSet<String> {
    properties.keys().cloned().collect()
}

fn fields_of(operations: &[&str]) -> std::collections::BTreeSet<String> {
    let mut set: std::collections::BTreeSet<String> = ["operation", "profile", "workspace"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    for (name, fields) in OPERATIONS.iter() {
        if operations.contains(name) {
            set.extend(fields.iter().filter_map(|f| f.name).map(str::to_owned));
        }
    }
    set
}

#[test]
fn schema_each_tool_publishes_exactly_the_fields_of_its_operations() {
    assert_eq!(
        names(&published(ToolKind::Read)),
        fields_of(READ_OPERATIONS)
    );
    assert_eq!(
        names(&published(ToolKind::Write)),
        fields_of(WRITE_OPERATIONS)
    );
}

#[test]
fn schema_every_operation_has_a_table_entry() {
    for name in READ_OPERATIONS.iter().chain(WRITE_OPERATIONS) {
        assert!(OPERATIONS.iter().any(|(op, _)| op == name), "{name}");
    }
}

#[test]
fn schema_fields_are_split_between_the_tools() {
    let read = names(&published(ToolKind::Read));
    let write = names(&published(ToolKind::Write));
    for field in ["state", "logs", "scan_limit", "build"] {
        assert!(read.contains(field), "read lacks {field}");
    }
    for field in ["title", "body", "wait"] {
        assert!(!read.contains(field), "read has {field}");
    }
    for field in ["title", "var", "wait", "uuid", "build"] {
        assert!(write.contains(field), "write lacks {field}");
    }
    for field in ["state", "logs"] {
        assert!(!write.contains(field), "write has {field}");
    }
}

#[test]
fn schema_types_are_compact() {
    for tool in [ToolKind::Read, ToolKind::Write] {
        let schema = schema::input_schema(tool);
        assert!(schema.get("$schema").is_none());
        assert_eq!(schema["additionalProperties"], json!(false));
        for (name, property) in published(tool) {
            for banned in ["format", "minimum", "default", "title"] {
                assert!(property.get(banned).is_none(), "{name} has {banned}");
            }
            assert!(property["type"].is_string(), "{name}: {property}");
        }
    }
    let write = published(ToolKind::Write);
    assert_eq!(write["title"], json!({"type": "string"}));
    assert_eq!(write["uuid"], json!({"type": "string"}));
    assert_eq!(write["wait"]["type"], "boolean");
    assert_eq!(write["line"], json!({"type": "integer"}));
    assert_eq!(write["var"]["type"], "array");
    assert_eq!(write["var"]["items"], json!({"type": "string"}));
}

#[test]
fn a_field_the_tool_does_not_publish_is_still_accepted_and_ignored() {
    let plain = pr("pr_view");
    let with_title = Arguments {
        title: Some("ignored".to_owned()),
        ..pr("pr_view")
    };
    assert_eq!(read(&with_title), read(&plain));
    let unknown: Result<Arguments, _> =
        serde_json::from_value(json!({"operation": "pr_view", "nonsense": 1}));
    assert!(unknown.is_err(), "an unknown field must be rejected");
}
