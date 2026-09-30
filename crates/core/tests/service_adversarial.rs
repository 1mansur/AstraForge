use astraforge_core::patch::PatchProposal;
use astraforge_core::service::{Engine, Request, RequestEnvelope};
use rusqlite::params;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
fn repository(root: &Path) {
    std::fs::create_dir(root).unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(root)
        .status()
        .unwrap()
        .success());
    std::fs::write(root.join("main.rs"), "fn original() {}\n").unwrap();
}
fn envelope(repository_id: Option<&str>, request: Value) -> RequestEnvelope {
    serde_json::from_value(json!({"version":1,"repositoryId":repository_id,"request":request}))
        .unwrap()
}
fn open(engine: &Arc<Engine>, root: &Path) -> Value {
    engine
        .handle_envelope(envelope(
            None,
            json!({"method":"open_repository","params":{"path":root}}),
        ))
        .unwrap()
}
#[test]
fn desktop_contract_requires_version_and_matching_repository_before_mutation() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    repository(&first);
    repository(&second);
    let engine = Arc::new(Engine::new(&temp.path().join("state.db"), Arc::new(|_, _| {})).unwrap());
    let initial = open(&engine, &first);
    let stale_id = initial["id"].as_str().unwrap();
    let current = open(&engine, &second);
    let request =
        json!({"method":"create_file","params":{"path":"must-not-exist.txt","directory":false}});
    let stale = engine
        .handle_envelope(envelope(Some(stale_id), request.clone()))
        .unwrap_err();
    assert_eq!(stale.code, "stale_repository");
    assert_eq!(
        engine
            .handle_envelope(envelope(None, request.clone()))
            .unwrap_err()
            .code,
        "protocol_repository"
    );
    let mut incompatible = envelope(current["id"].as_str(), request.clone());
    incompatible.version = 99;
    assert_eq!(
        engine.handle_envelope(incompatible).unwrap_err().code,
        "protocol_version"
    );
    assert!(!first.join("must-not-exist.txt").exists());
    assert!(!second.join("must-not-exist.txt").exists());
    engine
        .handle_envelope(envelope(current["id"].as_str(), request))
        .unwrap();
    assert!(second.join("must-not-exist.txt").exists());
    let fixtures: Value =
        serde_json::from_str(include_str!("../../../fixtures/protocol-v1.json")).unwrap();
    for name in ["scopedRequest", "globalRequest"] {
        assert!(serde_json::from_value::<RequestEnvelope>(fixtures[name].clone()).is_ok());
    }
    assert!(serde_json::from_value::<RequestEnvelope>(json!({"version":1,"repositoryId":null,"admin":true,"request":{"method":"repositories","params":{}}})).is_err());
}
#[test]
fn recovery_conflict_allows_inspection_and_blocks_mutations_until_resolved() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repository");
    repository(&root);
    let engine = Arc::new(Engine::new(&temp.path().join("state.db"), Arc::new(|_, _| {})).unwrap());
    open(&engine, &root);
    let workspace = engine.workspace().unwrap();
    let before = workspace.read("main.rs").unwrap();
    let patch = engine
        .patches
        .propose(
            &workspace,
            vec![PatchProposal {
                path: "main.rs".into(),
                content: Some("fn patched() {}\n".into()),
                expected_hash: Some(before.hash),
            }],
            "fixture",
        )
        .unwrap();
    let journal = json!([{"path":"main.rs","before":before.content,"after":"fn patched() {}\n"}]);
    engine.db.with(|connection| {
        connection.execute("INSERT INTO patch_journal(patch_id,repository_id,prior_status,operation,items,started_at) VALUES(?1,?2,'proposed','apply',?3,1)",params![patch.id,workspace.id,journal.to_string()])?;
        Ok(())
    }).unwrap();
    std::fs::write(root.join("main.rs"), "external valuable content\n").unwrap();
    let reopened = open(&engine, &root);
    assert_eq!(
        reopened["recoveryRequired"]["code"],
        "PATCH_RECOVERY_REQUIRED"
    );
    let read = engine
        .handle(Request::ReadFile {
            path: "main.rs".into(),
        })
        .unwrap();
    assert_eq!(read["content"], "external valuable content\n");
    let error = engine
        .handle(Request::WriteFile {
            path: "main.rs".into(),
            content: "overwrite".into(),
            expected_hash: read["hash"].as_str().unwrap().into(),
        })
        .unwrap_err();
    assert_eq!(error.code, "PATCH_RECOVERY_REQUIRED");
    assert_eq!(
        std::fs::read_to_string(root.join("main.rs")).unwrap(),
        "external valuable content\n"
    );
    std::fs::write(root.join("main.rs"), before.content).unwrap();
    assert!(open(&engine, &root)["recoveryRequired"].is_null());
    engine
        .handle(Request::CreateFile {
            path: "recovered.txt".into(),
            directory: false,
        })
        .unwrap();
    engine
        .db
        .with(|connection| {
            assert_eq!(
                connection
                    .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))?,
                "ok"
            );
            Ok(())
        })
        .unwrap();
}
#[test]
fn watcher_events_are_versioned_scoped_and_ordered_across_repository_switches() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first");
    let second = temp.path().join("second");
    repository(&first);
    repository(&second);
    let (sender, receiver) = mpsc::channel();
    let engine = Arc::new(
        Engine::new(
            &temp.path().join("state.db"),
            Arc::new(move |name, event| {
                sender.send((name.to_owned(), event)).unwrap();
            }),
        )
        .unwrap(),
    );
    let initial = open(&engine, &first);
    std::fs::write(first.join("main.rs"), "fn first_change() {}\n").unwrap();
    let receive = |expected: &Value, after: u64| {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let (name, event) = receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .unwrap();
            assert_eq!(event["version"], 1);
            assert!(event["sequence"].as_u64().unwrap() > after);
            if name == "workspace_changed" && event["repositoryId"] == *expected {
                assert!(event["payload"]["paths"].is_array());
                return event["sequence"].as_u64().unwrap();
            }
        }
    };
    let sequence = receive(&initial["id"], 0);
    let current = open(&engine, &second);
    std::fs::write(first.join("main.rs"), "fn obsolete_change() {}\n").unwrap();
    std::fs::write(second.join("main.rs"), "fn current_change() {}\n").unwrap();
    receive(&current["id"], sequence);
}
#[test]
fn operation_log_records_failures_without_file_content_and_recovers_interrupted_records() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repository");
    repository(&root);
    let path = temp.path().join("state.db");
    let engine = Arc::new(Engine::new(&path, Arc::new(|_, _| {})).unwrap());
    open(&engine, &root);
    assert!(engine
        .handle(Request::WriteFile {
            path: "main.rs".into(),
            content: "PRIVATE_FILE_CONTENT".into(),
            expected_hash: "stale".into()
        })
        .is_err());
    engine.db.with(|connection| {
        let (id,status,code): (String,String,String) = connection.query_row("SELECT id,status,error_code FROM operation_log WHERE method='write_file'",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        assert_eq!(id.len(),36);
        assert_eq!(status,"failed");
        assert!(!code.contains("PRIVATE_FILE_CONTENT"));
        connection.execute("INSERT INTO operation_log(id,method,started_at,status) VALUES('interrupted-fixture','index_repository',1,'running')",[])?;
        Ok(())
    }).unwrap();
    drop(engine);
    let recovered = Engine::new(&path, Arc::new(|_, _| {})).unwrap();
    recovered
        .db
        .with(|connection| {
            assert_eq!(
                connection.query_row(
                    "SELECT status FROM operation_log WHERE id='interrupted-fixture'",
                    [],
                    |row| row.get::<_, String>(0)
                )?,
                "interrupted"
            );
            Ok(())
        })
        .unwrap();
}
fn queued_request_cancelled_during_admission(method: &'static str) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repository");
    repository(&root);
    let engine = Arc::new(Engine::new(&temp.path().join("state.db"), Arc::new(|_, _| {})).unwrap());
    open(&engine, &root);
    engine.handle(Request::IndexRepository {}).unwrap();
    let (entered_sender, entered_receiver) = mpsc::channel();
    let (release_sender, release_receiver) = mpsc::channel();
    engine.db.with(|connection| {
        connection.create_scalar_function("pause_admission", 0, rusqlite::functions::FunctionFlags::SQLITE_UTF8 | rusqlite::functions::FunctionFlags::SQLITE_INNOCUOUS, move |_| {
            entered_sender.send(()).map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))?;
            release_receiver.recv_timeout(Duration::from_secs(5)).map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))?;
            Ok(0i64)
        })?;
        connection.execute_batch(&format!("CREATE TRIGGER pause_request_admission BEFORE INSERT ON operation_log WHEN NEW.method='{method}' BEGIN SELECT pause_admission(); END;"))?;
        Ok(())
    }).unwrap();
    let worker_engine = engine.clone();
    let worker = std::thread::spawn(move || {
        worker_engine.handle(if method == "index_repository" {
            Request::IndexRepository {}
        } else {
            Request::Search {
                query: "original".into(),
                mode: "symbol".into(),
                offset: 0,
                limit: 10,
            }
        })
    });
    entered_receiver
        .recv_timeout(Duration::from_secs(5))
        .expect("request reached admission with the database mutex held");
    engine
        .handle(if method == "index_repository" {
            Request::CancelIndex {}
        } else {
            Request::CancelSearch {}
        })
        .expect("cancellation remains available while admission owns database mutex");
    release_sender.send(()).unwrap();
    let outcome = worker.join().unwrap();
    engine
        .db
        .with(|connection| {
            connection.execute_batch("DROP TRIGGER pause_request_admission;")?;
            Ok(())
        })
        .unwrap();
    if method == "index_repository" {
        assert!(
            matches!(outcome, Ok(ref value) if value["cancelled"] == true)
                || matches!(outcome, Err(ref error) if matches!(error.code.as_str(), "cancelled" | "CANCELLED")),
            "a queued index started despite acknowledged cancellation: {outcome:?}"
        );
    } else {
        assert!(
            matches!(outcome, Err(ref error) if matches!(error.code.as_str(), "SEARCH_CANCELLED" | "cancelled" | "CANCELLED")),
            "a queued search ran despite acknowledged cancellation: {outcome:?}"
        );
    }
}
#[test]
fn queued_index_cancellation_survives_database_admission_wait() {
    queued_request_cancelled_during_admission("index_repository");
}
#[test]
fn queued_search_cancellation_survives_database_admission_wait() {
    queued_request_cancelled_during_admission("search");
}
#[test]
#[ignore = "Invoked as a real process by process_stop_survives_operation_log_failure"]
fn operation_log_stop_helper() {
    std::thread::sleep(Duration::from_secs(30));
}
#[test]
fn process_stop_survives_operation_log_failure() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("repository");
    repository(&root);
    let engine = Arc::new(Engine::new(&temp.path().join("state.db"), Arc::new(|_, _| {})).unwrap());
    open(&engine, &root);
    let spec = astraforge_core::process::CommandSpec {
        program: std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        args: vec![
            "operation_log_stop_helper".into(),
            "--exact".into(),
            "--ignored".into(),
            "--nocapture".into(),
        ],
        cwd: None,
        env: std::collections::BTreeMap::new(),
        approved: true,
        is_test: false,
    };
    let id = engine
        .handle(Request::StartCommand { spec })
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    engine.db.with(|connection| {
        connection.execute_batch("CREATE TRIGGER reject_operation_log BEFORE INSERT ON operation_log BEGIN SELECT RAISE(ABORT,'injected operation log failure'); END;")?;
        Ok(())
    }).unwrap();
    let stopped = engine.handle(Request::StopCommand { id: id.clone() });
    if stopped.is_err() {
        engine
            .processes
            .stop(&id)
            .expect("cleanup actual child despite regression");
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if !engine.processes.poll(&id, 0).unwrap().running {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "actual command did not terminate"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        stopped.is_ok(),
        "diagnostic persistence blocked process cancellation: {stopped:?}"
    );
}
