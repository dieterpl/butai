"""The test runner: registration, selection, isolation, reporting.

Deliberately small and dependency-free. It gives us the four things a suite
like this needs and that a bare `assert` script does not: profiles (so one
suite is both a 60-second gate and a 30-minute soak), hard per-test timeouts
(a wedged PTY must fail a test, not hang the run), `xfail` (so behaviour butai
gets wrong today is *reported* rather than either hidden or permanently red),
and coverage keys (so an untested route is a failure).
"""

import argparse
import fnmatch
import importlib
import json
import os
import pkgutil
import shutil
import signal
import sys
import tempfile
import time
import traceback

from . import coverage as coverage_spec
from .daemon import Config, Daemon

__all__ = ["test", "Skip", "Context", "Runner", "main"]

PROFILE_ORDER = {"smoke": 0, "standard": 1, "soak": 2}

PASS = "pass"
FAIL = "fail"
ERROR = "error"
SKIP = "skip"
XFAIL = "xfail"
XPASS = "xpass"
TIMEOUT = "timeout"

BAD_STATUSES = {FAIL, ERROR, TIMEOUT}

REGISTRY = []


class Skip(Exception):
    """Raised to skip a test with a reason (missing tool, missing lane)."""


class _Timeout(Exception):
    pass


class Test:
    def __init__(self, fn, profile, tags, xfail, timeout, name):
        self.fn = fn
        self.profile = profile
        self.tags = tuple(tags)
        self.xfail = xfail
        self.timeout = timeout
        self.name = name or fn.__name__
        self.module = fn.__module__.rsplit(".", 1)[-1]

    @property
    def full_name(self):
        return f"{self.module}::{self.name}"


def test(profile="standard", tags=(), xfail=None, timeout=180, name=None):
    """Register a test.

    `xfail` is a sentence explaining behaviour that is currently wrong. The test
    still runs; failing is expected and reported under KNOWN GAPS, while passing
    is flagged as XPASS so the note gets deleted when butai is fixed.
    """
    if profile not in PROFILE_ORDER:
        raise ValueError(f"unknown profile {profile!r}")

    def decorate(fn):
        REGISTRY.append(Test(fn, profile, tags, xfail, timeout, name))
        return fn

    return decorate


class Context:
    """Per-test services: temp space, daemons, coverage, and report data."""

    def __init__(self, runner, current):
        self.runner = runner
        self.test = current
        self._tmp = None
        self._daemons = []
        self.notes = []
        self.metrics = {}

    # -- resources ---------------------------------------------------------

    @property
    def tmp(self):
        if self._tmp is None:
            self._tmp = tempfile.mkdtemp(prefix=f"butai-t-{self.test.name[:24]}-")
        return self._tmp

    def daemon(self, config=None, name=None, env=None, start=True, **kw):
        """A daemon isolated in its own HOME, stopped when the test ends."""
        d = Daemon(
            name=name or f"{self.test.name[:20]}-{len(self._daemons)}",
            config=config if config is not None else Config(),
            env=env,
            **kw,
        )
        self._daemons.append(d)
        if start:
            d.start()
        return d

    def cleanup(self):
        for d in reversed(self._daemons):
            try:
                d.stop()
            except Exception:
                pass
        if self._tmp and not os.environ.get("BUTAI_KEEP_TMP"):
            shutil.rmtree(self._tmp, ignore_errors=True)

    # -- reporting ---------------------------------------------------------

    def cover(self, *keys):
        for k in keys:
            self.runner.covered.add(k)

    def note(self, text):
        self.notes.append(str(text))

    def metric(self, key, value):
        self.metrics[key] = value

    def row(self, table, **fields):
        """Append a row to a named report table (the compat matrices)."""
        self.runner.tables.setdefault(table, []).append(fields)

    def skip(self, reason):
        raise Skip(reason)

    def require(self, condition, reason):
        if not condition:
            raise Skip(reason)

    def require_tool(self, name):
        if shutil.which(name) is None:
            raise Skip(f"{name} is not installed in this image")
        return name

    # -- knobs -------------------------------------------------------------

    @property
    def soak_seconds(self):
        return self.runner.soak_seconds

    @property
    def scale(self):
        """Load multiplier, so the same stress test is cheap in smoke."""
        return self.runner.scale


