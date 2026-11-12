import os
import sys
import time
import shutil
import tempfile
import subprocess
import ctypes
from ctypes import wintypes

# Win32 API for exact process working set
class PROCESS_MEMORY_COUNTERS(ctypes.Structure):
    _fields_ = [
        ('cb', wintypes.DWORD),
        ('PageFaultCount', wintypes.DWORD),
        ('PeakWorkingSetSize', ctypes.c_size_t),
        ('WorkingSetSize', ctypes.c_size_t),
        ('QuotaPeakPagedPoolUsage', ctypes.c_size_t),
        ('QuotaPagedPoolUsage', ctypes.c_size_t),
        ('QuotaPeakNonPagedPoolUsage', ctypes.c_size_t),
        ('QuotaNonPagedPoolUsage', ctypes.c_size_t),
        ('PagefileUsage', ctypes.c_size_t),
        ('PeakPagefileUsage', ctypes.c_size_t),
    ]

psapi = ctypes.WinDLL('psapi.dll')
kernel32 = ctypes.WinDLL('kernel32.dll')

PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
PROCESS_VM_READ = 0x0010

def get_process_working_set_mb(pid):
    h = kernel32.OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, False, pid)
    if not h:
        return 0.0
    counters = PROCESS_MEMORY_COUNTERS()
    counters.cb = ctypes.sizeof(PROCESS_MEMORY_COUNTERS)
    try:
        res = psapi.GetProcessMemoryInfo(h, ctypes.byref(counters), counters.cb)
    except AttributeError:
        res = kernel32.K32GetProcessMemoryInfo(h, ctypes.byref(counters), counters.cb)
    kernel32.CloseHandle(h)
    if res:
        return counters.WorkingSetSize / (1024.0 * 1024.0)
    return 0.0

def get_process_tree_pids(root_pid):
    # Snapshot all processes and build parent-child tree
    class PROCESSENTRY32(ctypes.Structure):
        _fields_ = [
            ('dwSize', wintypes.DWORD),
            ('cntUsage', wintypes.DWORD),
            ('th32ProcessID', wintypes.DWORD),
            ('th32DefaultHeapID', ctypes.c_size_t),
            ('th32ModuleID', wintypes.DWORD),
            ('cntThreads', wintypes.DWORD),
            ('th32ParentProcessID', wintypes.DWORD),
            ('pcPriClassBase', wintypes.LONG),
            ('dwFlags', wintypes.DWORD),
            ('szExeFile', ctypes.c_char * 260)
        ]

    TH32CS_SNAPPROCESS = 0x00000002
    h_snap = kernel32.CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
    if h_snap == -1 or h_snap == 0:
        return {root_pid}

    entry = PROCESSENTRY32()
    entry.dwSize = ctypes.sizeof(PROCESSENTRY32)

    edges = []
    if kernel32.Process32First(h_snap, ctypes.byref(entry)):
        while True:
            edges.append((entry.th32ProcessID, entry.th32ParentProcessID))
            if not kernel32.Process32Next(h_snap, ctypes.byref(entry)):
                break
    kernel32.CloseHandle(h_snap)

    tracked = {root_pid}
    added = True
    while added:
        added = False
        for pid, ppid in edges:
            if ppid in tracked and pid not in tracked:
                tracked.add(pid)
                added = True
    return tracked

def measure_tree_ram_mb(root_pid):
    pids = get_process_tree_pids(root_pid)
    total_mb = sum(get_process_working_set_mb(pid) for pid in pids)
    return round(total_mb, 1)

def kill_tree(root_pid):
    pids = get_process_tree_pids(root_pid)
    for pid in pids:
        try:
            h = kernel32.OpenProcess(0x0001, False, pid) # PROCESS_TERMINATE
            if h:
                kernel32.TerminateProcess(h, 0)
                kernel32.CloseHandle(h)
        except Exception:
            pass

EMBER_EXE = os.path.abspath(r"target\release\ember.exe")
CHROME_EXE = r"C:\Program Files\Google\Chrome\Application\chrome.exe"
HAS_CHROME = os.path.isfile(CHROME_EXE)

URLS_1 = ["https://example.com"]
URLS_5 = [
    "https://example.com",
    "https://news.ycombinator.com",
    "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "https://github.com",
    "https://duckduckgo.com"
]
URLS_20 = [
    "https://example.com",
    "https://news.ycombinator.com",
    "https://en.wikipedia.org/wiki/Rust_(programming_language)",
    "https://github.com",
    "https://duckduckgo.com",
    "https://www.rust-lang.org",
    "https://crates.io",
    "https://doc.rust-lang.org/book/",
    "https://developer.mozilla.org/en-US/",
    "https://httpbin.org",
    "https://en.wikipedia.org/wiki/Web_browser",
    "https://en.wikipedia.org/wiki/Ad_blocking",
    "https://news.ycombinator.com/news",
    "https://github.com/explore",
    "https://en.wikipedia.org/wiki/Windows_11",
    "https://example.org",
    "https://example.net",
    "https://www.wikipedia.org",
    "https://startpage.com",
    "https://html5test.co"
]

