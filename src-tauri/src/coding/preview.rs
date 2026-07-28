//! Desktop-only web preview and managed development-server support for local
//! coding sessions. Preview pages are deliberately isolated from Tauri IPC and
//! may navigate only to explicitly authorized loopback origins.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex, RwLock};
use std::time::Duration;
use tauri::{Emitter, Manager, WebviewUrl};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

const EVENT: &str = "coding-preview";
const MAX_LOG_LINES: usize = 500;
const EVAL_TIMEOUT: Duration = Duration::from_secs(10);

const INIT_SCRIPT: &str = r#"
(() => {
  const state = { logs: [], refs: new Map(), generation: 0 };
  Object.defineProperty(window, '__LOCALLMOS_PREVIEW__', { value: state, configurable: false });
  const stringify = (value) => {
    try {
      if (typeof value === 'string') return value;
      if (value instanceof Error) return `${value.name}: ${value.message}\n${value.stack || ''}`;
      return JSON.stringify(value);
    } catch (_) { return String(value); }
  };
  const record = (level, values) => {
    state.logs.push({ level, message: values.map(stringify).join(' '), timestamp: new Date().toISOString() });
    if (state.logs.length > 500) state.logs.splice(0, state.logs.length - 500);
  };
  for (const level of ['log', 'info', 'warn', 'error', 'debug']) {
    const original = console[level].bind(console);
    console[level] = (...values) => { record(level, values); original(...values); };
  }
  addEventListener('error', (event) => record('error', [`${event.message} at ${event.filename}:${event.lineno}:${event.colno}`]));
  addEventListener('unhandledrejection', (event) => record('unhandledrejection', [event.reason]));
})();
"#;

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewStatus {
    pub session_id: String,
    pub window_open: bool,
    pub url: Option<String>,
    pub server_state: String,
    pub server_command: Option<String>,
}

struct ServerProcess {
    child: Child,
    pid: u32,
    command: String,
    ready: bool,
    #[cfg(windows)]
    job: isize,
}

#[cfg(windows)]
impl Drop for ServerProcess {
    fn drop(&mut self) {
        if self.job != 0 {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(
                    self.job as windows_sys::Win32::Foundation::HANDLE,
                );
            }
            self.job = 0;
        }
    }
}

struct PreviewSession {
    label: String,
    url: Option<String>,
    authorized_origins: Arc<RwLock<HashSet<String>>>,
    server_logs: Arc<StdMutex<VecDeque<String>>>,
    server: Option<ServerProcess>,
}

impl PreviewSession {
    fn new(session_id: &str) -> Self {
        Self {
            label: format!("preview-{session_id}"),
            url: None,
            authorized_origins: Arc::new(RwLock::new(HashSet::new())),
            server_logs: Arc::new(StdMutex::new(VecDeque::new())),
            server: None,
        }
    }
}

pub struct PreviewManager {
    sessions: Mutex<HashMap<String, PreviewSession>>,
    http: reqwest::Client,
}

