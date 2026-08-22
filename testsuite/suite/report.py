"""A single-file HTML report, written next to results.json.

No assets, no CDN, no build step — same rule the `web/` client follows, so the
report opens from a file:// URL or as a CI artifact without anything else.
"""

import html
import os
import time

STATUS_STYLE = {
    "pass": ("ok", "PASS"),
    "fail": ("bad", "FAIL"),
    "error": ("bad", "ERROR"),
    "timeout": ("bad", "TIMEOUT"),
    "xfail": ("warn", "KNOWN GAP"),
    "xpass": ("info", "NOW PASSING"),
    "skip": ("muted", "SKIP"),
}

CSS = """
:root {
  color-scheme: light dark;
  --bg: #ffffff; --fg: #14171a; --muted: #5b6570; --line: #e3e7ea;
  --card: #f7f9fa; --ok: #157f3d; --bad: #c0272d; --warn: #96650a;
  --info: #6c3fb5; --accent: #2f6feb; --mono: ui-monospace, SFMono-Regular,
  "SF Mono", Menlo, Consolas, monospace;
}
@media (prefers-color-scheme: dark) {
  :root { --bg: #0f1214; --fg: #e6e9ec; --muted: #9aa4ae; --line: #262c31;
    --card: #171b1f; --ok: #4fc07a; --bad: #ff6b6b; --warn: #e3b341;
    --info: #b392f0; --accent: #6ea8fe; }
}
:root[data-theme="dark"] {
  --bg: #0f1214; --fg: #e6e9ec; --muted: #9aa4ae; --line: #262c31;
  --card: #171b1f; --ok: #4fc07a; --bad: #ff6b6b; --warn: #e3b341;
  --info: #b392f0; --accent: #6ea8fe;
}
:root[data-theme="light"] {
  --bg: #ffffff; --fg: #14171a; --muted: #5b6570; --line: #e3e7ea;
  --card: #f7f9fa; --ok: #157f3d; --bad: #c0272d; --warn: #96650a;
  --info: #6c3fb5; --accent: #2f6feb;
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg);
  font: 15px/1.55 -apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif; }
.wrap { max-width: 1100px; margin: 0 auto; padding: 2rem 1.25rem 5rem; }
h1 { font-size: 1.6rem; margin: 0 0 .25rem; letter-spacing: -.01em; }
h2 { font-size: 1.05rem; margin: 2.5rem 0 .75rem; text-transform: uppercase;
  letter-spacing: .08em; color: var(--muted); }
.sub { color: var(--muted); margin: 0 0 1.5rem; }
.tiles { display: grid; gap: .6rem; grid-template-columns: repeat(auto-fit, minmax(110px, 1fr)); }
.tile { background: var(--card); border: 1px solid var(--line); border-radius: 10px;
  padding: .75rem .9rem; }
.tile .n { font: 600 1.5rem/1.1 var(--mono); }
.tile .l { color: var(--muted); font-size: .74rem; text-transform: uppercase;
  letter-spacing: .07em; margin-top: .2rem; }
.verdict { display: inline-block; padding: .3rem .8rem; border-radius: 999px;
  font: 600 .8rem/1 var(--mono); letter-spacing: .06em; }
.verdict.ok { background: color-mix(in srgb, var(--ok) 16%, transparent); color: var(--ok); }
.verdict.bad { background: color-mix(in srgb, var(--bad) 16%, transparent); color: var(--bad); }
.scroll { overflow-x: auto; border: 1px solid var(--line); border-radius: 10px; }
table { border-collapse: collapse; width: 100%; font-size: .88rem; }
th, td { text-align: left; padding: .45rem .7rem; border-bottom: 1px solid var(--line);
  white-space: nowrap; }
th { color: var(--muted); font-weight: 600; font-size: .74rem;
  text-transform: uppercase; letter-spacing: .06em; }
tr:last-child td { border-bottom: none; }
td.wrap-cell { white-space: normal; min-width: 18rem; }
code, .mono { font-family: var(--mono); font-size: .86em; }
.tag { font: 600 .68rem/1 var(--mono); padding: .25rem .45rem; border-radius: 5px;
  letter-spacing: .04em; }
.tag.ok { background: color-mix(in srgb, var(--ok) 16%, transparent); color: var(--ok); }
.tag.bad { background: color-mix(in srgb, var(--bad) 16%, transparent); color: var(--bad); }
.tag.warn { background: color-mix(in srgb, var(--warn) 18%, transparent); color: var(--warn); }
.tag.info { background: color-mix(in srgb, var(--info) 16%, transparent); color: var(--info); }
.tag.muted { background: color-mix(in srgb, var(--muted) 15%, transparent); color: var(--muted); }
.gap { background: var(--card); border: 1px solid var(--line); border-left: 3px solid var(--warn);
  border-radius: 8px; padding: .7rem .9rem; margin-bottom: .6rem; }
.gap .who { font: 600 .85rem var(--mono); }
.gap .why { color: var(--muted); font-size: .88rem; margin-top: .2rem; }
pre { background: var(--card); border: 1px solid var(--line); border-radius: 8px;
  padding: .8rem; overflow-x: auto; font-family: var(--mono); font-size: .8rem;
  margin: .4rem 0 0; }
details summary { cursor: pointer; color: var(--accent); font-size: .84rem; }
.bar { height: 6px; border-radius: 3px; background: color-mix(in srgb, var(--muted) 22%, transparent);
  overflow: hidden; min-width: 90px; }
.bar > i { display: block; height: 100%; background: var(--ok); }
.bar.partial > i { background: var(--bad); }
"""


