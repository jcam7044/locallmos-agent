//! Child-process (stdio) MCP transport.
//!
//! The process lifecycle — own process group on unix, kill-on-close job object on
//! windows, stderr ring buffer, graceful-then-forceful teardown — is lifted from
//! `coding::preview`, which already solves it correctly on every platform. What
//! is new here is the framed request/response layer: a single reader task owns
//! `stdout` (which carries JSON-RPC and is never logged), correlating each
//! response to the request that is awaiting it via a `pending` map. `stdout`
//! closing (the process died) drains every in-flight request with an error, so a
//! crashed server surfaces immediately instead of hanging until timeout.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};

use super::protocol::{self, Incoming, McpTool, RpcError};

const MAX_LOG_LINES: usize = 500;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(20);

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// A live connection to one stdio MCP server.
pub struct StdioClient {
    child: Mutex<Child>,
    pid: u32,
    #[cfg(windows)]
    job: isize,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    next_id: AtomicI64,
    logs: Arc<StdMutex<VecDeque<String>>>,
    /// Set when the server sends `notifications/tools/list_changed`; the manager
    /// consumes it to know it should re-list and rebuild the snapshot.
    tools_changed: Arc<AtomicBool>,
}

impl StdioClient {
    /// Spawn `command args...` with `env`, perform the MCP handshake, and return
    /// a ready client. Fails (killing the child) if the handshake times out.
    pub async fn connect(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
        cwd: Option<&str>,
        client_version: &str,
    ) -> Result<Arc<Self>> {
        // On Windows, `npx`/`uvx` resolve to `.cmd` shims that CreateProcess
        // cannot launch directly; route through `cmd /C`. On unix, spawn the
        // program directly so the process group covers the whole tree.
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.arg("/C").arg(command).args(args);
            c
        } else {
            let mut c = Command::new(command);
            c.args(args);
            c
        };
        cmd.envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.as_std_mut().process_group(0);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to start MCP server '{command}'"))?;
        let pid = child.id().ok_or_else(|| anyhow!("MCP server has no process id"))?;
        #[cfg(windows)]
        let job = match create_kill_on_close_job(pid) {
            Ok(job) => job,
            Err(e) => {
                child.kill().await.ok();
                return Err(e);
            }
        };

        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout pipe"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr pipe"))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin pipe"))?;

        let logs: Arc<StdMutex<VecDeque<String>>> = Arc::new(StdMutex::new(VecDeque::new()));
        spawn_stderr_reader(stderr, logs.clone());

        let stdin = Arc::new(Mutex::new(stdin));
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let tools_changed = Arc::new(AtomicBool::new(false));
        spawn_stdout_reader(stdout, pending.clone(), stdin.clone(), tools_changed.clone());

        let client = Arc::new(Self {
            child: Mutex::new(child),
            pid,
            #[cfg(windows)]
            job,
            stdin,
            pending,
            next_id: AtomicI64::new(1),
            logs,
            tools_changed,
        });

        if let Err(e) = client.handshake(client_version).await {
            client.shutdown().await;
            let tail = client.log_tail();
            return Err(anyhow!("MCP handshake failed: {e}{}", format_tail(&tail)));
        }
        Ok(client)
    }

    async fn handshake(&self, client_version: &str) -> Result<()> {
        let params = protocol::initialize_params(client_version);
        let result = self
            .request("initialize", Some(params), HANDSHAKE_TIMEOUT)
            .await
            .context("initialize request failed")?;
        let server_protocol =
            result.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
        tracing::info!("MCP server initialized (protocol {server_protocol})");
        // The spec requires the client to confirm readiness before any other
        // request. tools/list etc. happen from the manager after connect returns.
        self.notify("notifications/initialized", None).await?;
        Ok(())
    }

    /// Send a request and await its response, bounded by `timeout`.
    pub async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let frame = protocol::Outgoing::request(id, method, params).to_frame();
        if let Err(e) = self.write_frame(&frame).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(rpc))) => Err(anyhow!("{method} failed: {rpc}")),
            // Sender dropped: the reader task drained pending because stdout closed.
            Ok(Err(_)) => Err(anyhow!("{method} failed: server connection closed")),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(anyhow!("{method} timed out after {}s", timeout.as_secs()))
            }
        }
    }

    async fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let frame = protocol::Outgoing::notification(method, params).to_frame();
        self.write_frame(&frame).await
    }

    async fn write_frame(&self, frame: &str) -> Result<()> {
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(frame.as_bytes()).await.context("writing to MCP server stdin")?;
        stdin.flush().await.context("flushing MCP server stdin")?;
        Ok(())
    }

    /// True if the child process has not exited.
    pub async fn is_alive(&self) -> bool {
        matches!(self.child.lock().await.try_wait(), Ok(None))
    }

    /// Take-and-clear the `tools/list_changed` flag.
    pub fn take_tools_changed(&self) -> bool {
        self.tools_changed.swap(false, Ordering::Relaxed)
    }

    pub fn log_tail(&self) -> String {
        self.logs
            .lock()
            .map(|l| l.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default()
    }

    /// Terminate the whole process tree, gracefully then forcefully.
    pub async fn shutdown(&self) {
        let mut child = self.child.lock().await;
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.pid as i32), libc::SIGTERM);
        }
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
            if self.job != 0 {
                CloseHandle(self.job as HANDLE);
            }
        }
        if tokio::time::timeout(Duration::from_secs(3), child.wait()).await.is_err() {
            #[cfg(unix)]
            unsafe {
                libc::kill(-(self.pid as i32), libc::SIGKILL);
            }
            child.kill().await.ok();
            child.wait().await.ok();
        }
    }
}

