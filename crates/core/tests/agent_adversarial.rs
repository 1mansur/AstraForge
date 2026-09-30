use astraforge_core::agent::{
    AgentHost, AgentLimits, AgentManager, AgentSession, ToolCall, ToolResult,
};
use astraforge_core::database::Database;
use astraforge_core::error::{AppError, Result};
use astraforge_core::process::CommandSpec;
use astraforge_core::provider::{ChatMessage, ChatProvider, Completion, Usage};
use rusqlite::params;
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tempfile::TempDir;
struct Fixture {
    database: Arc<Database>,
    manager: Arc<AgentManager>,
    content: PathBuf,
    _directory: TempDir,
}
impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().expect("temporary state");
        let database =
            Arc::new(Database::open(&directory.path().join("state.db")).expect("database"));
        let manager = Arc::new(AgentManager::new(database.clone()).expect("agent manager"));
        let content = directory.path().join("README.md");
        std::fs::write(&content, "Local repository context").expect("context fixture");
        Self {
            database,
            manager,
            content,
            _directory: directory,
        }
    }
    fn insert(&self, session: &AgentSession) {
        self.database.with(|connection| {
            connection.execute("INSERT INTO agent_sessions(id,repository_id,created_at,data) VALUES(?1,?2,?3,?4)", params![session.id, session.repository_id, session.created_at, serde_json::to_string(session)?])?;
            Ok(())
        }).expect("persist session fixture");
    }
    fn host(&self, panic_at: &'static str) -> Arc<dyn AgentHost> {
        Arc::new(FaultHost {
            path: self.content.clone(),
            panic_at,
        })
    }
}
fn session(id: &str, status: &str) -> AgentSession {
    AgentSession {
        id: id.into(),
        repository_id: "repo".into(),
        task: "Inspect local repository".into(),
        mode: "task".into(),
        status: status.into(),
        created_at: 1,
        nodes: Vec::new(),
        edges: Vec::new(),
        summary: format!("Preserve {status}"),
        steps: 0,
        repair_attempts: 0,
        verification_pending: false,
        pending_approval: None,
        pending_tool: None,
        pending_command: None,
        pending_patch: None,
    }
}
struct FaultProvider {
    panic: bool,
    tool: bool,
}
impl ChatProvider for FaultProvider {
    fn stream_chat(
        &self,
        _messages: &[ChatMessage],
        _cancel: &AtomicBool,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Completion> {
        assert!(!self.panic, "injected provider worker panic");
        let content = if self.tool {
            json!({"kind":"tool","tool":{"name":"read_file","arguments":{"path":"README.md"}}})
        } else {
            json!({"kind":"finish","summary":"Completed actual manager execution"})
        }
        .to_string();
        on_delta(&content);
        Ok(Completion {
            content,
            usage: Usage::default(),
        })
    }
}
struct FaultHost {
    path: PathBuf,
    panic_at: &'static str,
}
impl AgentHost for FaultHost {
    fn context(&self, _task: &str, _budget: usize) -> Result<String> {
        assert_ne!(self.panic_at, "context", "injected context worker panic");
        Ok(std::fs::read_to_string(&self.path)?)
    }
    fn execute(
        &self,
        _tool: &ToolCall,
        _approved: bool,
        cancel: &AtomicBool,
    ) -> Result<ToolResult> {
        assert_ne!(self.panic_at, "tool", "injected tool worker panic");
        if cancel.load(Ordering::Acquire) {
            return Err(AppError::new("cancelled", "Cancelled"));
        }
        Ok(ToolResult {
            value: json!({"content":std::fs::read_to_string(&self.path)?}),
            approval: None,
        })
    }
    fn execute_approved(&self, _command: &CommandSpec, _cancel: &AtomicBool) -> Result<ToolResult> {
        Err(AppError::new(
            "unexpected_command",
            "No approved command expected",
        ))
    }
    fn patch_status(&self, _id: &str) -> Result<String> {
        Err(AppError::new("unexpected_patch", "No patch expected"))
    }
}
fn limits() -> AgentLimits {
    AgentLimits {
        max_steps: 5,
        context_budget: 24000,
    }
}
fn wait_terminal(manager: &AgentManager, id: &str) -> AgentSession {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let value = manager.get(id).expect("persisted worker state");
        if value.status != "running" {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "worker never reached terminal state: {id}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
#[test]
fn startup_quarantines_bad_rows_and_recovers_other_sessions() {
    let fixture = Fixture::new();
    fixture.insert(&session("running", "running"));
    fixture.insert(&session("completed", "completed"));
    fixture.database.with(|connection| {
        connection.execute("INSERT INTO agent_sessions(id,repository_id,created_at,data) VALUES('corrupt','repo',1,'not valid JSON')", [])?;
        Ok(())
    }).expect("corrupt persisted row");
    let restarted = AgentManager::new(fixture.database.clone())
        .expect("one corrupt row must not prevent startup");
    assert_eq!(
        restarted.get("running").expect("recovered running").status,
        "interrupted"
    );
    assert_eq!(
        restarted
            .get("completed")
            .expect("preserved completed")
            .status,
        "completed"
    );
    assert!(restarted.get("corrupt").is_err());
    fixture
        .database
        .with(|connection| {
            let raw: String = connection.query_row(
                "SELECT data FROM agent_quarantine WHERE id='corrupt'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(raw, "not valid JSON");
            let count: i64 = connection.query_row(
                "SELECT COUNT(*) FROM diagnostics WHERE category='agent_recovery'",
                [],
                |row| row.get(0),
            )?;
            assert!(count >= 1);
            Ok(())
        })
        .expect("quarantine keeps evidence and emits diagnostics");
}
#[test]
fn oversized_and_identity_mismatched_sessions_are_quarantined() {
    let fixture = Fixture::new();
    fixture.database.with(|connection| {
        connection.execute("INSERT INTO agent_sessions(id,repository_id,created_at,data) VALUES('oversized','repo',1,CAST(zeroblob(33554433) AS TEXT))", [])?;
        connection.execute("INSERT INTO agent_sessions(id,repository_id,created_at,data) VALUES('row-id','repo',1,?1)", [serde_json::to_string(&session("different-id", "running"))?])?;
        Ok(())
    }).expect("hostile persistent rows");
    let restarted = AgentManager::new(fixture.database.clone()).expect("bounded startup recovery");
    assert!(restarted
        .list("repo")
        .expect("remaining sessions")
        .is_empty());
    fixture
        .database
        .with(|connection| {
            let count: i64 =
                connection.query_row("SELECT COUNT(*) FROM agent_quarantine", [], |row| {
                    row.get(0)
                })?;
            assert_eq!(count, 2);
            let reason: String = connection.query_row(
                "SELECT reason FROM agent_quarantine WHERE id='oversized'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(reason, "session_size_limit");
            Ok(())
        })
        .expect("quarantine metadata");
}
#[test]
fn failed_archive_import_does_not_leave_an_orphan_session() {
    let fixture = Fixture::new();
    fixture.database.with(|connection| {
        connection.execute_batch("CREATE TRIGGER reject_import BEFORE INSERT ON agent_imports BEGIN SELECT RAISE(ABORT,'injected import archive failure'); END;")?;
        Ok(())
    }).expect("archive failure injection");
    let archive =
        json!({"format":"astraforge-session","version":1,"session":session("source", "completed")})
            .to_string();
    assert!(fixture.manager.import("repo", &archive).is_err());
    assert!(fixture
        .manager
        .list("repo")
        .expect("session count after failed import")
        .is_empty());
    fixture
        .database
        .with(|connection| {
            let archives: i64 =
                connection.query_row("SELECT COUNT(*) FROM agent_imports", [], |row| row.get(0))?;
            assert_eq!(archives, 0);
            Ok(())
        })
        .expect("archive remains atomic");
}
#[test]
fn quarantine_preserves_the_original_row_if_diagnostic_persistence_fails() {
    let fixture = Fixture::new();
    fixture.database.with(|connection| {
        connection.execute("INSERT INTO agent_sessions(id,repository_id,created_at,data) VALUES('corrupt','repo',1,'invalid JSON')", [])?;
        connection.execute_batch("CREATE TRIGGER reject_quarantine_diagnostic BEFORE INSERT ON diagnostics WHEN NEW.category='agent_recovery' BEGIN SELECT RAISE(ABORT,'injected diagnostic failure'); END;")?;
        Ok(())
    }).expect("quarantine failure fixture");
    assert!(AgentManager::new(fixture.database.clone()).is_err());
    fixture
        .database
        .with(|connection| {
            let original: i64 = connection.query_row(
                "SELECT COUNT(*) FROM agent_sessions WHERE id='corrupt'",
                [],
                |row| row.get(0),
            )?;
            let quarantined: i64 = connection.query_row(
                "SELECT COUNT(*) FROM agent_quarantine WHERE id='corrupt'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(original, 1);
            assert_eq!(quarantined, 0);
            connection.execute_batch("DROP TRIGGER reject_quarantine_diagnostic;")?;
            Ok(())
        })
        .expect("quarantine transaction rolled back");
    let restarted = AgentManager::new(fixture.database.clone()).expect("retry recovery");
    assert!(restarted
        .list("repo")
        .expect("recovery complete")
        .is_empty());
}
#[test]
fn stop_does_not_rewrite_terminal_or_imported_sessions() {
    let fixture = Fixture::new();
    for status in ["completed", "failed", "imported", "cancelled"] {
        let original = session(status, status);
        fixture.insert(&original);
        let result = fixture.manager.stop(status);
        if status == "cancelled" {
            result.expect("idempotent cancellation");
        } else {
            assert_eq!(
                result.expect_err("terminal transition rejected").code,
                "session_state"
            );
        }
        let persisted = fixture.manager.get(status).expect("terminal session");
        assert_eq!(persisted.status, original.status);
        assert_eq!(persisted.summary, original.summary);
    }
}
#[test]
fn worker_panics_fail_sessions_and_release_all_running_slots() {
    let fixture = Fixture::new();
    for panic_at in ["provider", "context", "tool"] {
        let mut ids = Vec::new();
        for _ in 0..4 {
            let started = fixture
                .manager
                .start(
                    "repo",
                    "Inspect repository".into(),
                    "task".into(),
                    Arc::new(FaultProvider {
                        panic: panic_at == "provider",
                        tool: panic_at == "tool",
                    }),
                    fixture.host(panic_at),
                    limits(),
                )
                .expect("start faulting worker");
            ids.push(started.id);
        }
        for id in ids {
            let stopped = wait_terminal(&fixture.manager, &id);
            assert_eq!(stopped.status, "failed");
            assert!(stopped
                .nodes
                .iter()
                .any(|node| node.label == "agent_worker_panic"));
            assert!(stopped.nodes.iter().all(|node| node.status != "running"));
        }
        let started = fixture
            .manager
            .start(
                "repo",
                "Finish after fault".into(),
                "task".into(),
                Arc::new(FaultProvider {
                    panic: false,
                    tool: false,
                }),
                fixture.host("none"),
                limits(),
            )
            .expect("panic cleanup frees capacity");
        assert_eq!(
            wait_terminal(&fixture.manager, &started.id).status,
            "completed"
        );
    }
}
struct GatedHost {
    path: PathBuf,
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    finished: mpsc::Sender<()>,
}
impl Drop for GatedHost {
    fn drop(&mut self) {
        let _ = self.finished.send(());
    }
}
impl AgentHost for GatedHost {
    fn context(&self, _task: &str, _budget: usize) -> Result<String> {
        Ok(std::fs::read_to_string(&self.path)?)
    }
    fn execute(
        &self,
        _tool: &ToolCall,
        _approved: bool,
        _cancel: &AtomicBool,
    ) -> Result<ToolResult> {
        let content = std::fs::read_to_string(&self.path)?;
        self.entered.send(()).expect("signal entered tool");
        self.release
            .lock()
            .expect("tool gate lock")
            .recv_timeout(Duration::from_secs(5))
            .expect("release completed tool");
        Ok(ToolResult {
            value: json!({"content":content}),
            approval: None,
        })
    }
    fn execute_approved(&self, _command: &CommandSpec, _cancel: &AtomicBool) -> Result<ToolResult> {
        Err(AppError::new("unexpected_command", "No command expected"))
    }
    fn patch_status(&self, _id: &str) -> Result<String> {
        Err(AppError::new("unexpected_patch", "No patch expected"))
    }
}
struct ReleaseTool(Option<mpsc::Sender<()>>);
impl ReleaseTool {
    fn release(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
impl Drop for ReleaseTool {
    fn drop(&mut self) {
        self.release();
    }
}
#[test]
fn cancelled_session_never_transiently_resurrects_after_late_tool_result() {
    let fixture = Fixture::new();
    let (entered_sender, entered) = mpsc::channel();
    let (release_sender, release) = mpsc::channel();
    let (finished_sender, finished) = mpsc::channel();
    let mut release_tool = ReleaseTool(Some(release_sender));
    let started = fixture
        .manager
        .start(
            "repo",
            "Inspect repository while cancellation races a tool".into(),
            "task".into(),
            Arc::new(FaultProvider {
                panic: false,
                tool: true,
            }),
            Arc::new(GatedHost {
                path: fixture.content.clone(),
                entered: entered_sender,
                release: Mutex::new(release),
                finished: finished_sender,
            }),
            limits(),
        )
        .expect("start actual worker");
    entered
        .recv_timeout(Duration::from_secs(3))
        .expect("worker entered real file tool");
    fixture.manager.stop(&started.id).expect("persist stop");
    assert_eq!(
        fixture
            .manager
            .get(&started.id)
            .expect("stopped row")
            .status,
        "cancelled"
    );
    fixture.database.with(|connection| {
        connection.execute_batch("CREATE TABLE cancellation_observations(status TEXT NOT NULL); CREATE TRIGGER observe_cancel_insert AFTER INSERT ON agent_sessions BEGIN INSERT INTO cancellation_observations(status) VALUES(json_extract(NEW.data,'$.status')); END; CREATE TRIGGER observe_cancel_update AFTER UPDATE ON agent_sessions BEGIN INSERT INTO cancellation_observations(status) VALUES(json_extract(NEW.data,'$.status')); END;")?;
        Ok(())
    }).expect("audit every committed session write after stop");
    release_tool.release();
    finished
        .recv_timeout(Duration::from_secs(3))
        .expect("worker finalization completed");
    let observations = fixture
        .database
        .with(|connection| {
            let mut statement = connection
                .prepare("SELECT status FROM cancellation_observations ORDER BY rowid")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
        })
        .expect("read all intermediate persisted statuses");
    assert!(!observations.is_empty(), "worker finalizer must persist");
    assert!(
        observations.iter().all(|status| status == "cancelled"),
        "a stopped session was transiently resurrected: {observations:?}"
    );
    let stopped = fixture.manager.get(&started.id).expect("final stopped row");
    assert_eq!(stopped.status, "cancelled");
    assert!(stopped.nodes.iter().all(|node| node.status != "running"));
}