class Runner:
    def __init__(self, profile="standard", filters=(), out_dir="out", soak_seconds=1800, scale=1.0):
        self.profile = profile
        self.filters = tuple(filters)
        self.out_dir = out_dir
        self.soak_seconds = soak_seconds
        self.scale = scale
        self.covered = set()
        self.tables = {}
        self.results = []
        self.started = None

    # -- discovery ---------------------------------------------------------

    def discover(self, package="tests"):
        root = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), package)
        if root not in sys.path:
            sys.path.insert(0, os.path.dirname(root))
        found = []
        for mod in sorted(pkgutil.iter_modules([root]), key=lambda m: m.name):
            if mod.name.startswith("_"):
                continue
            importlib.import_module(f"{package}.{mod.name}")
            found.append(mod.name)
        return found

    def selected(self):
        limit = PROFILE_ORDER[self.profile]
        out = []
        for t in REGISTRY:
            if PROFILE_ORDER[t.profile] > limit:
                continue
            if self.filters and not any(_matches(t, f) for f in self.filters):
                continue
            out.append(t)
        return out

    # -- execution ---------------------------------------------------------

    def run(self):
        self.started = time.time()
        tests = self.selected()
        width = max((len(t.full_name) for t in tests), default=10)
        print(
            f"butai testsuite · profile={self.profile} · {len(tests)} tests"
            + (f" · filters={list(self.filters)}" if self.filters else "")
        )
        print("=" * (width + 34))
        for t in tests:
            result = self._run_one(t)
            self.results.append(result)
            print(
                f"{_badge(result['status'])} {t.full_name:<{width}}  {result['duration']:6.2f}s"
                + (f"  {result['message']}" if result["message"] else "")
            )
            sys.stdout.flush()
        return self.summary()

    def _run_one(self, t):
        ctx = Context(self, t)
        start = time.perf_counter()
        status, message = PASS, ""
        detail = ""
        previous = signal.signal(signal.SIGALRM, _raise_timeout)
        signal.setitimer(signal.ITIMER_REAL, t.timeout)
        try:
            t.fn(ctx)
        except Skip as e:
            status, message = SKIP, str(e)
        except _Timeout:
            status, message = TIMEOUT, f"exceeded {t.timeout}s"
            detail = "".join(traceback.format_exc())
        except AssertionError as e:
            status, message = FAIL, _first_line(e)
            detail = "".join(traceback.format_exc())
        except Exception as e:
            status, message = ERROR, f"{type(e).__name__}: {_first_line(e)}"
            detail = "".join(traceback.format_exc())
        finally:
            signal.setitimer(signal.ITIMER_REAL, 0)
            signal.signal(signal.SIGALRM, previous)
            try:
                ctx.cleanup()
            except Exception:
                pass

        if t.xfail:
            if status in BAD_STATUSES:
                status = XFAIL
                message = t.xfail
            elif status == PASS:
                status = XPASS
                message = f"expected to fail but passed — remove the xfail: {t.xfail}"

        return {
            "name": t.name,
            "module": t.module,
            "full_name": t.full_name,
            "profile": t.profile,
            "tags": list(t.tags),
            "status": status,
            "duration": time.perf_counter() - start,
            "message": message,
            "detail": detail,
            "notes": ctx.notes,
            "metrics": ctx.metrics,
            "xfail": t.xfail,
        }

    # -- reporting ---------------------------------------------------------

    def coverage_report(self):
        groups = {}
        for group, keys in coverage_spec.expected().items():
            missing = sorted(k for k in keys if k not in self.covered)
            groups[group] = {
                "total": len(keys),
                "covered": len(keys) - len(missing),
                "missing": missing,
            }
        return groups

    def summary(self):
        counts = {}
        for r in self.results:
            counts[r["status"]] = counts.get(r["status"], 0) + 1
        cov = self.coverage_report()
        # Coverage only means something when the whole suite ran; a filtered or
        # smoke run legitimately touches a subset.
        enforce = self.profile != "smoke" and not self.filters
        missing_total = sum(len(g["missing"]) for g in cov.values())
        return {
            "profile": self.profile,
            "filters": list(self.filters),
            "started": self.started,
            "duration": time.time() - self.started if self.started else 0,
            "counts": counts,
            "results": self.results,
            "coverage": cov,
            "coverage_enforced": enforce,
            "tables": self.tables,
            "ok": not any(r["status"] in BAD_STATUSES for r in self.results)
            and not (enforce and missing_total),
        }

    def write(self, summary):
        os.makedirs(self.out_dir, exist_ok=True)
        json_path = os.path.join(self.out_dir, "results.json")
        with open(json_path, "w") as fh:
            json.dump(summary, fh, indent=2, default=str)
        from .report import write_html

        html_path = write_html(summary, self.out_dir)
        return json_path, html_path


def _matches(t, pattern):
    return (
        fnmatch.fnmatch(t.full_name, f"*{pattern}*")
        or pattern in t.tags
        or fnmatch.fnmatch(t.module, f"*{pattern}*")
    )


def _raise_timeout(signum, frame):
    raise _Timeout()


def _first_line(exc):
    text = str(exc).strip()
    return text.splitlines()[0] if text else exc.__class__.__name__


