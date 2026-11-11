//! Shell command runner for User Tools integration.
//!
//! Executes external commands, captures stdout/stderr, and provides
//! click-to-jump error line parsing for compiler output. This replaces
//! the anti-feature "compiler integration" with a more general-purpose
//! user-configurable tool runner.
//!
//! User tools are defined in `~/.brouillon/tools.toml`:
//! ```toml
//! [[tool]]
//! name = "Cargo Build"
//! command = "cargo"
//! args = ["build"]
//! capture = true
//! shortcut = "ctrl+1"
//!
//! [[tool]]
//! name = "Run tests"
//! command = "cargo"
//! args = ["test"]
//! capture = true
//! shortcut = "ctrl+2"
//! ```

use crate::error_patterns::Stream;
use crate::tool_vars::{self, Quote, VarContext, VarError};
use crate::tools_config::{OutputSink, SavePolicy, ToolDef};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

/// Result of a shell command execution.
#[derive(Debug, Clone)]
pub struct CommandResult {
    /// The command that was run (for display).
    pub command_line: String,
    /// Exit code (None if the process was killed/crashed).
    pub exit_code: Option<i32>,
    /// Combined stdout content.
    pub stdout: String,
    /// Combined stderr content.
    pub stderr: String,
    /// Wall-clock execution time.
    pub duration: Duration,
    /// Whether the command succeeded (exit code 0).
    pub success: bool,
}

/// A parsed error location from compiler output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorLocation {
    /// File path mentioned in the error.
    pub file: String,
    /// Line number (1-based).
    pub line: usize,
    /// Column number (1-based, 0 if unknown).
    pub column: usize,
    /// Error message text.
    pub message: String,
}

/// A user-defined tool configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct UserTool {
    /// Display name in the Tools menu.
    pub name: String,
    /// Executable command.
    pub command: String,
    /// Command arguments.
    #[serde(default)]
    pub args: Vec<String>,
    /// Whether to capture output in the output pane.
    #[serde(default = "default_true")]
    pub capture: bool,
    /// Optional keyboard shortcut (e.g. "ctrl+1").
    #[serde(default)]
    pub shortcut: String,
    /// Optional working directory. Empty = use current file's directory.
    #[serde(default)]
    pub cwd: String,
}

impl UserTool {
    /// Resolve the working directory as a `PathBuf`.
    /// Returns `None` if `cwd` is empty (caller should use default).
    pub fn cwd_path(&self) -> Option<PathBuf> {
        if self.cwd.is_empty() {
            None
        } else {
            Some(PathBuf::from(&self.cwd))
        }
    }
}

fn default_true() -> bool { true }

/// TOML wrapper for deserialization.
#[derive(Debug, serde::Deserialize)]
struct ToolsFile {
    #[serde(default)]
    tool: Vec<UserTool>,
}

/// Run a command synchronously and capture its output.
pub fn run_command(command: &str, args: &[String], cwd: Option<&Path>) -> CommandResult {
    let start = Instant::now();
    let command_line = format!("{} {}", command, args.join(" "));

    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    let result: Result<Output, _> = cmd.output();
    match result {
        Ok(output) => {
            let duration = start.elapsed();
            let exit_code = output.status.code();
            CommandResult {
                command_line,
                exit_code,
                stdout: String::from_utf8_lossy(&output.stdout).to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).to_string(),
                duration,
                success: output.status.success(),
            }
        }
        Err(e) => {
            CommandResult {
                command_line,
                exit_code: None,
                stdout: String::new(),
                stderr: format!("Failed to execute command: {}", e),
                duration: start.elapsed(),
                success: false,
            }
        }
    }
}

/// Parse error locations from compiler output (Rust, GCC, Clang, Python format).
///
/// Recognizes patterns like:
/// - `file.rs:12:5: error: message` (Rust/GCC/Clang)
/// - `File "file.py", line 12` (Python)
/// - `file.js:12` (generic)
pub fn parse_error_locations(output: &str) -> Vec<ErrorLocation> {
    let mut locations = Vec::new();

    for line in output.lines() {
        // Rust/GCC/Clang: `path:line:col: level: message`
        if let Some(loc) = parse_gcc_style(line) {
            locations.push(loc);
            continue;
        }
        // Python: `File "path", line N`
        if let Some(loc) = parse_python_style(line) {
            locations.push(loc);
            continue;
        }
    }

    locations
}