impl PreviewManager {
    pub fn new(http: reqwest::Client) -> Arc<Self> {
        // This client is used only after loopback validation. A short timeout
        // keeps the readiness deadline meaningful; accepting a local dev cert
        // makes HTTPS preview servers usable without weakening the app's
        // general-purpose HTTP client.
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(1))
            .timeout(Duration::from_secs(2))
            .danger_accept_invalid_certs(true)
            .build()
            .unwrap_or(http);
        Arc::new(Self {
            sessions: Mutex::new(HashMap::new()),
            http,
        })
    }

    pub fn validate_url(raw: &str) -> Result<tauri::Url> {
        let url: tauri::Url = raw.parse().map_err(|e| anyhow!("invalid preview URL: {e}"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(anyhow!("preview URL must use http or https"));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(anyhow!("preview URL must not contain credentials"));
        }
        let host = url.host().ok_or_else(|| anyhow!("preview URL has no host"))?;
        let loopback = match host {
            url::Host::Domain(name) => name.eq_ignore_ascii_case("localhost"),
            url::Host::Ipv4(ip) => ip.is_loopback(),
            url::Host::Ipv6(ip) => ip.is_loopback(),
        };
        if !loopback {
            return Err(anyhow!("preview URL must use localhost or a loopback IP address"));
        }
        url.port_or_known_default().ok_or_else(|| anyhow!("preview URL has no valid port"))?;
        Ok(url)
    }

    fn origin(url: &tauri::Url) -> Result<String> {
        let host = url.host_str().ok_or_else(|| anyhow!("preview URL has no host"))?;
        let host = if host.contains(':') { format!("[{host}]") } else { host.to_ascii_lowercase() };
        let port = url.port_or_known_default().ok_or_else(|| anyhow!("preview URL has no port"))?;
        Ok(format!("{}://{}:{}", url.scheme(), host, port))
    }

    pub async fn needs_authorization(&self, session_id: &str, raw_url: &str) -> Result<Option<String>> {
        let url = Self::validate_url(raw_url)?;
        let origin = Self::origin(&url)?;
        let sessions = self.sessions.lock().await;
        let allowed = sessions
            .get(session_id)
            .and_then(|s| s.authorized_origins.read().ok().map(|set| set.contains(&origin)))
            .unwrap_or(false);
        Ok((!allowed).then_some(origin))
    }

    pub async fn authorize(&self, session_id: &str, origin: String) {
        let mut sessions = self.sessions.lock().await;
        let session = sessions.entry(session_id.to_string()).or_insert_with(|| PreviewSession::new(session_id));
        if let Ok(mut allowed) = session.authorized_origins.write() {
            allowed.insert(origin);
        };
    }

    pub async fn status(&self, app: &tauri::AppHandle, session_id: &str) -> PreviewStatus {
        let sessions = self.sessions.lock().await;
        let Some(session) = sessions.get(session_id) else {
            return PreviewStatus { session_id: session_id.into(), server_state: "stopped".into(), ..Default::default() };
        };
        PreviewStatus {
            session_id: session_id.into(),
            window_open: app.get_webview_window(&session.label).is_some(),
            url: session.url.clone(),
            server_state: session.server.as_ref().map(|s| if s.ready { "ready" } else { "starting" }).unwrap_or("stopped").into(),
            server_command: session.server.as_ref().map(|s| s.command.clone()),
        }
    }

    async fn emit_status(&self, app: &tauri::AppHandle, session_id: &str) {
        let _ = app.emit(EVENT, self.status(app, session_id).await);
    }

    pub async fn open(self: &Arc<Self>, app: &tauri::AppHandle, session_id: &str, raw_url: &str, width: u32, height: u32) -> Result<String> {
        let url = Self::validate_url(raw_url)?;
        let origin = Self::origin(&url)?;
        let (label, allowed) = {
            let mut sessions = self.sessions.lock().await;
            let session = sessions.entry(session_id.into()).or_insert_with(|| PreviewSession::new(session_id));
            let allowed = session.authorized_origins.read().map(|s| s.contains(&origin)).unwrap_or(false);
            session.url = Some(url.to_string());
            (session.label.clone(), allowed)
        };
        if !allowed {
            return Err(anyhow!("preview origin has not been approved: {origin}"));
        }

        if let Some(window) = app.get_webview_window(&label) {
            window.navigate(url.clone())?;
            window.set_size(tauri::LogicalSize::new(width, height))?;
            window.show()?;
            window.set_focus()?;
        } else {
            let authorized = {
                let sessions = self.sessions.lock().await;
                sessions.get(session_id).unwrap().authorized_origins.clone()
            };
            let nav_allowed = authorized.clone();
            let manager = self.clone();
            let app_for_load = app.clone();
            let sid = session_id.to_string();
            tauri::WebviewWindowBuilder::new(app, &label, WebviewUrl::External(url.clone()))
                .title(format!("Web Preview — {origin}"))
                .inner_size(width as f64, height as f64)
                .resizable(true)
                .initialization_script(INIT_SCRIPT)
                .on_navigation(move |next| {
                    let Ok(origin) = Self::origin(next) else { return false };
                    nav_allowed.read().map(|set| set.contains(&origin)).unwrap_or(false)
                })
                .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
                .on_download(|_, _| false)
                .on_page_load(move |_, payload| {
                    if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                        let manager = manager.clone();
                        let app = app_for_load.clone();
                        let sid = sid.clone();
                        let loaded = payload.url().to_string();
                        tauri::async_runtime::spawn(async move {
                            if let Some(session) = manager.sessions.lock().await.get_mut(&sid) {
                                session.url = Some(loaded);
                            }
                            manager.emit_status(&app, &sid).await;
                        });
                    }
                })
                .build()?;
        }
        self.emit_status(app, session_id).await;
        Ok(format!("Opened preview at {url} ({width}x{height})."))
    }

    pub async fn focus(&self, app: &tauri::AppHandle, session_id: &str) -> Result<()> {
        let label = self.label(session_id).await?;
        let window = app.get_webview_window(&label).ok_or_else(|| anyhow!("preview is not open"))?;
        window.show()?;
        window.set_focus()?;
        Ok(())
    }

    pub async fn reload(&self, app: &tauri::AppHandle, session_id: &str) -> Result<String> {
        let window = self.window(app, session_id).await?;
        window.reload()?;
        Ok("Reloaded preview.".into())
    }

    pub async fn resize(&self, app: &tauri::AppHandle, session_id: &str, width: u32, height: u32) -> Result<String> {
        if !(320..=3840).contains(&width) || !(240..=2160).contains(&height) {
            return Err(anyhow!("preview size must be between 320x240 and 3840x2160"));
        }
        let window = self.window(app, session_id).await?;
        window.set_size(tauri::LogicalSize::new(width, height))?;
        Ok(format!("Resized preview to {width}x{height}."))
    }

    pub async fn snapshot(&self, app: &tauri::AppHandle, session_id: &str, selector: Option<&str>) -> Result<String> {
        let selector = serde_json::to_string(selector.unwrap_or("body"))?;
        let script = format!(r#"(() => {{
          const state = window.__LOCALLMOS_PREVIEW__;
          if (!state) return JSON.stringify({{error:'preview instrumentation is unavailable'}});
          const root = document.querySelector({selector});
          if (!root) return JSON.stringify({{error:'selector not found'}});
          state.refs = new Map(); state.generation += 1;
          const visible = (el) => {{ const r=el.getBoundingClientRect(), s=getComputedStyle(el); return r.width>0 && r.height>0 && s.visibility!=='hidden' && s.display!=='none'; }};
          const nodes = [...root.querySelectorAll('a,button,input,textarea,select,[role],[tabindex]')].filter(visible).slice(0,200);
          const elements = nodes.map((el, i) => {{
            const ref = `e${{i+1}}`; state.refs.set(ref, el);
            const role = el.getAttribute('role') || (el.tagName==='A'?'link':el.tagName==='BUTTON'?'button':el.tagName==='INPUT'?'input':el.tagName.toLowerCase());
            const name = el.getAttribute('aria-label') || el.getAttribute('title') || el.labels?.[0]?.innerText || el.innerText || el.value || el.placeholder || '';
            return {{ref, role, name:String(name).trim().slice(0,240), disabled:!!el.disabled, checked:typeof el.checked==='boolean'?el.checked:undefined}};
          }});
          const text = String(root.innerText || '').replace(/\s+/g,' ').trim().slice(0,12000);
          return JSON.stringify({{url:location.href,title:document.title,generation:state.generation,text,elements}});
        }})()"#);
        self.eval(app, session_id, script).await
    }

    pub async fn click(&self, app: &tauri::AppHandle, session_id: &str, element_ref: &str) -> Result<String> {
        let r = serde_json::to_string(element_ref)?;
        let script = format!(r#"(() => {{ const s=window.__LOCALLMOS_PREVIEW__, el=s?.refs?.get({r}); if(!el||!el.isConnected) return JSON.stringify({{error:'stale or unknown ref; take a new snapshot'}}); el.scrollIntoView({{block:'center'}}); el.click(); return JSON.stringify({{ok:true,ref:{r}}}); }})()"#);
        self.eval(app, session_id, script).await
    }

    pub async fn fill(&self, app: &tauri::AppHandle, session_id: &str, element_ref: &str, text: &str, submit: bool) -> Result<String> {
        let r = serde_json::to_string(element_ref)?;
        let value = serde_json::to_string(text)?;
        let script = format!(r#"(() => {{ const s=window.__LOCALLMOS_PREVIEW__, el=s?.refs?.get({r}); if(!el||!el.isConnected) return JSON.stringify({{error:'stale or unknown ref; take a new snapshot'}}); el.focus(); if(el.tagName==='SELECT') el.value={value}; else {{ const setter=Object.getOwnPropertyDescriptor(el.tagName==='TEXTAREA'?HTMLTextAreaElement.prototype:HTMLInputElement.prototype,'value')?.set; setter?setter.call(el,{value}):el.value={value}; }} el.dispatchEvent(new Event('input',{{bubbles:true}})); el.dispatchEvent(new Event('change',{{bubbles:true}})); if({submit}) el.form?.requestSubmit(); return JSON.stringify({{ok:true,ref:{r},value:el.value}}); }})()"#);
        self.eval(app, session_id, script).await
    }

    pub async fn press(&self, app: &tauri::AppHandle, session_id: &str, element_ref: &str, key: &str) -> Result<String> {
        let r = serde_json::to_string(element_ref)?;
        let key = serde_json::to_string(key)?;
        let script = format!(r#"(() => {{ const s=window.__LOCALLMOS_PREVIEW__, el=s?.refs?.get({r}); if(!el||!el.isConnected) return JSON.stringify({{error:'stale or unknown ref; take a new snapshot'}}); el.focus(); el.dispatchEvent(new KeyboardEvent('keydown',{{key:{key},bubbles:true}})); el.dispatchEvent(new KeyboardEvent('keyup',{{key:{key},bubbles:true}})); if({key}==='Enter') el.form?.requestSubmit(); return JSON.stringify({{ok:true,ref:{r},key:{key}}}); }})()"#);
        self.eval(app, session_id, script).await
    }

    pub async fn console(&self, app: &tauri::AppHandle, session_id: &str, clear: bool) -> Result<String> {
        let script = format!(r#"(() => {{ const s=window.__LOCALLMOS_PREVIEW__; if(!s) return JSON.stringify({{error:'preview instrumentation is unavailable'}}); const logs=s.logs.slice(); if({clear}) s.logs.length=0; return JSON.stringify({{url:location.href,logs}}); }})()"#);
        self.eval(app, session_id, script).await
    }

    async fn eval(&self, app: &tauri::AppHandle, session_id: &str, script: String) -> Result<String> {
        let window = self.window(app, session_id).await?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sender = Arc::new(StdMutex::new(Some(tx)));
        window.eval_with_callback(script, move |result| {
            if let Some(tx) = sender.lock().ok().and_then(|mut s| s.take()) {
                let _ = tx.send(result);
            }
        })?;
        let raw = tokio::time::timeout(EVAL_TIMEOUT, rx).await.context("preview evaluation timed out")??;
        // Tauri serializes the JS result. Our scripts return JSON strings, so
        // unwrap that outer JSON string before returning compact model content.
        match serde_json::from_str::<Value>(&raw) {
            Ok(Value::String(inner)) => Ok(inner),
            Ok(value) => Ok(value.to_string()),
            Err(_) => Ok(raw),
        }
    }

    pub async fn start_server(self: &Arc<Self>, app: &tauri::AppHandle, session_id: &str, workspace: &Path, command: &str, raw_url: &str, timeout: Duration) -> Result<String> {
        let url = Self::validate_url(raw_url)?;
        self.stop_server(app, session_id).await.ok();

        let mut cmd = if cfg!(target_os = "windows") {
            let mut cmd = Command::new("cmd");
            cmd.args(["/C", command]);
            cmd
        } else {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", command]);
            cmd
        };
        cmd.current_dir(workspace).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.as_std_mut().process_group(0);
        }
        let mut child = cmd.spawn().context("failed to start development server")?;
        let pid = child.id().ok_or_else(|| anyhow!("development server has no process id"))?;
        #[cfg(windows)]
        let job = match create_kill_on_close_job(pid) {
            Ok(job) => job,
            Err(error) => {
                child.kill().await.ok();
                return Err(error);
            }
        };
        let logs = {
            let mut sessions = self.sessions.lock().await;
            let session = sessions.entry(session_id.into()).or_insert_with(|| PreviewSession::new(session_id));
            if let Ok(mut existing) = session.server_logs.lock() { existing.clear(); }
            session.server_logs.clone()
        };
        if let Some(stdout) = child.stdout.take() { spawn_log_reader(stdout, logs.clone(), "stdout"); }
        if let Some(stderr) = child.stderr.take() { spawn_log_reader(stderr, logs.clone(), "stderr"); }
        {
            let mut sessions = self.sessions.lock().await;
            let session = sessions.entry(session_id.into()).or_insert_with(|| PreviewSession::new(session_id));
            session.server = Some(ServerProcess {
                child, pid, command: command.into(), ready: false,
                #[cfg(windows)]
                job,
            });
            session.url = Some(url.to_string());
        }
        self.emit_status(app, session_id).await;

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.http.get(url.clone()).send().await.is_ok() {
                if let Some(server) = self.sessions.lock().await.get_mut(session_id).and_then(|s| s.server.as_mut()) {
                    server.ready = true;
                }
                self.emit_status(app, session_id).await;
                self.spawn_server_monitor(app.clone(), session_id.to_string());
                return Ok(format!("Development server is ready at {url}."));
            }
            let exited = {
                let mut sessions = self.sessions.lock().await;
                sessions.get_mut(session_id).and_then(|s| s.server.as_mut()).and_then(|s| s.child.try_wait().ok().flatten())
            };
            if let Some(status) = exited {
                let logs = self.server_logs(session_id, false).await.unwrap_or_default();
                self.stop_server(app, session_id).await.ok();
                return Err(anyhow!("development server exited with {status}\n{logs}"));
            }
            if tokio::time::Instant::now() >= deadline {
                let logs = self.server_logs(session_id, false).await.unwrap_or_default();
                self.stop_server(app, session_id).await.ok();
                return Err(anyhow!("development server did not become ready within {}s\n{logs}", timeout.as_secs()));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn server_logs(&self, session_id: &str, clear: bool) -> Result<String> {
        let sessions = self.sessions.lock().await;
        let session = sessions.get(session_id).ok_or_else(|| anyhow!("development server has not been started"))?;
        let mut logs = session.server_logs.lock().map_err(|_| anyhow!("server log buffer is unavailable"))?;
        let text = logs.iter().cloned().collect::<Vec<_>>().join("\n");
        if clear { logs.clear(); }
        Ok(if text.is_empty() { "(no server output)".into() } else { text })
    }

    fn spawn_server_monitor(self: &Arc<Self>, app: tauri::AppHandle, session_id: String) {
        let manager = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let exited = {
                    let mut sessions = manager.sessions.lock().await;
                    let Some(session) = sessions.get_mut(&session_id) else { return };
                    let Some(server) = session.server.as_mut() else { return };
                    match server.child.try_wait() {
                        Ok(Some(_)) => { session.server.take(); true }
                        Ok(None) => false,
                        Err(_) => { session.server.take(); true }
                    }
                };
                if exited {
                    manager.emit_status(&app, &session_id).await;
                    return;
                }
            }
        });
    }

    pub async fn stop_server(&self, app: &tauri::AppHandle, session_id: &str) -> Result<String> {
        let server = {
            let mut sessions = self.sessions.lock().await;
            sessions.get_mut(session_id).and_then(|s| s.server.take())
        };
        if let Some(mut server) = server {
            kill_process_tree(&mut server).await;
            self.emit_status(app, session_id).await;
            Ok("Stopped development server.".into())
        } else {
            Ok("Development server was already stopped.".into())
        }
    }

    pub async fn close_session(&self, app: &tauri::AppHandle, session_id: &str, close_window: bool) -> Result<()> {
        let mut session = self.sessions.lock().await.remove(session_id);
        if let Some(ref mut session) = session {
            if let Some(ref mut server) = session.server { kill_process_tree(server).await; }
            if close_window {
                if let Some(window) = app.get_webview_window(&session.label) { window.close().ok(); }
            }
        }
        let _ = app.emit(EVENT, PreviewStatus { session_id: session_id.into(), server_state: "stopped".into(), ..Default::default() });
        Ok(())
    }

    pub async fn stop_all(&self, app: &tauri::AppHandle) {
        let ids: Vec<String> = self.sessions.lock().await.keys().cloned().collect();
        for id in ids { self.close_session(app, &id, true).await.ok(); }
    }

    pub async fn session_for_window(&self, label: &str) -> Option<String> {
        self.sessions.lock().await.iter().find_map(|(id, s)| (s.label == label).then(|| id.clone()))
    }

    async fn label(&self, session_id: &str) -> Result<String> {
        self.sessions.lock().await.get(session_id).map(|s| s.label.clone()).ok_or_else(|| anyhow!("preview is not open"))
    }

    async fn window(&self, app: &tauri::AppHandle, session_id: &str) -> Result<tauri::WebviewWindow> {
        let label = self.label(session_id).await?;
        app.get_webview_window(&label).ok_or_else(|| anyhow!("preview is not open"))
    }
}

fn spawn_log_reader<R>(reader: R, logs: Arc<StdMutex<VecDeque<String>>>, stream: &'static str)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(mut logs) = logs.lock() {
                logs.push_back(format!("[{stream}] {line}"));
                while logs.len() > MAX_LOG_LINES { logs.pop_front(); }
            }
        }
    });
}

