#!/usr/bin/env python3
"""Minimal MCP stdio server for the crash-mid-tool-call probe (Q2).

Serves one tool. MODE=block never answers tools/call (so the harness can be
killed mid-await); MODE=respond answers immediately. Every inbound message is
appended verbatim to the file named by LOG, which is what the probe compares
across kill/resume: whether a resumed session re-issues the pending call, and
what identity (JSON-RPC id, params, _meta) the re-issued call carries.

Usage (via --mcp-config): {"command": "python3", "args": ["mcp_probe_server.py"],
                           "env": {"MODE": "block", "LOG": "/path/calls.jsonl"}}
"""

import json
import os
import sys
import time

MODE = os.environ.get("MODE", "respond")
LOG = os.environ.get("LOG", "/tmp/mcp-probe.jsonl")

TOOL = {
    "name": "wait_for_green_light",
    "description": "Blocks until an external system turns green. Call it when asked.",
    "inputSchema": {"type": "object", "properties": {}},
}


def log(entry):
    with open(LOG, "a") as f:
        f.write(json.dumps(entry) + "\n")


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            log({"unparsed": line})
            continue
        log({"ts": time.time(), "recv": msg})
        method, mid = msg.get("method"), msg.get("id")
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": mid, "result": {
                "protocolVersion": msg["params"]["protocolVersion"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "spike", "version": "0.0.0"},
            }})
        elif method == "tools/list":
            send({"jsonrpc": "2.0", "id": mid, "result": {"tools": [TOOL]}})
        elif method == "ping":
            send({"jsonrpc": "2.0", "id": mid, "result": {}})
        elif method == "tools/call":
            if MODE == "block":
                continue  # never answer; the probe kills the harness mid-await
            send({"jsonrpc": "2.0", "id": mid, "result": {
                "content": [{"type": "text", "text": "green"}],
                "isError": False,
            }})
        elif mid is not None:
            send({"jsonrpc": "2.0", "id": mid,
                  "error": {"code": -32601, "message": "method not found"}})


if __name__ == "__main__":
    main()