/// Parse `path:line:col: message` style errors.
fn parse_gcc_style(line: &str) -> Option<ErrorLocation> {
    // Find the first colon-separated triple that looks like file:line:col
    let parts: Vec<&str> = line.splitn(4, ':').collect();
    if parts.len() >= 3 {
        let file = parts[0].trim();
        if let Ok(line_num) = parts[1].trim().parse::<usize>() {
            let column = parts[2].trim().parse::<usize>().unwrap_or(0);
            let message = if parts.len() >= 4 {
                parts[3].trim().to_string()
            } else {
                String::new()
            };
            // Heuristic: file must contain a dot or slash to look like a path
            if file.contains('.') || file.contains('/') || file.contains('\\') {
                return Some(ErrorLocation {
                    file: file.to_string(),
                    line: line_num,
                    column,
                    message,
                });
            }
        }
    }
    None
}

/// Parse Python-style `File "path", line N` errors.
fn parse_python_style(line: &str) -> Option<ErrorLocation> {
    if let Some(start) = line.find("File \"") {
        let rest = &line[start + 6..];
        if let Some(end_quote) = rest.find('"') {
            let file = &rest[..end_quote];
            let after_quote = &rest[end_quote + 1..];
            if let Some(line_start) = after_quote.find("line ") {
                let num_str: String = after_quote[line_start + 5..]
                    .chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(line_num) = num_str.parse::<usize>() {
                    return Some(ErrorLocation {
                        file: file.to_string(),
                        line: line_num,
                        column: 0,
                        message: String::new(),
                    });
                }
            }
        }
    }
    None
}

/// Load user tools from `~/.brouillon/tools.toml`.
pub fn load_user_tools() -> Vec<UserTool> {
    let path = match dirs::home_dir() {
        Some(h) => h.join(".brouillon").join("tools.toml"),
        None => return Vec::new(),
    };
    load_tools_from(&path)
}

/// Load tools from a specific TOML file.
pub fn load_tools_from(path: &Path) -> Vec<UserTool> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    parse_tools_toml(&content)
}

/// Parse tools from TOML content.
pub fn parse_tools_toml(content: &str) -> Vec<UserTool> {
    match toml::from_str::<ToolsFile>(content) {
        Ok(f) => f.tool,
        Err(_) => Vec::new(),
    }
}

// ---------------------------------------------------------------------
// Lot 9: streaming runner, variables, save-before-run, confirmation gate.
// ---------------------------------------------------------------------

/// Who asks for the execution. Running a tool is arbitrary command
/// execution, so anything that is not the user typing a keystroke (a future
/// agent / MCP / plugin caller) must carry an explicit confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requester {
    User,
    Agent { confirmed: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// An agent asked without explicit user confirmation; nothing was run.
    ConfirmationRequired,
    Var(VarError),
    /// The save-before-run step failed; nothing was run.
    SaveFailed(String),
    Spawn(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConfirmationRequired => write!(f, "running a tool requires explicit user confirmation"),
            Self::Var(e) => write!(f, "{e}"),
            Self::SaveFailed(e) => write!(f, "save before run failed: {e}"),
            Self::Spawn(e) => write!(f, "cannot start process: {e}"),
        }
    }
}
impl std::error::Error for RunError {}

impl From<VarError> for RunError {
    fn from(e: VarError) -> Self {
        Self::Var(e)
    }
}

/// A fully expanded invocation.
#[derive(Debug, Clone, Default)]
pub struct RunSpec {
    /// Direct execution: program + args (no shell involved).
    pub program: String,
    pub args: Vec<String>,
    /// Shell execution: a ready command line whose substituted values are
    /// already quoted. Takes precedence over `program`/`args`.
    pub shell_line: Option<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub stdin: Option<String>,
}

impl RunSpec {
    pub fn display(&self) -> String {
        match &self.shell_line {
            Some(l) => l.clone(),
            None => format!("{} {}", self.program, self.args.join(" ")).trim_end().to_string(),
        }
    }
}