/// Paginate `tools/list` fully, honoring the server's `nextCursor`. Bounded so a
/// misbehaving server cannot loop forever.
pub async fn list_all_tools(client: &StdioClient, call_timeout: Duration) -> Result<Vec<McpTool>> {
    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..64 {
        let params = protocol::tools_list_params(cursor.as_deref());
        let value = client.request("tools/list", params, call_timeout).await?;
        let page: protocol::ToolsListResult =
            serde_json::from_value(value).context("tools/list result malformed")?;
        tools.extend(page.tools);
        match page.next_cursor {
            Some(c) if !c.is_empty() => cursor = Some(c),
            _ => return Ok(tools),
        }
    }
    Ok(tools)
}

fn format_tail(tail: &str) -> String {
    if tail.trim().is_empty() {
        String::new()
    } else {
        format!("\n--- server stderr ---\n{tail}")
    }
}

fn spawn_stderr_reader<R>(reader: R, logs: Arc<StdMutex<VecDeque<String>>>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(mut logs) = logs.lock() {
                logs.push_back(line);
                while logs.len() > MAX_LOG_LINES {
                    logs.pop_front();
                }
            }
        }
    });
}

fn spawn_stdout_reader<R>(
    reader: R,
    pending: Pending,
    stdin: Arc<Mutex<ChildStdin>>,
    tools_changed: Arc<AtomicBool>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            match protocol::parse_incoming(&line) {
                Some(Incoming::Response { id, result }) => {
                    if let Some(tx) = pending.lock().await.remove(&id) {
                        let _ = tx.send(result);
                    }
                }
                Some(Incoming::Notification { method, .. }) => {
                    if method == "notifications/tools/list_changed" {
                        tools_changed.store(true, Ordering::Relaxed);
                    }
                }
                Some(Incoming::ServerRequest { id, .. }) => {
                    // We advertise no capabilities, so reject server-initiated
                    // requests cleanly instead of leaving the server waiting.
                    let reply = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": protocol::METHOD_NOT_FOUND, "message": "not supported" }
                    });
                    let frame = format!("{reply}\n");
                    let _ = stdin.lock().await.write_all(frame.as_bytes()).await;
                }
                None => {
                    tracing::debug!("ignoring unparseable MCP frame: {line}");
                }
            }
        }
        // stdout closed → the process is gone. Fail every waiter now rather than
        // letting each one burn its full timeout.
        for (_, tx) in pending.lock().await.drain() {
            let _ = tx.send(Err(RpcError {
                code: 0,
                message: "server connection closed".into(),
                data: None,
            }));
        }
    });
}

#[cfg(windows)]
fn create_kill_on_close_job(pid: u32) -> Result<isize> {
    use std::ffi::c_void;
    use std::mem::{size_of, zeroed};
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

    unsafe {
        let job = CreateJobObjectW(null_mut(), std::ptr::null());
        if job == 0 as HANDLE || job == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
        {
            let error = std::io::Error::last_os_error();
            CloseHandle(job);
            return Err(error.into());
        }
        let process: HANDLE = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
        if process == 0 as HANDLE || AssignProcessToJobObject(job, process) == 0 {
            let error = std::io::Error::last_os_error();
            if process != 0 as HANDLE {
                CloseHandle(process);
            }
            CloseHandle(job);
            return Err(error.into());
        }
        CloseHandle(process);
        Ok(job as isize)
    }
}
