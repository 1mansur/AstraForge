use crate::error::{AppError, Result};
use crate::workspace::Workspace;
use serde_json::{json, Value};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
const MAX_GIT_OUTPUT: usize = 8 * 1024 * 1024;
pub struct GitService;
fn collect_output(mut stream: impl Read, overflow: Arc<AtomicBool>) -> std::io::Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Ok(result);
        }
        if result.len() + read > MAX_GIT_OUTPUT {
            overflow.store(true, Ordering::Release);
            return Ok(result);
        }
        result.extend_from_slice(&chunk[..read]);
    }
}
pub(crate) fn run(workspace: &Workspace, args: &[&str]) -> Result<Vec<u8>> {
    run_cancellable(workspace, args, &AtomicBool::new(false))
}
pub(crate) fn run_cancellable(
    workspace: &Workspace,
    args: &[&str],
    cancel: &AtomicBool,
) -> Result<Vec<u8>> {
    run_inner(workspace, args, true, false, cancel)
}
fn run_inner(
    workspace: &Workspace,
    args: &[&str],
    helpers: bool,
    allow_empty_config: bool,
    cancel: &AtomicBool,
) -> Result<Vec<u8>> {
    if cancel.load(Ordering::Acquire) {
        return Err(AppError::new("CANCELLED", "Git operation cancelled"));
    }
    let program = crate::process::resolve_program("git", &workspace.root)?;
    crate::process::validate_automatic_executable(&program)?;
    let mut command = Command::new(program);
    crate::process::minimal_environment(&mut command);
    if helpers {
        configure(&mut command, workspace)?;
    }
    command
        .current_dir(&workspace.root)
        .arg("--work-tree")
        .arg(&workspace.root)
        .args([
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
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env_remove("GIT_EXTERNAL_DIFF")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::process::configure_process(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| AppError::new("GIT_START", format!("Git could not start: {error}")))?;
    let tree = match crate::process::ProcessTree::attach(&mut child) {
        Ok(tree) => tree,
        Err(error) => {
            child.kill()?;
            child.wait()?;
            return Err(error);
        }
    };
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::new("GIT_PIPE", "Git stdout is unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AppError::new("GIT_PIPE", "Git stderr is unavailable"))?;
    let overflow = Arc::new(AtomicBool::new(false));
    let stdout_overflow = overflow.clone();
    let stderr_overflow = overflow.clone();
    let out_thread = thread::spawn(move || collect_output(stdout, stdout_overflow));
    let err_thread = thread::spawn(move || collect_output(stderr, stderr_overflow));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if cancel.load(Ordering::Acquire)
            || overflow.load(Ordering::Acquire)
            || started.elapsed() > Duration::from_secs(30)
        {
            tree.terminate(&mut child)?;
            child.wait()?;
            out_thread
                .join()
                .map_err(|_| AppError::new("GIT_READER", "Git output reader failed"))??;
            err_thread
                .join()
                .map_err(|_| AppError::new("GIT_READER", "Git error reader failed"))??;
            if cancel.load(Ordering::Acquire) {
                return Err(AppError::new("CANCELLED", "Git operation cancelled"));
            }
            return Err(AppError::new(
                "GIT_LIMIT",
                "Git exceeded the 30 second or 8 MiB output limit; narrow the request",
            ));
        }
        thread::sleep(Duration::from_millis(15));
    };
    tree.terminate(&mut child)?;
    let stdout = out_thread
        .join()
        .map_err(|_| AppError::new("GIT_READER", "Git output reader failed"))??;
    let stderr = err_thread
        .join()
        .map_err(|_| AppError::new("GIT_READER", "Git error reader failed"))??;
    if overflow.load(Ordering::Acquire) {
        return Err(AppError::new(
            "GIT_LIMIT",
            "Git output exceeded 8 MiB; narrow the request",
        ));
    }
    if !status.success() && !(allow_empty_config && status.code() == Some(1)) {
        let detail: String = String::from_utf8_lossy(&stderr)
            .chars()
            .take(2000)
            .collect();
        return Err(AppError::new("GIT_COMMAND", detail.trim()));
    }
    Ok(stdout)
}
impl GitService {
    pub fn status(workspace: &Workspace) -> Result<Value> {
        let output = run(workspace, &["status", "--porcelain=v1", "-z", "--branch"])?;
        let mut records = output
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty());
        let mut branch = String::new();
        let mut entries = Vec::new();
        while let Some(record) = records.next() {
            if record.starts_with(b"## ") {
                branch = String::from_utf8_lossy(&record[3..]).to_string();
                continue;
            }
            if record.len() < 4 {
                return Err(AppError::new(
                    "GIT_PROTOCOL",
                    "Git returned an invalid status record",
                ));
            }
            let path = String::from_utf8(record[3..].to_vec())
                .map_err(|_| AppError::new("PATH_ENCODING", "Git returned a non UTF-8 path"))?;
            let original = if matches!(record[0], b'R' | b'C') || matches!(record[1], b'R' | b'C') {
                Some(
                    String::from_utf8(
                        records
                            .next()
                            .ok_or_else(|| AppError::new("GIT_PROTOCOL", "Missing rename source"))?
                            .to_vec(),
                    )
                    .map_err(|_| AppError::new("PATH_ENCODING", "Git returned a non UTF-8 path"))?,
                )
            } else {
                None
            };
            entries.push(json!({"path":path,"index":(record[0] as char).to_string(),"worktree":(record[1] as char).to_string(),"originalPath":original}));
        }
        Ok(json!({"branch":branch,"entries":entries}))
    }
    pub fn diff(workspace: &Workspace, path: Option<&str>, staged: bool) -> Result<String> {
        if let Some(path) = path {
            workspace.resolve(path)?;
        }
        let mut args = vec![
            "--literal-pathspecs",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
        ];
        if staged {
            args.push("--cached");
        }
        args.push("--");
        if let Some(path) = path {
            args.push(path);
        }
        Ok(String::from_utf8_lossy(&run(workspace, &args)?).to_string())
    }
    pub fn history(workspace: &Workspace, path: Option<&str>) -> Result<Value> {
        if let Some(path) = path {
            workspace.resolve(path)?;
        }
        let mut args = vec![
            "--literal-pathspecs",
            "log",
            "-n",
            "100",
            "--no-show-signature",
            "--no-color",
            "--format=%H%x00%an%x00%aI%x00%s%x00",
            "--",
        ];
        if let Some(path) = path {
            args.push(path);
        }
        let output = run(workspace, &args)?;
        let mut fields = output.split(|byte| *byte == 0);
        let mut commits = Vec::new();
        while let Some(hash) = fields.next() {
            let hash = String::from_utf8_lossy(hash).trim().to_owned();
            if hash.is_empty() {
                continue;
            }
            let author = fields
                .next()
                .ok_or_else(|| AppError::new("GIT_PROTOCOL", "Missing commit author"))?;
            let date = fields
                .next()
                .ok_or_else(|| AppError::new("GIT_PROTOCOL", "Missing commit date"))?;
            let subject = fields
                .next()
                .ok_or_else(|| AppError::new("GIT_PROTOCOL", "Missing commit subject"))?;
            commits.push(json!({"hash":hash,"author":String::from_utf8_lossy(author),"date":String::from_utf8_lossy(date),"subject":String::from_utf8_lossy(subject)}));
        }
        Ok(json!(commits))
    }
    pub fn branches(workspace: &Workspace) -> Result<Vec<String>> {
        let output = run(workspace, &["branch", "--format=%(refname:short)"])?;
        Ok(String::from_utf8_lossy(&output)
            .lines()
            .map(str::to_owned)
            .collect())
    }
    pub fn action(workspace: &Workspace, action: &str, value: &str) -> Result<String> {
        if value.is_empty() || value.contains('\0') || value.len() > 16_384 {
            return Err(AppError::new("GIT_ARGUMENT", "Invalid Git action argument"));
        }
        let args = match action {
            "stage" => {
                workspace.resolve(value)?;
                vec!["--literal-pathspecs", "add", "--", value]
            }
            "unstage" => {
                workspace.resolve(value)?;
                vec!["--literal-pathspecs", "restore", "--staged", "--", value]
            }
            "checkout" => {
                Self::validate_branch(workspace, value)?;
                vec!["switch", "--", value]
            }
            "branch" => {
                Self::validate_branch(workspace, value)?;
                vec!["branch", "--", value]
            }
            "commit" => vec![
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--no-verify",
                "-m",
                value,
            ],
            _ => return Err(AppError::new("GIT_ACTION", "Unknown Git action")),
        };
        Ok(String::from_utf8_lossy(&run(workspace, &args)?).to_string())
    }
    fn validate_branch(workspace: &Workspace, value: &str) -> Result<()> {
        if value.starts_with('-') || value.contains(['\r', '\n']) {
            return Err(AppError::new("GIT_BRANCH", "Invalid branch name"));
        }
        run(workspace, &["check-ref-format", "--branch", value])?;
        Ok(())
    }
}
pub(crate) fn configure(command: &mut Command, workspace: &Workspace) -> Result<()> {
    command.arg("--work-tree").arg(&workspace.root);
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
    let config = run_inner(
        workspace,
        &[
            "config",
            "--includes",
            "--null",
            "--name-only",
            "--get-regexp",
            r"^filter\..*\.(clean|smudge|process|required)$",
        ],
        false,
        true,
        &AtomicBool::new(false),
    )?;
    let mut drivers = std::collections::BTreeSet::new();
    for key in config
        .split(|byte| *byte == 0)
        .filter(|key| !key.is_empty())
    {
        let key = std::str::from_utf8(key)
            .map_err(|_| AppError::new("GIT_CONFIG", "Git filter configuration is not UTF-8"))?;
        if let Some((driver, _)) = key.rsplit_once('.') {
            drivers.insert(driver.to_owned());
        }
    }
    for driver in drivers {
        for field in ["clean", "smudge", "process", "required"] {
            command.arg("-c").arg(format!(
                "{driver}.{field}={}",
                if field == "required" { "false" } else { "" }
            ));
        }
    }
    Ok(())
}