/// Events streamed while a process runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEvent {
    Line { stream: Stream, text: String },
    Finished { exit_code: Option<i32>, stopped: bool, elapsed: Duration },
}

/// Live process: poll `events` (non-blocking) from the UI, call `stop` for the Stop button.
pub struct RunHandle {
    pub events: mpsc::Receiver<RunEvent>,
    pid: u32,
    stopped: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
}

impl RunHandle {
    /// Kill the process and its children. Safe to call repeatedly or after exit.
    pub fn stop(&self) {
        if self.done.load(Ordering::SeqCst) {
            return;
        }
        self.stopped.store(true, Ordering::SeqCst);
        kill_tree(self.pid);
    }
}

fn kill_tree(pid: u32) {
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    #[cfg(not(windows))]
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn build_command(spec: &RunSpec) -> Command {
    let mut cmd;
    if let Some(line) = &spec.shell_line {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd = Command::new("cmd");
            cmd.arg("/C").raw_arg(line);
        }
        #[cfg(not(windows))]
        {
            cmd = Command::new("sh");
            cmd.arg("-c").arg(line);
        }
    } else {
        cmd = Command::new(&spec.program);
        cmd.args(&spec.args);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    if let Some(dir) = &spec.cwd {
        cmd.current_dir(dir);
    }
    cmd.envs(spec.env.iter().map(|(k, v)| (k, v)));
    cmd.stdin(if spec.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn pump_lines<R: std::io::Read + Send + 'static>(r: R, stream: Stream, tx: mpsc::Sender<RunEvent>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut rd = BufReader::new(r);
        let mut buf = Vec::new();
        while let Ok(n) = rd.read_until(b'\n', &mut buf) {
            if n == 0 {
                break;
            }
            let mut text = String::from_utf8_lossy(&buf).into_owned();
            while text.ends_with('\n') || text.ends_with('\r') {
                text.pop();
            }
            buf.clear();
            if tx.send(RunEvent::Line { stream, text }).is_err() {
                break;
            }
        }
    })
}

/// Start `spec`, streaming output as it arrives. Agent requests without
/// confirmation are refused before anything is spawned.
pub fn start(spec: &RunSpec, who: Requester) -> Result<RunHandle, RunError> {
    if who == (Requester::Agent { confirmed: false }) {
        return Err(RunError::ConfirmationRequired);
    }
    let started = Instant::now();
    let mut child = build_command(spec).spawn().map_err(|e| RunError::Spawn(e.to_string()))?;
    let (tx, rx) = mpsc::channel();
    let stopped = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));

    if let (Some(input), Some(mut sin)) = (spec.stdin.clone(), child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = sin.write_all(input.as_bytes());
        });
    }
    let readers = [
        child.stdout.take().map(|o| pump_lines(o, Stream::Stdout, tx.clone())),
        child.stderr.take().map(|e| pump_lines(e, Stream::Stderr, tx.clone())),
    ];
    let pid = child.id();
    let (stopped2, done2) = (stopped.clone(), done.clone());
    std::thread::spawn(move || {
        let status = child.wait();
        done2.store(true, Ordering::SeqCst);
        for r in readers.into_iter().flatten() {
            let _ = r.join();
        }
        let _ = tx.send(RunEvent::Finished {
            exit_code: status.ok().and_then(|s| s.code()),
            stopped: stopped2.load(Ordering::SeqCst),
            elapsed: started.elapsed(),
        });
    });
    Ok(RunHandle { events: rx, pid, stopped, done })
}

fn platform_quote() -> Quote {
    if cfg!(windows) { Quote::Cmd } else { Quote::Posix }
}

/// Initial directory: the tool's `cwd` template, else the active file's
/// directory, else `None` (inherit).
pub fn initial_dir(tool: &ToolDef, ctx: &VarContext) -> Result<Option<PathBuf>, VarError> {
    if !tool.cwd.trim().is_empty() {
        return Ok(Some(PathBuf::from(tool_vars::expand(&tool.cwd, ctx, Quote::None)?)));
    }
    Ok(ctx.file.as_ref().and_then(|f| f.parent()).filter(|p| !p.as_os_str().is_empty()).map(Path::to_path_buf))
}

