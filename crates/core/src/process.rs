use crate::database::Database;
use crate::error::{AppError, Result};
use crate::workspace::Workspace;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;
const MAX_BUFFER_BYTES: usize = 1024 * 1024;
const MAX_POLL_BYTES: usize = 128 * 1024;
const MAX_SESSIONS: usize = 128;
const MAX_RUNNING: usize = 16;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub approved: bool,
    #[serde(default)]
    pub is_test: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolicyDecision {
    pub level: String,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputChunk {
    pub sequence: u64,
    pub stream: String,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSnapshot {
    pub id: String,
    pub chunks: Vec<OutputChunk>,
    pub next_cursor: u64,
    pub exit_code: Option<i32>,
    pub running: bool,
    pub truncated: bool,
}
fn decision(level: &str, reason: &str) -> PolicyDecision {
    PolicyDecision {
        level: level.to_owned(),
        reason: reason.to_owned(),
    }
}
pub fn classify(spec: &CommandSpec) -> PolicyDecision {
    if spec.program.is_empty()
        || spec.program.contains(['\0', '\r', '\n'])
        || spec.args.len() > 256
        || spec
            .args
            .iter()
            .any(|argument| argument.contains('\0') || argument.len() > 32_768)
    {
        return decision(
            "BLOCKED",
            "The executable or arguments exceed the command protocol limits",
        );
    }
    let basename = spec
        .program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let basename = basename.strip_suffix(".exe").unwrap_or(&basename);
    if matches!(
        basename,
        "sudo"
            | "runas"
            | "diskpart"
            | "format"
            | "shutdown"
            | "reboot"
            | "bcdedit"
            | "reg"
            | "sc"
            | "mount"
            | "umount"
            | "mkfs"
            | "dd"
            | "netsh"
            | "takeown"
            | "icacls"
    ) {
        return decision(
            "BLOCKED",
            "System administration commands are outside repository tooling",
        );
    }
    if matches!(
        basename,
        "cmd"
            | "powershell"
            | "pwsh"
            | "bash"
            | "sh"
            | "zsh"
            | "fish"
            | "wscript"
            | "cscript"
            | "rm"
            | "del"
            | "rmdir"
            | "erase"
            | "mv"
            | "chmod"
            | "chown"
    ) {
        return decision(
            "DANGEROUS",
            "A shell or destructive command requires explicit approval for this invocation",
        );
    }
    if spec.program.contains(['/', '\\']) {
        return decision(
            "CAUTION",
            "Explicit executable paths require approval because repository binaries are untrusted",
        );
    }
    if !spec.env.is_empty() {
        return decision(
            "CAUTION",
            "Environment overrides can change executable behavior and require approval",
        );
    }
    if basename == "git" {
        let safe = match spec.args.as_slice() {
            [command] => matches!(
                command.as_str(),
                "status" | "diff" | "log" | "--version" | "version"
            ),
            [command, options @ ..] if command == "status" => options.iter().all(|value| {
                matches!(
                    value.as_str(),
                    "--short"
                        | "-s"
                        | "--branch"
                        | "-b"
                        | "--porcelain"
                        | "--porcelain=v1"
                        | "--porcelain=v2"
                )
            }),
            [command, options @ ..] if command == "diff" => options.iter().all(|value| {
                matches!(
                    value.as_str(),
                    "--stat"
                        | "--name-only"
                        | "--name-status"
                        | "--cached"
                        | "--staged"
                        | "--no-color"
                        | "--check"
                )
            }),
            [command, option] if command == "log" => matches!(
                option.as_str(),
                "--oneline" | "-1" | "-5" | "-10" | "--stat"
            ),
            [command, option] if command == "rev-parse" => {
                matches!(option.as_str(), "--show-toplevel" | "--is-inside-work-tree")
            }
            [command, option] if command == "branch" => {
                matches!(option.as_str(), "--list" | "--show-current")
            }
            _ => false,
        };
        return if safe {
            decision("SAFE", "This exact Git argument form is read only; hooks, pager and external diff are disabled")
        } else {
            decision(
                "CAUTION",
                "This Git invocation is not in the read-only argument grammar",
            )
        };
    }
    decision(
        "CAUTION",
        "Programs and test runners may execute repository code; explicit approval is required",
    )
}
struct OutputBuffer {
    chunks: VecDeque<OutputChunk>,
    bytes: usize,
    next_sequence: u64,
    exit_code: Option<i32>,
    running: bool,
    dropped: bool,
}
impl OutputBuffer {
    fn new() -> Self {
        Self {
            chunks: VecDeque::new(),
            bytes: 0,
            next_sequence: 1,
            exit_code: None,
            running: true,
            dropped: false,
        }
    }
    fn push(&mut self, stream: &str, text: String) {
        if text.is_empty() {
            return;
        }
        let chunk = OutputChunk {
            sequence: self.next_sequence,
            stream: stream.to_owned(),
            text,
        };
        self.next_sequence += 1;
        self.bytes += chunk.text.len();
        self.chunks.push_back(chunk);
        while self.bytes > MAX_BUFFER_BYTES || self.chunks.len() > 2048 {
            if let Some(chunk) = self.chunks.pop_front() {
                self.bytes -= chunk.text.len();
                self.dropped = true;
            }
        }
    }
    fn snapshot(&self, id: &str, cursor: u64) -> ProcessSnapshot {
        let mut bytes = 0;
        let mut chunks = Vec::new();
        for chunk in self.chunks.iter().filter(|chunk| chunk.sequence > cursor) {
            if bytes + chunk.text.len() > MAX_POLL_BYTES && !chunks.is_empty() {
                break;
            }
            bytes += chunk.text.len();
            chunks.push(chunk.clone());
        }
        let next_cursor = chunks
            .last()
            .map(|chunk| chunk.sequence)
            .unwrap_or(cursor.min(self.next_sequence - 1));
        let truncated = self
            .chunks
            .front()
            .map(|chunk| cursor.saturating_add(1) < chunk.sequence)
            .unwrap_or(self.dropped);
        ProcessSnapshot {
            id: id.to_owned(),
            chunks,
            next_cursor,
            exit_code: self.exit_code,
            running: self.running,
            truncated,
        }
    }
}
struct RunningProcess {
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    output: Mutex<OutputBuffer>,
    tree: ProcessTree,
}
impl RunningProcess {
    fn push(&self, stream: &str, text: String) -> Result<()> {
        self.output
            .lock()
            .map_err(|_| AppError::new("PROCESS_LOCK", "Process output lock was poisoned"))?
            .push(stream, text);
        Ok(())
    }
    fn terminate(&self) -> Result<()> {
        if !self
            .output
            .lock()
            .map_err(|_| AppError::new("PROCESS_LOCK", "Process output lock was poisoned"))?
            .running
        {
            return Ok(());
        }
        let mut child = self
            .child
            .lock()
            .map_err(|_| AppError::new("PROCESS_LOCK", "Process lock was poisoned"))?;
        self.tree.terminate(&mut child)
    }
}
pub struct ProcessManager {
    db: Arc<Database>,
    sessions: Mutex<HashMap<String, Arc<RunningProcess>>>,
    start_gate: Mutex<()>,
    initialization_error: Option<String>,
}
impl ProcessManager {
    pub fn new(db: Arc<Database>) -> Self {
        let initialization_error = db.with(|connection| {
            connection.execute_batch("CREATE TABLE IF NOT EXISTS command_runs(id TEXT PRIMARY KEY,repository_id TEXT NOT NULL,program TEXT NOT NULL,args_json TEXT NOT NULL,cwd TEXT NOT NULL,env_names_json TEXT NOT NULL,is_test INTEGER NOT NULL,status TEXT NOT NULL,started_at INTEGER NOT NULL,duration_ms INTEGER NOT NULL DEFAULT 0,exit_code INTEGER,output_json TEXT NOT NULL DEFAULT '[]',truncated INTEGER NOT NULL DEFAULT 0);CREATE INDEX IF NOT EXISTS command_repository ON command_runs(repository_id,started_at);UPDATE command_runs SET status='interrupted' WHERE status='running';")?;
            Ok(())
        }).err().map(|error| error.message);
        Self {
            db,
            sessions: Mutex::new(HashMap::new()),
            start_gate: Mutex::new(()),
            initialization_error,
        }
    }
    pub fn start(&self, workspace: &Workspace, spec: CommandSpec) -> Result<String> {
        let _start_guard = self
            .start_gate
            .lock()
            .map_err(|_| AppError::new("PROCESS_LOCK", "Process start lock was poisoned"))?;
        if let Some(error) = &self.initialization_error {
            return Err(AppError::new("PROCESS_DATABASE", error));
        }
        let policy = classify(&spec);
        if policy.level == "BLOCKED" {
            return Err(AppError::new("COMMAND_BLOCKED", policy.reason));
        }
        if policy.level != "SAFE" && !spec.approved {
            return Err(AppError::new("COMMAND_APPROVAL", policy.reason));
        }
        let cwd = match spec.cwd.as_deref() {
            Some(path) if !path.is_empty() && path != "." => workspace.resolve(path)?,
            _ => workspace.root.clone(),
        };
        if !cwd.is_dir() {
            return Err(AppError::new(
                "COMMAND_CWD",
                "Command working directory is not a directory",
            ));
        }
        let program = resolve_program(&spec.program, &workspace.root)?;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| AppError::new("PROCESS_LOCK", "Process session lock was poisoned"))?;
        let mut running = 0;
        let mut complete = Vec::new();
        for (id, process) in sessions.iter() {
            if process
                .output
                .lock()
                .map_err(|_| AppError::new("PROCESS_LOCK", "Process output lock was poisoned"))?
                .running
            {
                running += 1;
            } else {
                complete.push(id.clone());
            }
        }
        if running >= MAX_RUNNING {
            return Err(AppError::new(
                "PROCESS_LIMIT",
                "At most 16 commands may run at once",
            ));
        }
        for id in complete {
            if sessions.len() < MAX_SESSIONS {
                break;
            }
            sessions.remove(&id);
        }
        let mut command = Command::new(&program);
        command
            .current_dir(&cwd)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in [
            "PATH",
            "PATHEXT",
            "SystemRoot",
            "SYSTEMROOT",
            "WINDIR",
            "COMSPEC",
            "TEMP",
            "TMP",
            "TMPDIR",
            "HOME",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "LANG",
            "LC_ALL",
            "TERM",
            "NUMBER_OF_PROCESSORS",
            "PROCESSOR_ARCHITECTURE",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        for (name, value) in &spec.env {
            if name.contains(['=', '\0']) || name.is_empty() || value.contains('\0') {
                return Err(AppError::new(
                    "COMMAND_ENV",
                    "An environment variable is invalid",
                ));
            }
            command.env(name, value);
        }
        let basename = program
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if basename.eq_ignore_ascii_case("git") {
            command.args([
                "--no-pager",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "diff.external=",
                "-c",
                "core.pager=cat",
                "-c",
                "interactive.diffFilter=",
            ]);
            command
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_OPTIONAL_LOCKS", "0");
        }
        command.args(&spec.args);
        if basename.eq_ignore_ascii_case("git")
            && matches!(
                spec.args.first().map(String::as_str),
                Some("diff" | "show" | "log")
            )
        {
            command.args(["--no-ext-diff", "--no-textconv"]);
        }
        configure_process(&mut command);
        let id = Uuid::new_v4().to_string();
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| AppError::new("CLOCK", error.to_string()))?
            .as_millis() as i64;
        self.db.with(|connection| {
            connection.execute("INSERT INTO command_runs(id,repository_id,program,args_json,cwd,env_names_json,is_test,status,started_at) VALUES(?1,?2,?3,?4,?5,?6,?7,'running',?8)", params![id, workspace.id, spec.program, serde_json::to_string(&spec.args)?, cwd.to_string_lossy(), serde_json::to_string(&spec.env.keys().collect::<Vec<_>>())?, spec.is_test, started_at])?;
            Ok(())
        })?;
        let spawned = command.spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                self.db.with(|connection| {
                    connection
                        .execute("UPDATE command_runs SET status='failed' WHERE id=?1", [&id])?;
                    Ok(())
                })?;
                return Err(AppError::new(
                    "COMMAND_START",
                    format!("Command could not start: {error}"),
                ));
            }
        };
        let tree = match ProcessTree::attach(&mut child) {
            Ok(tree) => tree,
            Err(error) => {
                child.kill()?;
                child.wait()?;
                self.db.with(|connection| {
                    connection
                        .execute("UPDATE command_runs SET status='failed' WHERE id=?1", [&id])?;
                    Ok(())
                })?;
                return Err(error);
            }
        };
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AppError::new("PROCESS_PIPE", "Command stdout is unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AppError::new("PROCESS_PIPE", "Command stderr is unavailable"))?;
        let stdin = child.stdin.take();
        let process = Arc::new(RunningProcess {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            output: Mutex::new(OutputBuffer::new()),
            tree,
        });
        sessions.insert(id.clone(), process.clone());
        drop(sessions);
        let out_process = process.clone();
        let err_process = process.clone();
        let stdout_worker = thread::spawn(move || read_stream(stdout, &out_process, "stdout"));
        let stderr_worker = thread::spawn(move || read_stream(stderr, &err_process, "stderr"));
        let worker_id = id.clone();
        let db = self.db.clone();
        thread::spawn(move || {
            let started = Instant::now();
            let outcome = monitor(&process, stdout_worker, stderr_worker);
            let exit_code = match outcome {
                Ok(code) => code,
                Err(error) => {
                    if let Err(push_error) =
                        process.push("diagnostic", format!("{}: {}\n", error.code, error.message))
                    {
                        eprintln!("{}", push_error.message);
                    }
                    -1
                }
            };
            let persisted = (|| -> Result<()> {
                let mut output = process.output.lock().map_err(|_| {
                    AppError::new("PROCESS_LOCK", "Process output lock was poisoned")
                })?;
                output.exit_code = Some(exit_code);
                output.running = false;
                let chunks = serde_json::to_string(&output.chunks)?;
                db.with(|connection| {
                    connection.execute("UPDATE command_runs SET status='completed',duration_ms=?2,exit_code=?3,output_json=?4,truncated=?5 WHERE id=?1", params![worker_id, started.elapsed().as_millis() as i64, exit_code, chunks, output.dropped])?;
                    Ok(())
                })
            })();
            if let Err(error) = persisted {
                if let Err(push_error) = process.push(
                    "diagnostic",
                    format!("Persistence failed: {}\n", error.message),
                ) {
                    eprintln!("{}", push_error.message);
                }
            }
        });
        Ok(id)
    }
    fn get(&self, id: &str) -> Result<Arc<RunningProcess>> {
        self.sessions
            .lock()
            .map_err(|_| AppError::new("PROCESS_LOCK", "Process session lock was poisoned"))?
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::new("PROCESS_NOT_FOUND", "This command is no longer active"))
    }
    pub fn poll(&self, id: &str, cursor: u64) -> Result<ProcessSnapshot> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|_| AppError::new("PROCESS_LOCK", "Process session lock was poisoned"))?;
        if let Some(process) = sessions.get(id) {
            return Ok(process
                .output
                .lock()
                .map_err(|_| AppError::new("PROCESS_LOCK", "Process output lock was poisoned"))?
                .snapshot(id, cursor));
        }
        drop(sessions);
        self.db.with(|connection| {
            let row = connection
                .query_row(
                    "SELECT output_json,exit_code,status,truncated FROM command_runs WHERE id=?1",
                    [id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, Option<i32>>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, bool>(3)?,
                        ))
                    },
                )
                .optional()?
                .ok_or_else(|| AppError::new("PROCESS_NOT_FOUND", "Command not found"))?;
            let mut buffer = OutputBuffer::new();
            buffer.chunks = serde_json::from_str(&row.0)?;
            buffer.next_sequence = buffer
                .chunks
                .back()
                .map(|chunk| chunk.sequence + 1)
                .unwrap_or(1);
            buffer.exit_code = row.1;
            buffer.running = false;
            buffer.dropped = row.3;
            if row.2 == "interrupted" {
                buffer.push(
                    "diagnostic",
                    "This command was interrupted by application shutdown.\n".to_owned(),
                );
            }
            Ok(buffer.snapshot(id, cursor))
        })
    }
    pub fn input(&self, id: &str, text: &str) -> Result<()> {
        if text.len() > 16 * 1024 {
            return Err(AppError::new("STDIN_LIMIT", "Input exceeds 16 KiB"));
        }
        let process = self.get(id)?;
        let mut guard = process
            .stdin
            .lock()
            .map_err(|_| AppError::new("PROCESS_LOCK", "Command input lock was poisoned"))?;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| AppError::new("STDIN_CLOSED", "Command input is closed"))?;
        stdin.write_all(text.as_bytes())?;
        stdin.flush()?;
        Ok(())
    }
    pub fn stop(&self, id: &str) -> Result<()> {
        let process = self.get(id)?;
        process.terminate()?;
        process
            .stdin
            .lock()
            .map_err(|_| AppError::new("PROCESS_LOCK", "Command input lock was poisoned"))?
            .take();
        Ok(())
    }
    pub fn history(&self, workspace: &Workspace) -> Result<serde_json::Value> {
        self.db.with(|connection| {
            let mut statement = connection.prepare("SELECT id,program,args_json,is_test,status,started_at,duration_ms,exit_code FROM command_runs WHERE repository_id=?1 ORDER BY started_at DESC LIMIT 100")?;
            let rows = statement.query_map([&workspace.id], |row| Ok(json!({"id":row.get::<_, String>(0)?,"program":row.get::<_, String>(1)?,"arguments":row.get::<_, String>(2)?,"isTest":row.get::<_, bool>(3)?,"status":row.get::<_, String>(4)?,"startedAt":row.get::<_, i64>(5)?,"durationMs":row.get::<_, i64>(6)?,"exitCode":row.get::<_, Option<i32>>(7)?})))?.collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(json!(rows))
        })
    }
}
impl Drop for ProcessManager {
    fn drop(&mut self) {
        match self.sessions.lock() {
            Ok(sessions) => {
                for process in sessions.values() {
                    if let Err(error) = process.terminate() {
                        eprintln!("Process shutdown failed: {}", error.message);
                    }
                }
            }
            Err(error) => eprintln!("Process shutdown lock failed: {error}"),
        }
    }
}
fn monitor(
    process: &Arc<RunningProcess>,
    stdout: thread::JoinHandle<Result<()>>,
    stderr: thread::JoinHandle<Result<()>>,
) -> Result<i32> {
    let exit_code = loop {
        let status = process
            .child
            .lock()
            .map_err(|_| AppError::new("PROCESS_LOCK", "Process lock was poisoned"))?
            .try_wait()?;
        if let Some(status) = status {
            break status.code().unwrap_or(-1);
        }
        thread::sleep(Duration::from_millis(20));
    };
    process.terminate()?;
    process
        .stdin
        .lock()
        .map_err(|_| AppError::new("PROCESS_LOCK", "Command input lock was poisoned"))?
        .take();
    stdout
        .join()
        .map_err(|_| AppError::new("STREAM_WORKER", "Command output reader failed"))??;
    stderr
        .join()
        .map_err(|_| AppError::new("STREAM_WORKER", "Command error reader failed"))??;
    Ok(exit_code)
}
fn read_stream(mut stream: impl Read, process: &RunningProcess, name: &str) -> Result<()> {
    let mut chunk = [0_u8; 8192];
    let mut pending = Vec::new();
    loop {
        let size = stream.read(&mut chunk)?;
        if size == 0 {
            if !pending.is_empty() {
                process.push(name, String::from_utf8_lossy(&pending).to_string())?;
            }
            return Ok(());
        }
        pending.extend_from_slice(&chunk[..size]);
        let mut consumed = 0;
        let mut output = String::new();
        while consumed < pending.len() {
            match std::str::from_utf8(&pending[consumed..]) {
                Ok(value) => {
                    output.push_str(value);
                    consumed = pending.len();
                }
                Err(error) => {
                    let valid_end = consumed + error.valid_up_to();
                    output.push_str(
                        std::str::from_utf8(&pending[consumed..valid_end])
                            .map_err(|error| AppError::new("STREAM_UTF8", error.to_string()))?,
                    );
                    consumed = valid_end;
                    match error.error_len() {
                        Some(size) => {
                            output.push('\u{fffd}');
                            consumed += size;
                        }
                        None => break,
                    }
                }
            }
        }
        pending.drain(..consumed);
        process.push(name, output)?;
    }
}
pub(crate) fn resolve_program(program: &str, repository: &Path) -> Result<PathBuf> {
    if program.contains(['/', '\\']) {
        let candidate = Path::new(program);
        let candidate = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            repository.join(candidate)
        };
        let canonical = candidate.canonicalize()?;
        if !canonical.is_file() {
            return Err(AppError::new(
                "PROGRAM_PATH",
                "Executable path is not a file",
            ));
        }
        return Ok(canonical);
    }
    let search = std::env::var_os("PATH")
        .ok_or_else(|| AppError::new("PROGRAM_PATH", "PATH is not configured"))?;
    let mut suffixes = vec![String::new()];
    if cfg!(windows) && Path::new(program).extension().is_none() {
        suffixes.extend(
            [".exe", ".com", ".cmd", ".bat"]
                .into_iter()
                .map(str::to_owned),
        );
    }
    for directory in std::env::split_paths(&search) {
        if !directory.is_absolute() {
            continue;
        }
        for suffix in &suffixes {
            let path = directory.join(format!("{program}{suffix}"));
            if path.is_file() {
                let canonical = path.canonicalize()?;
                let repository = repository.canonicalize()?;
                if canonical.starts_with(&repository) {
                    continue;
                }
                return Ok(canonical);
            }
        }
    }
    Err(AppError::new(
        "PROGRAM_NOT_FOUND",
        "Executable was not found in the system PATH outside this repository",
    ))
}
#[cfg(windows)]
pub(crate) fn configure_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000204);
}
#[cfg(unix)]
pub(crate) fn configure_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}
#[cfg(windows)]
pub(crate) struct ProcessTree {
    handle: usize,
}
#[cfg(windows)]
impl ProcessTree {
    pub(crate) fn attach(child: &mut Child) -> Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut information: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &information as *const _ as *const std::ffi::c_void,
                std::mem::size_of_val(&information) as u32,
            )
        };
        if configured == 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            return Err(error.into());
        }
        if unsafe { AssignProcessToJobObject(handle, child.as_raw_handle()) } == 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                CloseHandle(handle);
            }
            return Err(error.into());
        }
        let tree = Self {
            handle: handle as usize,
        };
        resume_initial_thread(child.id())?;
        Ok(tree)
    }
    pub(crate) fn terminate(&self, child: &mut Child) -> Result<()> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        if unsafe { TerminateJobObject(self.handle as _, 1) } == 0 && child.try_wait()?.is_none() {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
}
#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        if unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle as _) } == 0 {
            eprintln!(
                "Process job handle could not close: {}",
                std::io::Error::last_os_error()
            );
        }
    }
}
#[cfg(unix)]
pub(crate) struct ProcessTree {
    group: i32,
}
#[cfg(unix)]
impl ProcessTree {
    pub(crate) fn attach(child: &mut Child) -> Result<Self> {
        Ok(Self {
            group: child.id() as i32,
        })
    }
    pub(crate) fn terminate(&self, _child: &mut Child) -> Result<()> {
        if unsafe { libc::kill(-self.group, libc::SIGKILL) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error.into());
            }
        }
        Ok(())
    }
}
#[cfg(windows)]
fn resume_initial_thread(process_id: u32) -> Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().into());
    }
    let outcome = (|| -> Result<()> {
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut has_entry = unsafe { Thread32First(snapshot, &mut entry) } != 0;
        while has_entry {
            if entry.th32OwnerProcessID == process_id {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(std::io::Error::last_os_error().into());
                }
                let resumed = unsafe { ResumeThread(thread) };
                let error = if resumed == u32::MAX {
                    Some(std::io::Error::last_os_error())
                } else {
                    None
                };
                if unsafe { CloseHandle(thread) } == 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                if let Some(error) = error {
                    return Err(error.into());
                }
                return Ok(());
            }
            has_entry = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        }
        Err(AppError::new(
            "PROCESS_THREAD",
            "The suspended command's initial thread could not be found",
        ))
    })();
    if unsafe { CloseHandle(snapshot) } == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    outcome
}