_BADGES = {
    PASS: "\033[32m PASS \033[0m",
    FAIL: "\033[31m FAIL \033[0m",
    ERROR: "\033[31mERROR \033[0m",
    SKIP: "\033[90m SKIP \033[0m",
    XFAIL: "\033[33mXFAIL \033[0m",
    XPASS: "\033[35mXPASS \033[0m",
    TIMEOUT: "\033[31mTIME  \033[0m",
}


def _badge(status):
    if not sys.stdout.isatty() or os.environ.get("NO_COLOR"):
        return f"{status.upper():<6}"
    return _BADGES.get(status, status)


def print_summary(summary):
    counts = summary["counts"]
    print()
    print("=" * 72)
    order = [PASS, FAIL, ERROR, TIMEOUT, XFAIL, XPASS, SKIP]
    line = "  ".join(f"{s}={counts.get(s, 0)}" for s in order if counts.get(s))
    print(f"{line}   ({summary['duration']:.1f}s total)")

    failures = [r for r in summary["results"] if r["status"] in BAD_STATUSES]
    if failures:
        print("\nFAILURES")
        print("-" * 72)
        for r in failures:
            print(f"\n{r['full_name']} — {r['message']}")
            if r["detail"]:
                for ln in r["detail"].strip().splitlines()[-14:]:
                    print(f"    {ln}")

    gaps = [r for r in summary["results"] if r["status"] in (XFAIL, XPASS)]
    if gaps:
        print("\nKNOWN GAPS")
        print("-" * 72)
        for r in gaps:
            marker = "confirmed" if r["status"] == XFAIL else "NOW PASSING"
            print(f"  [{marker}] {r['full_name']}")
            print(f"      {r['xfail']}")

    notes = [(r["full_name"], n) for r in summary["results"] for n in r["notes"]]
    if notes:
        print("\nOBSERVATIONS")
        print("-" * 72)
        for name, note in notes:
            print(f"  {name}: {note}")

    for table, rows in sorted(summary["tables"].items()):
        if not rows:
            continue
        print(f"\n{table.upper()}")
        print("-" * 72)
        _print_table(rows)

    cov = summary["coverage"]
    print("\nAPI COVERAGE")
    print("-" * 72)
    for group, data in cov.items():
        mark = "ok" if not data["missing"] else "MISSING"
        print(f"  {group:<20} {data['covered']:>3}/{data['total']:<3} {mark}")
        for key in data["missing"]:
            print(f"      - {key}")
    if not summary["coverage_enforced"]:
        print("  (coverage not enforced for this profile/filter)")

    print()
    print("PASS" if summary["ok"] else "FAIL")
    print("=" * 72)


def _print_table(rows):
    columns = []
    for row in rows:
        for k in row:
            if k not in columns:
                columns.append(k)
    widths = {c: max(len(c), *(len(str(r.get(c, ""))) for r in rows)) for c in columns}
    print("  " + "  ".join(c.ljust(widths[c]) for c in columns))
    print("  " + "  ".join("-" * widths[c] for c in columns))
    for row in rows:
        print("  " + "  ".join(str(row.get(c, "")).ljust(widths[c]) for c in columns))


def main(argv=None):
    parser = argparse.ArgumentParser(prog="butai-testsuite")
    parser.add_argument(
        "profile",
        nargs="?",
        default=os.environ.get("BUTAI_PROFILE", "standard"),
        choices=sorted(PROFILE_ORDER, key=PROFILE_ORDER.get),
        help="smoke (fast gate) < standard (default) < soak (adds long-running tests)",
    )
    parser.add_argument("--filter", action="append", default=[], help="substring, tag or module")
    parser.add_argument("--out", default=os.environ.get("BUTAI_OUT", "out"))
    parser.add_argument("--minutes", type=float, default=None, help="soak duration")
    parser.add_argument("--scale", type=float, default=None, help="stress load multiplier")
    parser.add_argument("--list", action="store_true", help="list selected tests and exit")
    args = parser.parse_args(argv)

    soak = (args.minutes or float(os.environ.get("BUTAI_SOAK_MINUTES", "30"))) * 60
    scale = args.scale if args.scale is not None else float(os.environ.get("BUTAI_SCALE", "1"))
    runner = Runner(
        profile=args.profile,
        filters=tuple(args.filter),
        out_dir=args.out,
        soak_seconds=soak,
        scale=scale,
    )
    runner.discover()

    if args.list:
        for t in runner.selected():
            print(f"{t.profile:<9} {t.full_name}  tags={','.join(t.tags) or '-'}")
        return 0

    summary = runner.run()
    print_summary(summary)
    try:
        json_path, html_path = runner.write(summary)
        print(f"\nreport: {html_path}\n  json: {json_path}")
    except Exception as e:
        print(f"\n(could not write report: {e})")
    return 0 if summary["ok"] else 1


# Deliberately no `if __name__ == "__main__"` here: see suite/__main__.py.