/// Expand a tool into a `RunSpec`. Direct mode expands each argument on its
/// own (no re-parsing, no injection). Shell mode quotes every substituted value.
pub fn build_run_spec(tool: &ToolDef, ctx: &VarContext) -> Result<RunSpec, VarError> {
    let q = if tool.shell { platform_quote() } else { Quote::None };
    let mut spec = RunSpec { cwd: initial_dir(tool, ctx)?, ..Default::default() };
    if tool.shell {
        let mut line = tool_vars::expand(&tool.command, ctx, q)?;
        for a in &tool.args {
            line.push(' ');
            line.push_str(&tool_vars::expand(a, ctx, q)?);
        }
        spec.shell_line = Some(line);
    } else {
        spec.program = tool_vars::expand(&tool.command, ctx, q)?;
        for a in &tool.args {
            spec.args.push(tool_vars::expand(a, ctx, q)?);
        }
    }
    for (k, v) in &tool.env {
        spec.env.push((k.clone(), tool_vars::expand(v, ctx, Quote::None)?));
    }
    if tool.output == OutputSink::ReplaceSelection {
        spec.stdin = Some(ctx.selection.clone().unwrap_or_default());
    }
    Ok(spec)
}

/// Full tool launch: confirmation gate, variable expansion, then the
/// caller-provided save step (`save` is told which scope to save and returns
/// `Err` to abort), then spawn.
pub fn start_tool(
    tool: &ToolDef,
    ctx: &VarContext,
    who: Requester,
    save: &mut dyn FnMut(SavePolicy) -> Result<(), String>,
) -> Result<RunHandle, RunError> {
    if who == (Requester::Agent { confirmed: false }) {
        return Err(RunError::ConfirmationRequired);
    }
    let spec = build_run_spec(tool, ctx)?;
    if tool.save_before != SavePolicy::None {
        save(tool.save_before).map_err(RunError::SaveFailed)?;
    }
    start(&spec, who)
}

/// Block until a filter run ends; stdout becomes the replacement text.
/// Non-zero exit or stop returns the stderr text as the error.
pub fn collect_filter(handle: &RunHandle) -> Result<String, String> {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    loop {
        match handle.events.recv() {
            Ok(RunEvent::Line { stream: Stream::Stderr, text }) => err.push(text),
            Ok(RunEvent::Line { text, .. }) => out.push(text),
            Ok(RunEvent::Finished { exit_code: Some(0), stopped: false, .. }) => return Ok(out.join("\n")),
            Ok(RunEvent::Finished { exit_code, .. }) => {
                return Err(format!("filter failed (exit {}): {}", exit_code.map_or("-".into(), |c| c.to_string()), err.join("\n")));
            }
            Err(_) => return Err("filter ended unexpectedly".into()),
        }
    }
}

/// Output-window footer, e.g. `Output completed (3 sec consumed), exit code 0`.
pub fn format_footer(elapsed: Duration, exit_code: Option<i32>, stopped: bool) -> String {
    let secs = elapsed.as_secs();
    let took = if secs >= 60 { format!("{} min {} sec", secs / 60, secs % 60) } else { format!("{secs} sec") };
    let head = if stopped { "Output stopped by user" } else { "Output completed" };
    match exit_code {
        Some(c) => format!("{head} ({took} consumed), exit code {c}"),
        None => format!("{head} ({took} consumed)"),
    }
}

