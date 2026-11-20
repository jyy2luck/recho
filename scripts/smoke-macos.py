"""Isolated native smoke and a single Release memory sample; requires macOS UI access."""
import argparse
import ctypes
import gzip
import http.client
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import platform
import socket
import sqlite3
import statistics
import subprocess
import tempfile
import threading
import time
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--executable", type=Path, default=Path("app/build/release/recho.app/Contents/MacOS/recho"))
parser.add_argument("--evidence-dir", type=Path, default=Path("research/evidence/native-macos"))
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
evidence = args.evidence_dir.resolve()
evidence.mkdir(parents=True, exist_ok=True)
helper = evidence / "macos-ui"
subprocess.run(["swiftc", str(root / "scripts/macos-ui.swift"), "-o", str(helper)], check=True)
data = Path(tempfile.mkdtemp(prefix="recho-smoke-"))
log = (evidence / "app.log").open("w")
app = subprocess.Popen([str(args.executable.resolve())], env=dict(os.environ, RECHO_DATA_DIR=str(data), RECHO_PROXY_PORT="0"), stdout=log, stderr=log)
libproc = ctypes.CDLL("/usr/lib/libproc.dylib")
samples = []
phase = "startup"
done = threading.Event()


def sample():
    while not done.wait(.2):
        usage = (ctypes.c_uint64 * 32)()
        if libproc.proc_pid_rusage(app.pid, 2, ctypes.byref(usage)) == 0:
            samples.append({"time": time.monotonic(), "phase": phase, "rss": usage[8], "footprint": usage[9], "cpu_ns": usage[2] + usage[3]})


threading.Thread(target=sample, daemon=True).start()


def wait_for(predicate, timeout=12):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        assert app.poll() is None, f"App exited {app.returncode}: {(evidence / 'app.log').read_text()}"
        time.sleep(.15)
    raise AssertionError("Timed out waiting for native state")


def ui(action, *values):
    return subprocess.check_output([str(helper), str(app.pid), action, *map(str, values)], text=True)


def tree():
    return json.dumps(json.loads(ui("tree")), ensure_ascii=False)


def copy_body(expected):
    ui("clear-clipboard")
    ui("press", "复制完整正文")
    wait_for(lambda: subprocess.check_output(["pbpaste"]) == expected, timeout=3)


def screenshot(name):
    windows = json.loads(ui("windows"))
    window = next(window for window in windows if window.get("kCGWindowName") == "recho")
    subprocess.run(["screencapture", "-x", "-l", str(window["kCGWindowNumber"]), str(evidence / (name + ".png"))], check=True)


class Origin(BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers["Content-Length"]))
        output = gzip.compress(body) if self.path == "/gzip" else body
        self.send_response(200)
        self.send_header("Content-Type", "application/octet-stream" if self.path == "/binary" else "text/plain; charset=utf-8")
        self.send_header("Content-Length", str(len(output)))
        if self.path == "/gzip":
            self.send_header("Content-Encoding", "gzip")
        self.end_headers()
        self.wfile.write(output)

    def log_message(self, *_):
        pass


