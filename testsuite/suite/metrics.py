"""Measurement helpers: latency percentiles and daemon resource sampling.

The stress lane's job is to produce numbers, not just pass/fail, so everything
here is designed to end up in the report even when the assertion passes.
"""

import os
import threading
import time

__all__ = [
    "percentile",
    "Latency",
    "ProcSampler",
    "slope",
    "human_bytes",
    "rss_kb",
    "thread_count",
    "fd_count",
]


def percentile(values, pct):
    """Linear-interpolated percentile of an unsorted sequence."""
    if not values:
        return 0.0
    ordered = sorted(values)
    if len(ordered) == 1:
        return float(ordered[0])
    rank = (len(ordered) - 1) * (pct / 100.0)
    low = int(rank)
    high = min(low + 1, len(ordered) - 1)
    frac = rank - low
    return ordered[low] * (1 - frac) + ordered[high] * frac


class Latency:
    """Records durations in milliseconds and summarizes them."""

    def __init__(self, label):
        self.label = label
        self.samples = []
        self.errors = 0

    def record(self, ms):
        self.samples.append(ms)

    def record_error(self):
        self.errors += 1

    def time(self, fn, *args, **kwargs):
        """Run `fn`, record how long it took, return its result."""
        start = time.perf_counter()
        try:
            return fn(*args, **kwargs)
        finally:
            self.record((time.perf_counter() - start) * 1000.0)

    def summary(self):
        return {
            "label": self.label,
            "count": len(self.samples),
            "errors": self.errors,
            "min_ms": round(min(self.samples), 2) if self.samples else 0.0,
            "p50_ms": round(percentile(self.samples, 50), 2),
            "p95_ms": round(percentile(self.samples, 95), 2),
            "p99_ms": round(percentile(self.samples, 99), 2),
            "max_ms": round(max(self.samples), 2) if self.samples else 0.0,
        }

    def describe(self):
        s = self.summary()
        return (
            f"{s['label']}: n={s['count']} errors={s['errors']} "
            f"p50={s['p50_ms']}ms p95={s['p95_ms']}ms p99={s['p99_ms']}ms max={s['max_ms']}ms"
        )


class ProcSampler:
    """Background sampler for a process's RSS, thread count and open FDs.

    Reads /proc directly rather than shelling out, so it stays cheap enough to
    run at 200ms during a flood without perturbing what it measures.
    """

    def __init__(self, pid, interval=0.25):
        self.pid = pid
        self.interval = interval
        self.samples = []  # (elapsed_s, rss_kb, threads, fds)
        self._stop = threading.Event()
        self._thread = None
        self._start = None

    def __enter__(self):
        self.start()
        return self

    def __exit__(self, *exc):
        self.stop()
        return False

    def start(self):
        self._start = time.perf_counter()
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    def stop(self):
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=2.0)

    def _run(self):
        while not self._stop.is_set():
            sample = self.sample()
            if sample is None:
                break
            self.samples.append(sample)
            self._stop.wait(self.interval)

    def sample(self):
        rss = rss_kb(self.pid)
        if rss is None:
            return None
        elapsed = time.perf_counter() - self._start if self._start else 0.0
        return (elapsed, rss, thread_count(self.pid), fd_count(self.pid))

    # -- reading -----------------------------------------------------------

    @property
    def rss_series(self):
        return [(s[0], s[1]) for s in self.samples]

    def peak_rss_kb(self):
        return max((s[1] for s in self.samples), default=0)

    def peak_threads(self):
        return max((s[2] for s in self.samples), default=0)

    def peak_fds(self):
        return max((s[3] for s in self.samples), default=0)

    def summary(self):
        # Always the same shape, even with no samples: /proc is absent on some
        # platforms, and callers index these keys directly. A zeroed summary is
        # a far better failure than a KeyError three frames away.
        if not self.samples:
            return {
                "samples": 0,
                "duration_s": 0,
                "rss_start_kb": 0,
                "rss_end_kb": 0,
                "rss_peak_kb": 0,
                "rss_slope_kb_per_min": 0.0,
                "threads_start": 0,
                "threads_end": 0,
                "threads_peak": 0,
                "fds_start": 0,
                "fds_end": 0,
                "fds_peak": 0,
            }
        first, last = self.samples[0], self.samples[-1]
        return {
            "samples": len(self.samples),
            "duration_s": round(last[0], 1),
            "rss_start_kb": first[1],
            "rss_end_kb": last[1],
            "rss_peak_kb": self.peak_rss_kb(),
            "rss_slope_kb_per_min": round(slope(self.rss_series) * 60, 1),
            "threads_start": first[2],
            "threads_end": last[2],
            "threads_peak": self.peak_threads(),
            "fds_start": first[3],
            "fds_end": last[3],
            "fds_peak": self.peak_fds(),
        }

    def describe(self):
        s = self.summary()
        if not s.get("samples"):
            return "no samples"
        return (
            f"rss {human_bytes(s['rss_start_kb'] * 1024)} -> "
            f"{human_bytes(s['rss_end_kb'] * 1024)} "
            f"(peak {human_bytes(s['rss_peak_kb'] * 1024)}, "
            f"slope {s['rss_slope_kb_per_min']} kB/min) · "
            f"threads {s['threads_start']}->{s['threads_end']} "
            f"(peak {s['threads_peak']}) · "
            f"fds {s['fds_start']}->{s['fds_end']} (peak {s['fds_peak']})"
        )


def slope(series):
    """Least-squares slope of (x, y) pairs — the leak detector.

    A healthy daemon under steady load has a slope near zero; a leak shows up
    as a positive slope that survives the whole soak window.
    """
    n = len(series)
    if n < 2:
        return 0.0
    mean_x = sum(p[0] for p in series) / n
    mean_y = sum(p[1] for p in series) / n
    num = sum((p[0] - mean_x) * (p[1] - mean_y) for p in series)
    den = sum((p[0] - mean_x) ** 2 for p in series)
    return num / den if den else 0.0


def human_bytes(n):
    step = float(n)
    for unit in ("B", "kB", "MB", "GB"):
        if step < 1024 or unit == "GB":
            return f"{step:.1f} {unit}" if unit != "B" else f"{int(step)} B"
        step /= 1024
    return f"{step:.1f} GB"


def rss_kb(pid):
    """Resident set size in kB, or None if the process is gone."""
    try:
        with open(f"/proc/{pid}/status") as fh:
            for line in fh:
                if line.startswith("VmRSS:"):
                    return int(line.split()[1])
    except OSError:
        return None
    return 0


def thread_count(pid):
    """OS threads. Every PTY pane costs two of these, so it is a load proxy."""
    try:
        return len(os.listdir(f"/proc/{pid}/task"))
    except OSError:
        return 0


def fd_count(pid):
    try:
        return len(os.listdir(f"/proc/{pid}/fd"))
    except OSError:
        return 0
