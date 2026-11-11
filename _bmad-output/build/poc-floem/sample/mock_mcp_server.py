import sys
import json

def log(msg):
    sys.stderr.write(f"[mock-mcp-log] {msg}\n")
    sys.stderr.flush()

log("Mock MCP server starting...")

try:
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        log(f"Received: {line}")
        try:
            req = json.loads(line)
            req_id = req.get("id")
            method = req.get("method")
            params = req.get("params", {})

            # Prepare default response result
            result = None

            if method == "ping":
                result = {"status": "pong"}
            elif method == "chat":
                user_msg = params.get("message", "")
                result = {
                    "reply": f"Hello! This is a mock response from the offline local LLM. You asked: '{user_msg}'"
                }
            else:
                result = {"status": "unsupported method"}

            resp = {
                "jsonrpc": "2.0",
                "id": req_id,
                "result": result
            }

            sys.stdout.write(json.dumps(resp) + "\n")
            sys.stdout.flush()
        except Exception as e:
            log(f"Error handling line: {e}")
except KeyboardInterrupt:
    log("Interrupted")

log("Mock MCP server exiting.")
