#!/usr/bin/env python3
"""A stand-in for Anthropic's Messages API, for dino-tui's tests: a debug dinod's proxy sends the
stand-in Claude's calls here (DINO_ANTHROPIC_UPSTREAM). It streams a short made-up answer, slowly
enough to show a session working and a rate in tokens per second, with usage and the subscription
headers a Claude account's answers carry (a 5-hour window at STANDIN_5H, 42% unless set).

Prints the port it listens on (127.0.0.1), then serves until killed.
"""

import hashlib
import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

WORDS = ("Looked at it. The list groups sessions by repo, needs-you first; attaching passes the "
         "agent's terminal through; the prefix key brings you back. Nothing real ran here.").split()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def do_POST(self):
        n = int(self.headers.get("content-length") or 0)
        body = json.loads(self.rfile.read(n) or b"{}")
        if "count_tokens" in self.path or "/v1/messages" not in self.path:
            data = json.dumps({"input_tokens": 1200}).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        now = int(time.time())
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        # Each account its own organization: a token's call says which (dinod tells accounts apart
        # by it); the stand-in Claude's own calls are account 1's.
        auth = self.headers.get("authorization", "")
        org = "org-standin-" + hashlib.sha1(auth.encode()).hexdigest()[:8] if auth.startswith("Bearer sk-ant-") else "org-standin-1"
        self.send_header("anthropic-organization-id", org)
        self.send_header("anthropic-ratelimit-unified-status", "allowed")
        self.send_header("anthropic-ratelimit-unified-5h-status", "allowed")
        self.send_header("anthropic-ratelimit-unified-5h-utilization", os.environ.get("STANDIN_5H", "0.42"))
        self.send_header("anthropic-ratelimit-unified-5h-reset", str(now + 7200))
        self.send_header("anthropic-ratelimit-unified-7d-status", "allowed")
        self.send_header("anthropic-ratelimit-unified-7d-utilization", "0.10")
        self.send_header("anthropic-ratelimit-unified-7d-reset", str(now + 5 * 86400))
        self.send_header("connection", "close")
        self.end_headers()
        model = body.get("model", "claude-opus-4-5")
        delay = float(os.environ.get("STANDIN_DELAY", "0.12"))

        def event(kind, data):
            self.wfile.write(f"event: {kind}\ndata: {json.dumps(data)}\n\n".encode())
            self.wfile.flush()

        try:
            event("message_start", {"type": "message_start", "message": {"id": f"msg_standin{now}", "type": "message", "role": "assistant", "model": model, "content": [], "stop_reason": None,
                                                                        "usage": {"input_tokens": 1200, "output_tokens": 1, "cache_read_input_tokens": 9000, "cache_creation_input_tokens": 300}}})
            event("content_block_start", {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})
            for w in WORDS:
                event("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": w + " "}})
                time.sleep(delay)
            event("content_block_stop", {"type": "content_block_stop", "index": 0})
            event("message_delta", {"type": "message_delta", "delta": {"stop_reason": "end_turn", "stop_sequence": None}, "usage": {"output_tokens": len(WORDS) * 9}})
            event("message_stop", {"type": "message_stop"})
        except (BrokenPipeError, ConnectionResetError):
            pass
        self.close_connection = True


def main():
    srv = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    print(srv.server_address[1], flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    sys.exit(main())
