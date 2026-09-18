#!/usr/bin/env python3
"""A stand-in for embarch-core, just enough to drive embarch-ui's Live Study tab.

Serves the routes the tab touches, with fixtures for the states that are hard
to produce on a bench: an interrupted study, an unreadable one, a capped feed,
a `lagged` frame, and a study with neither a trace nor any data.
"""
import json, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

TOKEN = "stub-token"
SCHEMA = int(sys.argv[2]) if len(sys.argv) > 2 else 0

RUNNING = "aa" * 16
DONE = "bb" * 16
INTERRUPTED = "cc" * 16
UNREADABLE = "dd" * 16
BARE = "ee" * 16

STUDIES = {
    "keep": 50,
    "studies": [
        {"study_id": RUNNING, "study_name": "live-run", "status": "running",
         "started_utc_ms": 1700000000000,
         "steps": {"total": 1, "passed": 1, "failed": 0, "timed_out": 0, "unknown": 0},
         "taps": [{"name": "dev-bench", "encoding": "Text", "rendered": False},
                  {"name": "rail", "encoding": {"Samples": {"layout": "U16Le", "unit": "Milliamps", "channel_id": 0}}, "rendered": True}]},
        {"study_id": DONE, "study_name": "nightly", "status": "completed",
         "started_utc_ms": 1700000000000, "ended_utc_ms": 1700000045000,
         "steps": {"total": 2, "passed": 1, "failed": 1, "timed_out": 0, "unknown": 0},
         "taps": [{"name": "dev-bench", "encoding": "Text", "rendered": False},
                  {"name": "rail", "encoding": {"Samples": {"layout": "U16Le", "unit": "Milliamps", "channel_id": 0}}, "rendered": True},
                  {"name": "blob", "encoding": "Raw", "rendered": False}]},
        {"study_id": INTERRUPTED, "study_name": "killed-midway", "status": "interrupted",
         "started_utc_ms": 1700000000000,
         "steps": {"total": 1, "passed": 1, "failed": 0, "timed_out": 0, "unknown": 0},
         "taps": []},
        {"study_id": UNREADABLE, "status": "unknown",
         "note": "this build cannot read its events file: EOF while parsing a value"},
        {"study_id": BARE, "study_name": "no-taps-at-all", "status": "completed",
         "started_utc_ms": 1700000000000, "ended_utc_ms": 1700000001000,
         "steps": {"total": 1, "passed": 1, "failed": 0, "timed_out": 0, "unknown": 0},
         "taps": [{"name": "dev-bench", "encoding": "Text", "rendered": False}]},
    ],
}

STEPS = {
    DONE: {"study_name": "nightly", "timed": True, "steps": [
        {"index": 0, "step_name": "ble-connect", "outcome": "Pass", "reason": None,
         "delay_before_ms": 0, "started_utc_ms": 1700000000000, "ended_utc_ms": 1700000000250},
        {"index": 1, "step_name": "hrm-start", "outcome": "Fail", "reason": "ERR_PERMISSION",
         "delay_before_ms": 0, "started_utc_ms": 1700000000250, "ended_utc_ms": 1700000045000},
    ]},
    BARE: {"study_name": "no-taps-at-all", "timed": True, "steps": [
        {"index": 0, "step_name": "noop", "outcome": "Pass", "reason": None,
         "delay_before_ms": 0, "started_utc_ms": 1700000000000, "ended_utc_ms": 1700000001000},
    ]},
    INTERRUPTED: {"study_name": "killed-midway", "timed": False, "steps": [
        {"index": 0, "step_name": "ble-connect", "outcome": "Pass", "reason": None,
         "delay_before_ms": None, "started_utc_ms": None, "ended_utc_ms": None},
    ]},
}

STREAMS = {
    DONE: {"streams": [
        {"id": 0, "name": "dev-bench", "encoding": "Text", "rendered": False},
        {"id": 1, "name": "rail", "encoding": {"Samples": {"layout": "U16Le", "unit": "Milliamps", "channel_id": 0}}, "rendered": True},
        {"id": 2, "name": "blob", "encoding": "Raw", "rendered": False},
    ]},
    BARE: {"streams": [{"id": 0, "name": "dev-bench", "encoding": "Text", "rendered": False}]},
    RUNNING: {"streams": [
        {"id": 0, "name": "dev-bench", "encoding": "Text", "rendered": False},
        {"id": 1, "name": "rail", "encoding": {"Samples": {"layout": "U16Le", "unit": "Milliamps", "channel_id": 0}}, "rendered": True},
    ]},
}

