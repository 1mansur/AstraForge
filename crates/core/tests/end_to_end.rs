use astraforge_core::patch::PatchProposal;
use astraforge_core::process::CommandSpec;
use astraforge_core::service::{Engine, Request};
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
fn git(root: &std::path::Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=AstraForge test",
            "-c",
            "user.email=test@example.invalid",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn run_test(engine: &Arc<Engine>) -> i64 {
    let spec = CommandSpec {
        program: std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        args: vec![
            "repository_test_helper".into(),
            "--ignored".into(),
            "--nocapture".into(),
        ],
        cwd: None,
        env: BTreeMap::new(),
        approved: true,
        is_test: true,
    };
    let id = engine
        .handle(Request::StartCommand { spec })
        .unwrap()
        .as_str()
        .unwrap()
        .to_string();
    let start = Instant::now();
    loop {
        let state = engine
            .handle(Request::PollCommand {
                id: id.clone(),
                cursor: 0,
            })
            .unwrap();
        if state["running"] == false {
            return state["exitCode"].as_i64().unwrap();
        }
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(30));
    }
}
#[test]
#[ignore = "Invoked as a real subprocess by the repository workflow test"]
fn repository_test_helper() {
    let content = std::fs::read_to_string("auth.ts").unwrap();
    assert!(
        content.contains("return true"),
        "Authentication regression: refresh still fails"
    );
}
#[test]
fn repository_edit_patch_test_git_and_restart_work_together() {
    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path().join("repository");
    std::fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "-q"]);
    std::fs::write(
        repository.join("auth.ts"),
        "export function refresh() { return false; }\n",
    )
    .unwrap();
    git(&repository, &["add", "auth.ts"]);
    git(&repository, &["commit", "-qm", "Initial auth"]);
    let database = temporary.path().join("state.sqlite");
    let engine = Arc::new(Engine::new(&database, Arc::new(|_, _| {})).unwrap());
    let repo = engine
        .handle(Request::OpenRepository {
            path: repository.to_string_lossy().into_owned(),
        })
        .unwrap();
    engine.handle(Request::IndexRepository {}).unwrap();
    assert_eq!(
        engine
            .handle(Request::Tree {
                path: String::new()
            })
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        engine
            .handle(Request::Search {
                query: "refresh".into(),
                mode: "symbol".into(),
                offset: 0,
                limit: 20
            })
            .unwrap()[0]["path"],
        "auth.ts"
    );
    assert_ne!(run_test(&engine), 0);
    let original = engine
        .handle(Request::ReadFile {
            path: "auth.ts".into(),
        })
        .unwrap();
    let patch = engine
        .handle(Request::ProposePatch {
            proposals: vec![PatchProposal {
                path: "auth.ts".into(),
                content: Some("export function refresh() { return true; }\n".into()),
                expected_hash: Some(original["hash"].as_str().unwrap().into()),
            }],
            source: "end-to-end test".into(),
        })
        .unwrap();
    assert!(std::fs::read_to_string(repository.join("auth.ts"))
        .unwrap()
        .contains("false"));
    let id = patch["id"].as_str().unwrap().to_string();
    engine
        .handle(Request::ApplyPatch { id: id.clone() })
        .unwrap();
    assert_eq!(run_test(&engine), 0);
    let status = engine.handle(Request::GitStatus {}).unwrap();
    assert_eq!(status["entries"][0]["path"], "auth.ts");
    assert!(engine
        .handle(Request::GitDiff {
            path: Some("auth.ts".into()),
            staged: false
        })
        .unwrap()
        .as_str()
        .unwrap()
        .contains("+export function refresh() { return true; }"));
    engine.handle(Request::RevertPatch { id }).unwrap();
    assert_eq!(
        engine
            .handle(Request::ReadFile {
                path: "auth.ts".into()
            })
            .unwrap()["hash"],
        original["hash"]
    );
    drop(engine);
    std::thread::sleep(Duration::from_millis(500));
    let reopened = Arc::new(Engine::new(&database, Arc::new(|_, _| {})).unwrap());
    assert_eq!(
        reopened.handle(Request::Repositories {}).unwrap()[0]["id"],
        repo["id"]
    );
    reopened
        .handle(Request::OpenRepository {
            path: repository.to_string_lossy().into_owned(),
        })
        .unwrap();
    assert_eq!(
        reopened.handle(Request::Patches {}).unwrap()[0]["status"],
        "reverted"
    );
}
