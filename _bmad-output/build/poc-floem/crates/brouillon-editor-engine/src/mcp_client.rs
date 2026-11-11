use std::process::Stdio;
use std::sync::{Arc, Mutex};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::BufReader;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    pub result: Option<Value>,
    pub error: Option<Value>,
}

pub struct McpClient {
    child: Arc<Mutex<Option<Child>>>,
    tx: mpsc::UnboundedSender<String>,
}

use std::sync::LazyLock;

static TOKIO_RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime for MCP client")
});

impl McpClient {
    /// Spawn a local MCP server subprocess and establish standard I/O communication.
    pub fn spawn(
        command: &str,
        args: &[String],
        on_response: impl Fn(JsonRpcResponse) + Send + 'static,
    ) -> Result<Self, std::io::Error> {
        // Enforce running inside our dedicated tokio runtime context if not already in one
        let _guard = TOKIO_RUNTIME.enter();

        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = child.stdin.take().expect("failed to open stdin");
        let stdout = child.stdout.take().expect("failed to open stdout");
        let stderr = child.stderr.take().expect("failed to open stderr");

        let child_arc = Arc::new(Mutex::new(Some(child)));
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        // Background task to write requests to the server's stdin
        let mut writer = stdin;
        TOKIO_RUNTIME.spawn(async move {
            while let Some(msg) = rx.recv().await {
                if let Err(e) = writer.write_all(msg.as_bytes()).await {
                    eprintln!("[mcp] write error: {e}");
                    break;
                }
                if let Err(e) = writer.write_all(b"\n").await {
                    eprintln!("[mcp] write error: {e}");
                    break;
                }
                let _ = writer.flush().await;
            }
        });

        // Background task to read responses from the server's stdout
        let mut reader = BufReader::new(stdout).lines();
        TOKIO_RUNTIME.spawn(async move {
            while let Ok(Some(line)) = reader.next_line().await {
                if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&line) {
                    on_response(resp);
                } else {
                    eprintln!("[mcp] invalid json-rpc message: {line}");
                }
            }
        });

        // Background task to capture stderr log messages
        let mut err_reader = BufReader::new(stderr).lines();
        TOKIO_RUNTIME.spawn(async move {
            while let Ok(Some(line)) = err_reader.next_line().await {
                eprintln!("[mcp server log] {line}");
            }
        });

        Ok(Self { child: child_arc, tx })
    }

    /// Send a request to the MCP server
    pub fn send_request(&self, id: i64, method: &str, params: Value) -> Result<(), Box<dyn std::error::Error>> {
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: Value::Number(id.into()),
            method: method.to_string(),
            params,
        };
        let msg = serde_json::to_string(&req)?;
        self.tx.send(msg)?;
        Ok(())
    }

    /// Terminate the MCP server subprocess
    pub fn terminate(&self) {
        if let Ok(mut guard) = self.child.lock() {
            if let Some(mut child) = guard.take() {
                let _ = child.start_kill();
            }
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn test_mcp_client_mock() {
        tokio::block_on(async {
            let command = "powershell";
            let args = vec!["-Command".to_string(), "while ($line = [Console]::In.ReadLine()) { [Console]::Out.WriteLine($line) }".to_string()];

            let received = Arc::new(AtomicBool::new(false));
            let received_clone = received.clone();

            let client_res = McpClient::spawn(&command, &args, move |resp| {
                if resp.id == Value::Number(42.into()) {
                    received_clone.store(true, Ordering::SeqCst);
                }
            });

            if let Ok(client) = client_res {
                client.send_request(42, "ping", Value::Null).unwrap();
                // PowerShell takes a variable time to start (more than a second when the machine is
                // busy), so wait for the echo instead of sleeping for a fixed second.
                for _ in 0..100 {
                    if received.load(Ordering::SeqCst) {
                        break;
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
                client.terminate();
                assert!(received.load(Ordering::SeqCst), "should have received the echo response");
            } else {
                eprintln!("skipped test: cmd not available");
            }
        });
    }
}
