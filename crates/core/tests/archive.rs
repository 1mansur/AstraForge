use astraforge_core::agent::AgentSession;
use astraforge_core::patch::PatchProposal;
use astraforge_core::process::CommandSpec;
use astraforge_core::service::{Engine, Request};
use rusqlite::params;
use serde_json::json;
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
#[test]
fn session_exports_actual_artifacts_and_import_retains_them_without_execution() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("repo");
    std::fs::create_dir(&root).unwrap();
    assert!(Command::new("git")
        .args(["init", "-q"])
        .arg(&root)
        .status()
        .unwrap()
        .success());
    std::fs::write(root.join("a.ts"), "const a=1;\n").unwrap();
    let engine =
        Arc::new(Engine::new(&temporary.path().join("db.sqlite"), Arc::new(|_, _| {})).unwrap());
    let repo = engine
        .handle(Request::OpenRepository {
            path: root.to_string_lossy().into_owned(),
        })
        .unwrap();
    let workspace = engine.workspace().unwrap();
    let file = workspace.read("a.ts").unwrap();
    let patch = engine
        .patches
        .propose(
            &workspace,
            vec![PatchProposal {
                path: "a.ts".into(),
                content: Some("const a=2;\n".into()),
                expected_hash: Some(file.hash),
            }],
            "agent",
        )
        .unwrap();
    let command = engine
        .processes
        .start(
            &workspace,
            CommandSpec {
                program: "git".into(),
                args: vec!["--version".into()],
                cwd: None,
                env: BTreeMap::new(),
                approved: false,
                is_test: true,
            },
        )
        .unwrap();
    let start = Instant::now();
    while engine.processes.poll(&command, 0).unwrap().running {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(20));
    }
    let session:AgentSession=serde_json::from_value(json!({"id":"archive-test","repositoryId":repo["id"],"task":"test export","mode":"task","status":"completed","createdAt":1,"nodes":[],"edges":[],"summary":"export fixture","steps":1,"repairAttempts":1,"pendingApproval":null,"pendingTool":null,"pendingPatch":null})).unwrap();
    engine.db.with(|connection|{
        connection.execute("INSERT INTO agent_sessions(id,repository_id,created_at,data) VALUES(?1,?2,1,?3)",params![session.id,workspace.id,serde_json::to_string(&session)?])?;
        for (id,result) in [("patch-call",json!({"id":patch.id})),("test-call",json!({"id":command}))] {
            connection.execute("INSERT INTO tool_calls(id,session_id,timestamp,arguments,result,duration_ms,status) VALUES(?1,?2,1,'{}',?3,1,'completed')",params![id,session.id,result.to_string()])?;
        }
        Ok(())
    }).unwrap();
    let exported = engine.agents.export(&session.id).unwrap();
    let archive: serde_json::Value = serde_json::from_str(&exported).unwrap();
    assert_eq!(archive["patches"][0]["changes"][0]["after"], "const a=2;\n");
    assert_eq!(archive["executions"][0]["exitCode"], 0);
    assert_eq!(archive["executions"][0]["isTest"], true);
    let imported = engine.agents.import(&workspace.id, &exported).unwrap();
    assert_eq!(imported.status, "imported");
    let reexported: serde_json::Value =
        serde_json::from_str(&engine.agents.export(&imported.id).unwrap()).unwrap();
    assert_eq!(reexported["patches"], archive["patches"]);
    assert_eq!(reexported["executions"], archive["executions"]);
    assert_eq!(
        std::fs::read_to_string(root.join("a.ts")).unwrap(),
        "const a=1;\n"
    );
}
