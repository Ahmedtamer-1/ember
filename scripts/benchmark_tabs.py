#!/usr/bin/env python3
"""
Benchmark and Edge-Case Test Suite for Ember tab animations:
- Edge Cases: Rapid open/close spam, interrupted animations, closing all tabs,
  window resize mid-animation, tab suspension/restore, tab duplication,
  drag-to-reorder pointer tracking & sibling gaps, and reduced-motion toggle.
- Tab Scaling: Tab strip resizing and animation performance across 1, 5, 15, 30, and 60 tabs.
"""

import http.server
import json
import os
import subprocess
import sys
import threading
import time

PORT = 9876
BENCH_RESULTS = None
EDGE_RESULTS = None
SERVER = None

HTML_PATH = os.path.join(os.path.dirname(__file__), "..", "src", "ui", "chrome.html")

class BenchmarkHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/" or self.path.startswith("/?"):
            try:
                with open(HTML_PATH, "rb") as f:
                    content = f.read()
                self.send_response(200)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(content)))
                self.end_headers()
                self.wfile.write(content)
            except Exception as e:
                self.send_error(500, str(e))
        else:
            self.send_error(404)

    def do_POST(self):
        global BENCH_RESULTS, EDGE_RESULTS
        if self.path == "/api/bench-result":
            content_length = int(self.headers.get("Content-Length", 0))
            post_data = self.rfile.read(content_length)
            BENCH_RESULTS = json.loads(post_data.decode("utf-8"))
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"status":"ok"}')
        elif self.path == "/api/edge-case-result":
            content_length = int(self.headers.get("Content-Length", 0))
            post_data = self.rfile.read(content_length)
            EDGE_RESULTS = json.loads(post_data.decode("utf-8"))
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"status":"ok"}')
        else:
            self.send_error(404)

    def log_message(self, format, *args):
        pass

def run_server():
    global SERVER
    SERVER = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), BenchmarkHandler)
    SERVER.serve_forever()

def find_browser():
    edge_paths = [
        r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
        r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
    ]
    for p in edge_paths:
        if os.path.exists(p):
            return p

    chrome_paths = [
        r"C:\Program Files\Google\Chrome\Application\chrome.exe",
        r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    ]
    for p in chrome_paths:
        if os.path.exists(p):
            return p
    return None

def run_headless_test(browser_exe, query_param, timeout_secs=20):
    cmd = [
        browser_exe,
        "--headless=new",
        "--window-size=1180,800",
        "--disable-gpu",
        "--no-sandbox",
        "--disable-extensions",
        f"http://127.0.0.1:{PORT}/?{query_param}"
    ]
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    return proc

def main():
    global BENCH_RESULTS, EDGE_RESULTS
    browser_exe = find_browser()
    if not browser_exe:
        print("Error: No Edge or Chrome executable found for headless benchmark.")
        sys.exit(1)

    print(f"Using browser for test harness: {browser_exe}")

    server_thread = threading.Thread(target=run_server, daemon=True)
    server_thread.start()
    time.sleep(0.5)

    # 1. Run Edge Cases Test Suite
    print("\n[1/2] Running tab animation edge cases test suite...")
    proc_edge = run_headless_test(browser_exe, "test=edge-cases")
    start_wait = time.time()
    while EDGE_RESULTS is None and (time.time() - start_wait) < 20:
        time.sleep(0.2)
    try:
        proc_edge.terminate()
        proc_edge.wait(timeout=2)
    except Exception:
        proc_edge.kill()

    if EDGE_RESULTS is None:
        print("Error: Edge cases test suite timed out.")
        sys.exit(1)

    print("\n================ EDGE CASES TEST RESULTS ================\n")
    print("| Test Case | Status | Details |")
    print("| :--- | :---: | :--- |")
    all_passed = True
    for t in EDGE_RESULTS:
        status = "PASSED" if t["passed"] else "FAILED"
        if not t["passed"]:
            all_passed = False
        print(f"| {t['name']:36s} | {status:6s} | {t['details']} |")

    if not all_passed:
        print("\nError: One or more edge case tests failed.")
        sys.exit(1)

    # 2. Run Tab Scaling Benchmark
    print("\n[2/2] Running tab resizing benchmark across [1, 5, 15, 30, 60] tabs...")
    proc_bench = run_headless_test(browser_exe, "bench=tabs")
    start_wait = time.time()
    while BENCH_RESULTS is None and (time.time() - start_wait) < 30:
        time.sleep(0.2)
    try:
        proc_bench.terminate()
        proc_bench.wait(timeout=2)
    except Exception:
        proc_bench.kill()

    if BENCH_RESULTS is None:
        print("Error: Tab scaling benchmark timed out.")
        sys.exit(1)

    os.makedirs(os.path.join(os.path.dirname(__file__), "..", "target"), exist_ok=True)
    out_file = os.path.join(os.path.dirname(__file__), "..", "target", "tabs_benchmark.json")
    with open(out_file, "w") as f:
        json.dump({"edgeCases": EDGE_RESULTS, "benchmarks": BENCH_RESULTS}, f, indent=2)

    print("\n================ TAB SCALING BENCHMARK RESULTS ================\n")
    print("| Tabs | Width (px) | Title Opacity | Micro (<52px) | Overflow | Scroll Btns | Avg FPS | Avg Frame (ms) | Max Frame (ms) | Dropped (>24ms) |")
    print("| ---: | ---------: | ------------: | :-----------: | :------: | :---------: | ------: | -------------: | -------------: | --------------: |")
    for r in BENCH_RESULTS:
        micro_str = "Yes (Favicon only)" if r["microTab"] else "No"
        overflow_str = "Yes" if r["isOverflowing"] else "No"
        scroll_str = "Visible" if r["scrollButtonsVisible"] else "Hidden"
        print(f"| {r['tabCount']:4d} | {r['computedWidthPx']:10d} | {r['titleOpacity']:13.2f} | {micro_str:13s} | {overflow_str:8s} | {scroll_str:11s} | {r['fps']:7.1f} | {r['avgFrameTimeMs']:14.2f} | {r['maxFrameTimeMs']:14.2f} | {r['droppedFramesCount']:15d} |")

    print("\nAll edge cases passed and tab scaling benchmark completed successfully.")

if __name__ == "__main__":
    main()
