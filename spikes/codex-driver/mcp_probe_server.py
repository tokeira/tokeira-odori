#!/usr/bin/env python3
"""Tiny sessionless streamable-HTTP MCP server for the Codex driver probe.

Every request is emitted as one JSON line containing the headers and body.
MODE=respond returns a tool result; MODE=block sleeps long enough to exercise
Codex's pinned tool timeout; MODE=close drops the tools/call connection.
"""

import json
import os
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


HOST = "127.0.0.1"
PORT = int(os.environ.get("PORT", "8765"))
TOKEN = os.environ.get("TOKEN", "odori-probe-token")
MODE = os.environ.get("MODE", "respond")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def do_GET(self):
        self.send_error(405)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length)
        body = json.loads(raw or b"null")
        print(
            json.dumps(
                {
                    "headers": {key: value for key, value in self.headers.items()},
                    "body": body,
                },
                separators=(",", ":"),
            ),
            flush=True,
        )
        if self.headers.get("Authorization") != f"Bearer {TOKEN}":
            self._json(401, {"error": "missing bearer token"})
            return

        method = body.get("method") if isinstance(body, dict) else None
        request_id = body.get("id") if isinstance(body, dict) else None
        if method == "notifications/initialized":
            self.send_response(202)
            self.send_header("Content-Length", "0")
            self.end_headers()
        elif method == "initialize":
            self._result(
                request_id,
                {
                    "protocolVersion": body.get("params", {}).get(
                        "protocolVersion", "2025-06-18"
                    ),
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "odori-codex-probe", "version": "0.1.0"},
                },
            )
        elif method == "tools/list":
            self._result(
                request_id,
                {
                    "tools": [
                        {
                            "name": "odori_probe",
                            "description": "Return a fixed probe result.",
                            "inputSchema": {"type": "object", "properties": {}},
                            "annotations": {
                                "readOnlyHint": True,
                                "destructiveHint": False,
                                "idempotentHint": True,
                                "openWorldHint": False,
                            },
                        }
                    ]
                },
            )
        elif method == "tools/call":
            if MODE == "block":
                time.sleep(30)
            elif MODE == "close":
                self.close_connection = True
                return
            self._result(
                request_id,
                {"content": [{"type": "text", "text": "odori-probe-ok"}]},
            )
        else:
            self._json(
                200,
                {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {"code": -32601, "message": f"unknown method: {method}"},
                },
            )

    def _result(self, request_id, result):
        self._json(200, {"jsonrpc": "2.0", "id": request_id, "result": result})

    def _json(self, status, payload):
        data = json.dumps(payload, separators=(",", ":")).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
        self.wfile.flush()


if __name__ == "__main__":
    print(f"listening=http://{HOST}:{PORT}/mcp mode={MODE}", flush=True)
    ThreadingHTTPServer((HOST, PORT), Handler).serve_forever()