def write_html(summary, out_dir, filename="report.html"):
    path = os.path.join(out_dir, filename)
    with open(path, "w") as fh:
        fh.write(_render(summary))
    return path


def _render(s):
    counts = s["counts"]
    ok = s["ok"]
    when = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(s.get("started") or time.time()))
    parts = [
        "<title>butai testsuite report</title>",
        f"<style>{CSS}</style>",
        '<div class="wrap">',
        "<h1>butai testsuite</h1>",
        f'<p class="sub">profile <code>{e(s["profile"])}</code> · {e(when)} · '
        f'{s["duration"]:.1f}s · <span class="verdict {"ok" if ok else "bad"}">'
        f'{"PASS" if ok else "FAIL"}</span></p>',
        _tiles(counts),
        _gaps(s),
        _failures(s),
        _coverage(s),
        _tables(s),
        _observations(s),
        _results(s),
        "</div>",
    ]
    return "\n".join(p for p in parts if p)


def _tiles(counts):
    order = [
        ("pass", "passed"),
        ("fail", "failed"),
        ("error", "errors"),
        ("timeout", "timeouts"),
        ("xfail", "known gaps"),
        ("xpass", "now passing"),
        ("skip", "skipped"),
    ]
    tiles = "".join(
        f'<div class="tile"><div class="n">{counts.get(k, 0)}</div><div class="l">{label}</div></div>'
        for k, label in order
    )
    return f'<div class="tiles">{tiles}</div>'


def _gaps(s):
    gaps = [r for r in s["results"] if r["status"] in ("xfail", "xpass")]
    if not gaps:
        return ""
    out = ["<h2>Known gaps</h2>"]
    for r in gaps:
        state = "confirmed" if r["status"] == "xfail" else "now passing — remove the xfail"
        out.append(
            f'<div class="gap"><div class="who">{e(r["full_name"])} '
            f'<span class="tag {"warn" if r["status"] == "xfail" else "info"}">{e(state)}</span>'
            f'</div><div class="why">{e(r["xfail"] or "")}</div></div>'
        )
    return "\n".join(out)


def _failures(s):
    bad = [r for r in s["results"] if r["status"] in ("fail", "error", "timeout")]
    if not bad:
        return ""
    out = ["<h2>Failures</h2>"]
    for r in bad:
        out.append(
            f'<div class="gap" style="border-left-color:var(--bad)">'
            f'<div class="who">{e(r["full_name"])} '
            f'<span class="tag bad">{e(r["status"])}</span></div>'
            f'<div class="why">{e(r["message"])}</div>'
        )
        if r.get("detail"):
            out.append(
                f"<details><summary>traceback</summary><pre>{e(r['detail'])}</pre></details>"
            )
        out.append("</div>")
    return "\n".join(out)