CAPTURES = {
    (DONE, "dev-bench"): b"booted\nuart:~$ hrm_start\nERR_PERMISSION\na line with no newline",
    (DONE, "rail"): ("rx_utc_ms,step_name,value,unit,channel_id\n" + "".join(
        "%d,ble-connect,%.3f,Milliamps,0\n" % (1700000000000 + i * 10, 1.5 + (i % 7) * 0.25)
        for i in range(300))).encode(),
    (DONE, "blob"): bytes(range(256)),
    (BARE, "dev-bench"): b"",
}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _bytes(self, body, ctype="application/octet-stream", code=200):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _text(self, msg, code=404):
        self._bytes(msg.encode(), "text/plain", code)

    def do_GET(self):
        if self.headers.get("authorization") != "Bearer " + TOKEN:
            return self._text("unauthorized", 401)
        path = urlparse(self.path).path
        parts = [p for p in path.split("/") if p]

        if path == "/status":
            return self._json({"status": "ok", "probes": [],
                               "study_designer_schema_version": SCHEMA})
        if path == "/studies":
            return self._json(STUDIES)
        if path == "/alerts" or path == "/probes/enrolled" or path == "/signals":
            return self._json([])
        if path == "/logs/recent":
            return self._json({"lines": []})
        if path == "/dev-bench/port":
            return self._text("no dev-bench", 404)

        if len(parts) >= 2 and parts[0] == "study":
            sid = parts[1]
            if len(parts) == 2:
                if sid == RUNNING:
                    return self._json({"status": "running", "current_step": 0,
                                       "total_steps": 3, "result": None, "reason": None})
                return self._text("no study job found (Core has restarted since)", 404)
            if parts[2] == "steps":
                if sid in STEPS:
                    return self._json(STEPS[sid])
                if sid == UNREADABLE:
                    return self._text("has an events.json this build cannot read", 422)
                return self._text("no events.json", 404)
            if parts[2] == "streams":
                if sid in STREAMS:
                    return self._json(STREAMS[sid])
                return self._text("no captured streams", 404)
            if parts[2] == "stream" and len(parts) >= 4:
                body = CAPTURES.get((sid, parts[3]))
                if body is None:
                    return self._text("no such tap", 404)
                return self._bytes(body)
            if parts[2] == "events":
                return self._sse(sid)
        return self._text("not found in the stub", 404)

    # ---- the live stream ---------------------------------------------------
    def _sse(self, sid):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()

        def frame(obj, name=None):
            out = b""
            if name:
                out += b"event: " + name.encode() + b"\n"
            out += b"data: " + json.dumps(obj).encode() + b"\n\n"
            return out

        try:
            # A console chunk that ends mid-line, then one that finishes it.
            self.wfile.write(frame({"kind": "StreamText", "study_id": sid, "stream_id": 0,
                                    "stream_name": "dev-bench", "step_index": 0,
                                    "rx_utc_ms": 1700000000000, "text": "booted\nuart:~$ hal"}))
            self.wfile.flush()
            time.sleep(0.2)
            self.wfile.write(frame({"kind": "StreamText", "study_id": sid, "stream_id": 0,
                                    "stream_name": "dev-bench", "step_index": 0,
                                    "rx_utc_ms": 1700000000100, "text": "f a line\nstill arriving"}))
            self.wfile.write(frame({"kind": "SampleBatch", "study_id": sid, "stream_id": 1,
                                    "stream_name": "rail",
                                    "samples": [{"rx_utc_ms": 1700000000000 + i * 10,
                                                 "step_name": "ble-connect",
                                                 "value": 1.5 + (i % 5) * 0.2,
                                                 "unit": "Milliamps", "channel_id": 0}
                                                for i in range(20)]}))
            self.wfile.write(frame({"kind": "StepCompleted", "study_id": sid, "step_index": 0,
                                    "result": {"step_name": "ble-connect", "outcome": "Pass",
                                               "captured_data": None}}))
            # The one frame a client must surface rather than swallow.
            self.wfile.write(frame({}, None).replace(b"data: {}", b"data: 7").replace(
                b"\n\n", b"\n\n") if False else b"event: lagged\ndata: 7\n\n")
            self.wfile.flush()
            time.sleep(0.3)
            self.wfile.write(frame({"kind": "StepCompleted", "study_id": sid, "step_index": 1,
                                    "result": {"step_name": "hrm-start",
                                               "outcome": {"Fail": {"reason": "ERR_PERMISSION"}},
                                               "captured_data": None}}))
            self.wfile.write(frame({"kind": "StatusChanged", "study_id": sid,
                                    "status": "failed", "reason": "step 2 ('hrm-start') failed (ERR_PERMISSION)"}))
            self.wfile.flush()
            time.sleep(5)
        except (BrokenPipeError, ConnectionResetError):
            pass


if __name__ == "__main__":
    port = int(sys.argv[1])
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