async fn kill_process_tree(server: &mut ServerProcess) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(server.pid as i32), libc::SIGTERM);
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
        if server.job != 0 {
            CloseHandle(server.job as HANDLE);
            server.job = 0;
        }
    }
    if tokio::time::timeout(Duration::from_secs(3), server.child.wait()).await.is_err() {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(server.pid as i32), libc::SIGKILL);
        }
        server.child.kill().await.ok();
        server.child.wait().await.ok();
    }
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
            if process != 0 as HANDLE { CloseHandle(process); }
            CloseHandle(job);
            return Err(error.into());
        }
        CloseHandle(process);
        Ok(job as isize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_loopback_http_urls() {
        for url in ["http://localhost:3000", "https://127.0.0.1:4443/a", "http://[::1]:8080"] {
            assert!(PreviewManager::validate_url(url).is_ok(), "rejected {url}");
        }
        for url in ["https://example.com", "file:///tmp/a.html", "http://user:pass@localhost:3000"] {
            assert!(PreviewManager::validate_url(url).is_err(), "accepted {url}");
        }
    }

    #[test]
    fn origins_include_effective_ports() {
        let http = PreviewManager::validate_url("http://LOCALHOST/path").unwrap();
        let https = PreviewManager::validate_url("https://localhost/path").unwrap();
        assert_eq!(PreviewManager::origin(&http).unwrap(), "http://localhost:80");
        assert_eq!(PreviewManager::origin(&https).unwrap(), "https://localhost:443");
    }

    #[tokio::test]
    async fn authorization_is_scoped_to_session_and_memory() {
        let manager = PreviewManager::new(reqwest::Client::new());
        let url = "http://localhost:5173/app";
        let origin = manager.needs_authorization("one", url).await.unwrap().unwrap();
        manager.authorize("one", origin).await;
        assert_eq!(manager.needs_authorization("one", url).await.unwrap(), None);
        assert!(manager.needs_authorization("two", url).await.unwrap().is_some());
        manager.sessions.lock().await.remove("one");
        assert!(manager.needs_authorization("one", url).await.unwrap().is_some());
    }
}