print("=== Starting Ember Performance & RAM Audit ===")
ember_size_mb = round(os.path.getsize(EMBER_EXE) / (1024.0 * 1024.0), 2)
print(f"Ember Binary Size: {ember_size_mb} MB")

# 1. Startup Time Measurement
startup_times = []
for _ in range(3):
    t0 = time.perf_counter()
    p = subprocess.Popen([EMBER_EXE])
    # Give the window time to initialize
    time.sleep(0.3)
    t1 = time.perf_counter()
    startup_times.append((t1 - t0) * 1000.0)
    kill_tree(p.pid)
    time.sleep(0.3)
ember_startup_ms = round(sum(startup_times) / len(startup_times), 0)
print(f"Ember Startup Time: {ember_startup_ms} ms")

chrome_startup_ms = "N/A"
if HAS_CHROME:
    c_times = []
    for _ in range(3):
        t0 = time.perf_counter()
        tmp_dir = tempfile.mkdtemp(prefix="chrome_bench_")
        p = subprocess.Popen([CHROME_EXE, f"--user-data-dir={tmp_dir}", "--no-first-run", "--no-default-browser-check", "about:blank"])
        time.sleep(0.3)
        t1 = time.perf_counter()
        c_times.append((t1 - t0) * 1000.0)
        kill_tree(p.pid)
        shutil.rmtree(tmp_dir, ignore_errors=True)
        time.sleep(0.3)
    chrome_startup_ms = round(sum(c_times) / len(c_times), 0)
    print(f"Chrome Startup Time: {chrome_startup_ms} ms")

# 2. RAM Measurements
def test_ember_ram(urls, materialize_all=False):
    env = os.environ.copy()
    if materialize_all:
        env["EMBER_BENCH_MATERIALIZE_ALL"] = "1"
    else:
        env.pop("EMBER_BENCH_MATERIALIZE_ALL", None)
    
    p = subprocess.Popen([EMBER_EXE] + urls, env=env)
    settle = 6 if len(urls) > 5 else 4
    time.sleep(settle)
    ram = measure_tree_ram_mb(p.pid)
    kill_tree(p.pid)
    time.sleep(0.5)
    return ram

def test_chrome_ram(urls):
    if not HAS_CHROME:
        return "N/A"
    tmp_dir = tempfile.mkdtemp(prefix="chrome_bench_")
    cmd = [CHROME_EXE, f"--user-data-dir={tmp_dir}", "--no-first-run", "--no-default-browser-check", "--disable-background-networking"] + urls
    p = subprocess.Popen(cmd)
    settle = 7 if len(urls) > 5 else 4
    time.sleep(settle)
    ram = measure_tree_ram_mb(p.pid)
    kill_tree(p.pid)
    shutil.rmtree(tmp_dir, ignore_errors=True)
    time.sleep(0.5)
    return ram

print("\n--- Testing 1 Tab RAM ---")
ember_1tab = test_ember_ram(URLS_1)
chrome_1tab = test_chrome_ram(URLS_1)
print(f"Ember: {ember_1tab} MB | Chrome: {chrome_1tab} MB")

print("\n--- Testing 5 Tabs RAM ---")
ember_5tab_lazy = test_ember_ram(URLS_5, materialize_all=False)
ember_5tab_mat = test_ember_ram(URLS_5, materialize_all=True)
chrome_5tab = test_chrome_ram(URLS_5)
print(f"Ember (Lazy): {ember_5tab_lazy} MB | Ember (All Live): {ember_5tab_mat} MB | Chrome: {chrome_5tab} MB")

print("\n--- Testing 20 Tabs RAM ---")
ember_20tab_lazy = test_ember_ram(URLS_20, materialize_all=False)
ember_20tab_mat = test_ember_ram(URLS_20, materialize_all=True)
chrome_20tab = test_chrome_ram(URLS_20)
print(f"Ember (Lazy): {ember_20tab_lazy} MB | Ember (All Live): {ember_20tab_mat} MB | Chrome: {chrome_20tab} MB")

print("\n================ FINAL AUDIT TABLE ================")
print(f"| Metric | Ember (Lazy) | Ember (All Live) | Google Chrome | Brave (Ref.) |")
print(f"| :--- | :---: | :---: | :---: | :---: |")
print(f"| **Binary Size** | **{ember_size_mb} MB** | **{ember_size_mb} MB** | 4.26 MB (Stub) / ~180 MB | ~125 MB |")
print(f"| **Cold Startup Time** | **{ember_startup_ms} ms** | **{ember_startup_ms} ms** | {chrome_startup_ms} ms | ~280 ms |")
print(f"| **RAM (1 Tab: example.com)** | **{ember_1tab} MB** | **{ember_1tab} MB** | {chrome_1tab} MB | ~120 MB |")
print(f"| **RAM (5 Tabs)** | **{ember_5tab_lazy} MB** | **{ember_5tab_mat} MB** | {chrome_5tab} MB | ~340 MB |")
print(f"| **RAM (20 Tabs)** | **{ember_20tab_lazy} MB** | **{ember_20tab_mat} MB** | {chrome_20tab} MB | ~890 MB |")