origin = ThreadingHTTPServer(("127.0.0.1", 0), Origin)
threading.Thread(target=origin.serve_forever, daemon=True).start()
report = {"platform": platform.platform(), "data_directory": str(data), "pid": app.pid}
try:
    wait_for(lambda: (data / "control.json").exists())
    descriptor = json.loads((data / "control.json").read_text())

    def api(path):
        request = urllib.request.Request(descriptor["url"] + path, headers={"Authorization": "Bearer " + descriptor["token"]})
        with urllib.request.urlopen(request, timeout=10) as response:
            return json.load(response)

    initial = api("/state")
    port = initial["proxyPort"]
    report["proxy_port"] = port
    wait_for(lambda: bool(json.loads(ui("windows"))))
    ui("activate")
    tree()
    phase = "empty"
    time.sleep(5)
    screenshot("empty")

    def proxy(path, body):
        connection = http.client.HTTPConnection("127.0.0.1", port, timeout=10)
        connection.request("POST", f"http://127.0.0.1:{origin.server_port}{path}", body=body, headers={"Content-Type": "text/plain"})
        response = connection.getresponse()
        received = response.read()
        assert response.status == 200
        if path == "/gzip":
            received = gzip.decompress(received)
        assert received == body
        connection.close()

    text = ("native-start\n" + ("abcdefghijklmnopqrstuvwxyz0123456789" * 2 + "\n") * 1100 + "中文正文\nnative-tail-END\n").encode()
    phase = "native_interaction"
    proxy("/plain", text)
    url = f"http://127.0.0.1:{origin.server_port}/plain"
    wait_for(lambda: url in tree())
    ui("double-click-text", url)
    wait_for(lambda: "复制完整正文" in tree())
    copy_body(text)
    screenshot("plain-detail")
    ui("click-text", "请求体")
    copy_body(text)
    ui("click-text", "请求头")
    wait_for(lambda: "80342" in tree() and "text/plain" in tree())
    screenshot("request-headers")
    ui("click-text", "总览")
    ui("hover-text", "native-start")
    ui("scroll", -10000)
    wait_for(lambda: "native-tail-END" in tree())
    screenshot("plain-tail")
    ui("click-text", "Hex")
    wait_for(lambda: "00000000  " in tree())
    first_hex = next(value for value in tree().split('"') if value.startswith("00000000  "))
    ui("hover-text", first_hex)
    ui("scroll", -10000)
    hex_tail = f"{(len(text) - 1) // 16 * 16:08x}  "
    wait_for(lambda: hex_tail in tree())
    screenshot("hex-tail")
    ui("click-text", "Raw")
    proxy("/gzip", text)
    gzip_url = f"http://127.0.0.1:{origin.server_port}/gzip"
    wait_for(lambda: gzip_url in tree())
    ui("double-click-text", gzip_url)
    wait_for(lambda: "复制完整正文" in tree())
    copy_body(text)
    screenshot("gzip-detail")
    ui("click-text", "JSON")
    wait_for(lambda: "共 0 项" in tree())
    ui("click-text", "All")
    wait_for(lambda: "共 2 项" in tree())
    binary = bytes(range(256)) * 320 + b"binary-END"
    proxy("/binary", binary)
    binary_url = f"http://127.0.0.1:{origin.server_port}/binary"
    wait_for(lambda: binary_url in tree())
    ui("double-click-text", binary_url)
    wait_for(lambda: "00000000  00 01 02" in tree())
    first_hex = next(value for value in tree().split('"') if value.startswith("00000000  00 01 02"))
    ui("hover-text", first_hex)
    ui("scroll", -10000)
    binary_tail = f"{(len(binary) - 1) // 16 * 16:08x}  "
    wait_for(lambda: binary_tail in tree())
    screenshot("binary-tail")
    ui("click-text", "■  停止")
    wait_for(lambda: not api("/state")["running"])
    ui("click-text", "▶  调试")
    wait_for(lambda: api("/state")["running"])
    screenshot("resumed")
    ui("click-text", "清空捕获")
    wait_for(lambda: "持久化历史将保留" in tree())
    ui("click-text", "清空")
    wait_for(lambda: not api("/state")["sessions"])
    database = sqlite3.connect(f"file:{data / 'history.sqlite'}?mode=ro", uri=True)
    assert database.execute("SELECT count(*) FROM sessions").fetchone()[0] == 3
    report["native"] = {"plain_and_gzip_decoded_bytes": len(text), "binary_bytes": len(binary), "exact_copy": True, "request_body_and_headers": True, "text_and_hex_scroll_to_tail": True, "quick_filters": True, "pause_resume": True, "clear_preserves_history": True}
    print("Native capture, copy, pause/resume and clear passed", flush=True)

    payload = ("p" * 400 + "\n") * 160
    payload = payload.encode()
    phase = "capture200"
    for index in range(200):
        proxy(f"/load/{index}", payload)
        time.sleep(.03)
    phase = "loaded"
    time.sleep(5)
    screenshot("loaded200")
    assert database.execute("SELECT count(*) FROM sessions").fetchone()[0] == 203
    report["workload"] = {"sequential_posts": 200, "request_and_response_bytes_each": len(payload), "spacing_ms": 30, "stable_window_seconds": 3}
    phase = "shutdown"
    ui("close")
    app.wait(timeout=10)
    assert app.returncode == 0
    assert not (data / "control.json").exists()
    with socket.socket() as probe:
        probe.bind(("0.0.0.0", port))
    report["clean_shutdown"] = True
    for name in ["empty", "loaded"]:
        values = [sample for sample in samples if sample["phase"] == name]
        recent = [sample for sample in values if sample["time"] >= values[-1]["time"] - 3]
        report[name] = {key + "_mib": round(statistics.median(sample[key] for sample in recent) / 1024**2, 2) for key in ["footprint", "rss"]}
        report[name]["cpu_percent_one_core"] = round((recent[-1]["cpu_ns"] - recent[0]["cpu_ns"]) / ((recent[-1]["time"] - recent[0]["time"]) * 1e9) * 100, 2)
    report["sampled_peak_footprint_mib"] = round(max(sample["footprint"] for sample in samples) / 1024**2, 2)
    report["executable_bytes"] = args.executable.stat().st_size
    print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)
except Exception:
    try:
        screenshot("failure")
        (evidence / "failure-tree.json").write_text(tree())
    except Exception:
        pass
    raise
finally:
    done.set()
    if app.poll() is None:
        app.terminate()
        try:
            app.wait(timeout=5)
        except subprocess.TimeoutExpired:
            app.kill()
            app.wait()
    origin.shutdown()
    log.close()
    (evidence / "samples.json").write_text(json.dumps(samples))
    (evidence / "result.json").write_text(json.dumps(report, ensure_ascii=False, indent=2))