/// Built-in default tools.
pub fn default_tools() -> Vec<UserTool> {
    vec![
        UserTool {
            name: "Cargo Build".into(),
            command: "cargo".into(),
            args: vec!["build".into()],
            capture: true,
            shortcut: "ctrl+1".into(),
            cwd: String::new(),
        },
        UserTool {
            name: "Cargo Test".into(),
            command: "cargo".into(),
            args: vec!["test".into()],
            capture: true,
            shortcut: "ctrl+2".into(),
            cwd: String::new(),
        },
        UserTool {
            name: "Cargo Run".into(),
            command: "cargo".into(),
            args: vec!["run".into()],
            capture: true,
            shortcut: "ctrl+3".into(),
            cwd: String::new(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_gcc_style_error() {
        let locs = parse_error_locations("src/main.rs:42:5: error: expected `;`");
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].file, "src/main.rs");
        assert_eq!(locs[0].line, 42);
        assert_eq!(locs[0].column, 5);
        assert!(locs[0].message.contains("expected"));
    }

    #[test]
    fn parse_gcc_style_warning() {
        let locs = parse_error_locations("lib.rs:10:1: warning: unused variable");
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].file, "lib.rs");
        assert_eq!(locs[0].line, 10);
    }

    #[test]
    fn parse_python_style_error() {
        let locs = parse_error_locations("  File \"script.py\", line 25, in main");
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].file, "script.py");
        assert_eq!(locs[0].line, 25);
    }

    #[test]
    fn parse_multiple_errors() {
        let output = "src/a.rs:1:1: error: foo\nsrc/b.rs:2:3: warning: bar\n";
        let locs = parse_error_locations(output);
        assert_eq!(locs.len(), 2);
    }

    #[test]
    fn parse_no_errors() {
        let locs = parse_error_locations("Build successful\n0 warnings\n");
        assert!(locs.is_empty());
    }

    #[test]
    fn parse_tools_toml_basic() {
        let toml = r#"
[[tool]]
name = "Build"
command = "make"
args = ["all"]
shortcut = "ctrl+1"

[[tool]]
name = "Clean"
command = "make"
args = ["clean"]
"#;
        let tools = parse_tools_toml(toml);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "Build");
        assert_eq!(tools[0].shortcut, "ctrl+1");
        assert_eq!(tools[1].name, "Clean");
    }

    #[test]
    fn parse_tools_toml_empty() {
        let tools = parse_tools_toml("");
        assert!(tools.is_empty());
    }

    #[test]
    fn default_tools_count() {
        let tools = default_tools();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0].name, "Cargo Build");
    }

    #[test]
    fn command_result_success() {
        // Use cmd /C on Windows, /bin/echo on Unix
        #[cfg(windows)]
        let result = run_command("cmd", &["/C".to_string(), "echo".to_string(), "hello".to_string()], None);
        #[cfg(not(windows))]
        let result = run_command("echo", &["hello".to_string()], None);
        assert!(result.success);
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("hello"));
    }

    #[test]
    fn command_result_failure() {
        // Run a command that doesn't exist
        let result = run_command("nonexistent_command_xyz_123", &[], None);
        assert!(!result.success);
    }

    #[test]
    fn parse_error_with_path_separator() {
        let locs = parse_error_locations("src/parser/mod.rs:100:8: error: type mismatch");
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].file, "src/parser/mod.rs");
    }

    #[test]
    fn parse_windows_path_error() {
        let locs = parse_error_locations("src\\main.rs:5:1: error: unexpected token");
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].file, "src\\main.rs");
    }

    #[test]
    fn cwd_path_empty_returns_none() {
        let tool = UserTool {
            name: "test".to_string(),
            command: "echo".to_string(),
            args: vec![],
            capture: true,
            shortcut: String::new(),
            cwd: String::new(),
        };
        assert!(tool.cwd_path().is_none());
    }

    #[test]
    fn cwd_path_with_value_returns_pathbuf() {
        let tool = UserTool {
            name: "test".to_string(),
            command: "echo".to_string(),
            args: vec![],
            capture: true,
            shortcut: String::new(),
            cwd: "/tmp".to_string(),
        };
        let path = tool.cwd_path().unwrap();
        assert_eq!(path.to_str().unwrap(), "/tmp");
    }

    #[test]
    fn user_tool_serde_roundtrip() {
        let tool = UserTool {
            name: "Build".to_string(),
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            capture: true,
            shortcut: "ctrl+1".to_string(),
            cwd: "/project".to_string(),
        };
        let json = serde_json::to_string(&tool).unwrap();
        let parsed: UserTool = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "Build");
        assert_eq!(parsed.cwd, "/project");
        assert!(parsed.capture);
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use crate::tools_config;

    const T: Duration = Duration::from_secs(10);

    fn shell_spec(line: &str) -> RunSpec {
        RunSpec { shell_line: Some(line.into()), ..Default::default() }
    }

    fn echo_then_sleep() -> RunSpec {
        #[cfg(windows)]
        return shell_spec("echo started & ping -n 30 127.0.0.1 >nul");
        #[cfg(not(windows))]
        return shell_spec("echo started; sleep 30");
    }

    fn tool(toml: &str) -> ToolDef {
        let c = tools_config::parse(toml);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        c.tools[0].clone()
    }

    fn drain(h: &RunHandle) -> (Vec<String>, Option<i32>, bool) {
        let mut lines = Vec::new();
        loop {
            match h.events.recv_timeout(T).expect("process did not finish") {
                RunEvent::Line { text, .. } => lines.push(text),
                RunEvent::Finished { exit_code, stopped, .. } => return (lines, exit_code, stopped),
            }
        }
    }

    #[test]
    fn lines_arrive_before_exit_and_stop_kills_tree() {
        let h = start(&echo_then_sleep(), Requester::User).unwrap();
        match h.events.recv_timeout(T).unwrap() {
            RunEvent::Line { text, stream } => assert_eq!((text.trim(), stream), ("started", Stream::Stdout)),
            other => panic!("expected a line first, got {other:?}"),
        }
        // Still running: no Finished yet.
        assert!(h.events.try_recv().is_err());
        let t0 = Instant::now();
        h.stop();
        let (_, _, stopped) = drain(&h);
        assert!(stopped);
        assert!(t0.elapsed() < Duration::from_secs(8), "stop must not wait for the 30 s child");
    }

    #[test]
    fn captures_exit_code_and_footer() {
        #[cfg(windows)]
        let spec = shell_spec("echo out & echo err 1>&2 & exit 3");
        #[cfg(not(windows))]
        let spec = shell_spec("echo out; echo err 1>&2; exit 3");
        let h = start(&spec, Requester::User).unwrap();
        let mut seen = Vec::new();
        let code = loop {
            match h.events.recv_timeout(T).unwrap() {
                RunEvent::Line { stream, text } => seen.push((stream, text.trim().to_string())),
                RunEvent::Finished { exit_code, stopped, .. } => {
                    assert!(!stopped);
                    break exit_code;
                }
            }
        };
        assert_eq!(code, Some(3));
        assert!(seen.contains(&(Stream::Stdout, "out".into())) && seen.contains(&(Stream::Stderr, "err".into())), "{seen:?}");
        assert_eq!(format_footer(Duration::from_secs(3), Some(0), false), "Output completed (3 sec consumed), exit code 0");
        assert_eq!(format_footer(Duration::from_secs(125), None, true), "Output stopped by user (2 min 5 sec consumed)");
    }

    #[test]
    fn agent_needs_explicit_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran.txt");
        #[cfg(windows)]
        let line = format!("echo x > \"{}\"", marker.display());
        #[cfg(not(windows))]
        let line = format!("echo x > '{}'", marker.display());
        let spec = shell_spec(&line);
        assert_eq!(start(&spec, Requester::Agent { confirmed: false }).err(), Some(RunError::ConfirmationRequired));
        let t = tool("[[tool]]\nname='a'\ncommand='x'\n");
        let r = start_tool(&t, &VarContext::default(), Requester::Agent { confirmed: false }, &mut |_| Ok(()));
        assert_eq!(r.err(), Some(RunError::ConfirmationRequired));
        std::thread::sleep(Duration::from_millis(300));
        assert!(!marker.exists(), "nothing may run without confirmation");
        let h = start(&spec, Requester::Agent { confirmed: true }).unwrap();
        drain(&h);
        assert!(marker.exists());
    }

    #[test]
    fn save_before_run_is_called_and_can_abort() {
        let t = tool("[[tool]]\nname='a'\ncommand='nonexistent_cmd_xyz'\nsave_before='all'\n");
        let mut got = Vec::new();
        let r = start_tool(&t, &VarContext::default(), Requester::User, &mut |p| {
            got.push(p);
            Err("disk full".into())
        });
        assert_eq!(got, vec![SavePolicy::All]);
        assert_eq!(r.err(), Some(RunError::SaveFailed("disk full".into())));
        // policy none: the callback is not invoked
        let t = tool("[[tool]]\nname='a'\ncommand='nonexistent_cmd_xyz'\n");
        let r = start_tool(&t, &VarContext::default(), Requester::User, &mut |_| panic!("no save expected"));
        assert!(matches!(r.err(), Some(RunError::Spawn(_))));
    }

    #[test]
    fn shell_mode_quotes_values_no_injection() {
        let t = tool("[[tool]]\nname='a'\ncommand='echo'\nargs=['${selection}']\nshell=true\n");
        let ctx = VarContext { selection: Some("one & echo pwned; echo pwned".into()), ..Default::default() };
        let spec = build_run_spec(&t, &ctx).unwrap();
        let h = start(&spec, Requester::User).unwrap();
        let (lines, code, _) = drain(&h);
        assert_eq!(code, Some(0));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("one & echo pwned; echo pwned"));
        // an unquotable value is refused instead of being run
        let bad = VarContext { selection: Some("a\"b".into()), ..Default::default() };
        if cfg!(windows) {
            assert!(matches!(build_run_spec(&t, &bad), Err(VarError::Unsafe { .. })));
        }
    }

    #[test]
    fn direct_mode_passes_args_untouched() {
        let t = tool("[[tool]]\nname='a'\ncommand='prog'\nargs=['--f=${file}', '${selection:none}']\n");
        let ctx = VarContext { file: Some(PathBuf::from("/a b/c d.txt")), ..Default::default() };
        let s = build_run_spec(&t, &ctx).unwrap();
        assert_eq!(s.args, vec!["--f=/a b/c d.txt", "none"]);
        assert!(s.shell_line.is_none());
        assert_eq!(s.cwd, Some(PathBuf::from("/a b")));
    }

    #[test]
    fn initial_dir_rules_and_effect() {
        let dir = tempfile::tempdir().unwrap();
        let t = tool(&format!("[[tool]]\nname='a'\ncommand='x'\ncwd='{}'\n", dir.path().to_string_lossy().replace('\\', "/")));
        let ctx = VarContext { file: Some(PathBuf::from("/elsewhere/f.txt")), ..Default::default() };
        assert_eq!(initial_dir(&t, &ctx).unwrap(), Some(PathBuf::from(dir.path().to_string_lossy().replace('\\', "/"))));
        let t2 = tool("[[tool]]\nname='a'\ncommand='x'\n");
        assert_eq!(initial_dir(&t2, &ctx).unwrap(), Some(PathBuf::from("/elsewhere")));
        assert_eq!(initial_dir(&t2, &VarContext::default()).unwrap(), None);
        // the process really starts there
        #[cfg(windows)]
        let line = "cd";
        #[cfg(not(windows))]
        let line = "pwd";
        let spec = RunSpec { cwd: Some(dir.path().to_path_buf()), ..shell_spec(line) };
        let (lines, _, _) = drain(&start(&spec, Requester::User).unwrap());
        let leaf = dir.path().file_name().unwrap().to_string_lossy().into_owned();
        assert!(lines[0].contains(&leaf), "{lines:?}");
    }

    #[test]
    fn filter_mode_replaces_selection_with_stdout() {
        // Echo stdin back: `findstr` on Windows, `cat` elsewhere.
        #[cfg(windows)]
        let (program, args) = ("findstr", vec!["^".to_string()]);
        #[cfg(not(windows))]
        let (program, args) = ("cat", Vec::new());
        let spec = RunSpec { program: program.into(), args, stdin: Some("b\na\nc\n".into()), ..Default::default() };
        let h = start(&spec, Requester::User).unwrap();
        assert_eq!(collect_filter(&h).unwrap(), "b\na\nc");
        let t = tool("[[tool]]\nname='a'\ncommand='sort'\noutput='replace_selection'\n");
        let ctx = VarContext { selection: Some("z\ny".into()), ..Default::default() };
        assert_eq!(build_run_spec(&t, &ctx).unwrap().stdin.as_deref(), Some("z\ny"));
    }
}