def _coverage(s):
    rows = []
    for group, data in s["coverage"].items():
        pct = 100 * data["covered"] / data["total"] if data["total"] else 100
        klass = "" if not data["missing"] else " partial"
        missing = (
            "<br>".join(f"<code>{e(m)}</code>" for m in data["missing"])
            if data["missing"]
            else '<span class="tag ok">complete</span>'
        )
        rows.append(
            f"<tr><td>{e(group)}</td>"
            f'<td class="mono">{data["covered"]}/{data["total"]}</td>'
            f'<td><div class="bar{klass}"><i style="width:{pct:.0f}%"></i></div></td>'
            f'<td class="wrap-cell">{missing}</td></tr>'
        )
    note = (
        ""
        if s.get("coverage_enforced")
        else '<p class="sub">Not enforced for this profile or filter — a partial run '
        "legitimately touches a subset.</p>"
    )
    return (
        "<h2>API coverage</h2>"
        + note
        + '<div class="scroll"><table><thead><tr><th>surface</th><th>covered</th>'
        "<th></th><th>not exercised</th></tr></thead><tbody>"
        + "".join(rows)
        + "</tbody></table></div>"
    )


def _tables(s):
    out = []
    for name, rows in sorted(s.get("tables", {}).items()):
        if not rows:
            continue
        columns = []
        for row in rows:
            for k in row:
                if k not in columns:
                    columns.append(k)
        head = "".join(f"<th>{e(c)}</th>" for c in columns)
        body = "".join(
            "<tr>" + "".join(f"<td>{_cell(r.get(c, ''))}</td>" for c in columns) + "</tr>"
            for r in rows
        )
        out.append(
            f"<h2>{e(name)}</h2><div class='scroll'><table><thead><tr>{head}</tr>"
            f"</thead><tbody>{body}</tbody></table></div>"
        )
    return "\n".join(out)


def _cell(value):
    text = str(value)
    lowered = text.lower()
    if lowered in ("ok", "yes", "detected", "true"):
        return f'<span class="tag ok">{e(text)}</span>'
    if lowered in ("missed", "no", "false", "broken"):
        return f'<span class="tag bad">{e(text)}</span>'
    return e(text)


def _observations(s):
    items = [(r["full_name"], n) for r in s["results"] for n in r.get("notes", [])]
    metrics = [(r["full_name"], r["metrics"]) for r in s["results"] if r.get("metrics")]
    if not items and not metrics:
        return ""
    out = ["<h2>Observations</h2><div class='scroll'><table><tbody>"]
    for name, note in items:
        out.append(f'<tr><td class="mono">{e(name)}</td><td class="wrap-cell">{e(note)}</td></tr>')
    for name, values in metrics:
        for k, v in values.items():
            out.append(
                f'<tr><td class="mono">{e(name)}</td>'
                f'<td class="wrap-cell"><code>{e(k)}</code> = {e(str(v))}</td></tr>'
            )
    out.append("</tbody></table></div>")
    return "\n".join(out)


def _results(s):
    rows = []
    for r in s["results"]:
        klass, label = STATUS_STYLE.get(r["status"], ("muted", r["status"]))
        rows.append(
            f'<tr><td><span class="tag {klass}">{e(label)}</span></td>'
            f'<td class="mono">{e(r["full_name"])}</td>'
            f'<td class="mono">{r["duration"]:.2f}s</td>'
            f'<td>{e(r["profile"])}</td>'
            f'<td class="mono">{e(", ".join(r["tags"]))}</td>'
            f'<td class="wrap-cell">{e(r["message"])}</td></tr>'
        )
    return (
        "<h2>All tests</h2><div class='scroll'><table><thead><tr><th></th><th>test</th>"
        "<th>time</th><th>profile</th><th>tags</th><th>detail</th></tr></thead><tbody>"
        + "".join(rows)
        + "</tbody></table></div>"
    )


def e(text):
    return html.escape(str(text), quote=True)
