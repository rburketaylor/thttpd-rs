#!/usr/bin/env python3
"""HTML report generator for golden master and differential test results.

Usage:
    # Report from baseline only (shows captured responses)
    python3 pipeline/generate_report.py --baseline PATH --output PATH

    # Report from differential run (shows pass/fail with diffs)
    python3 pipeline/generate_report.py --baseline PATH --diff-results PATH --output PATH
"""

import argparse
import json
import os
import sys

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
PROJECT_ROOT = os.path.join(SCRIPT_DIR, "..")


# ---------------------------------------------------------------------------
# HTML generation
# ---------------------------------------------------------------------------

HTML_TEMPLATE = """<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>thttpd-rs Test Report</title>
<style>
  body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
         margin: 0; padding: 20px; background: #f5f5f5; color: #333; }}
  h1 {{ color: #2c3e50; }}
  h2 {{ color: #34495e; margin-top: 30px; }}
  .summary {{ display: flex; gap: 20px; margin: 20px 0; }}
  .summary-box {{ background: white; border-radius: 8px; padding: 16px 24px;
                  box-shadow: 0 1px 3px rgba(0,0,0,0.1); }}
  .summary-box .num {{ font-size: 2em; font-weight: bold; }}
  .summary-box .label {{ color: #7f8c8d; font-size: 0.9em; }}
  .pass {{ color: #27ae60; }}
  .fail {{ color: #e74c3c; }}
  .neutral {{ color: #2980b9; }}
  table {{ border-collapse: collapse; width: 100%; background: white;
           border-radius: 8px; overflow: hidden;
           box-shadow: 0 1px 3px rgba(0,0,0,0.1); margin-bottom: 20px; }}
  th {{ background: #2c3e50; color: white; text-align: left; padding: 12px; }}
  td {{ padding: 10px 12px; border-bottom: 1px solid #ecf0f1; }}
  tr:hover {{ background: #f8f9fa; }}
  code {{ background: #ecf0f1; padding: 2px 6px; border-radius: 3px;
          font-size: 0.9em; }}
  .detail {{ background: #fff3cd; border-left: 4px solid #f39c12;
             padding: 12px; margin: 8px 0; border-radius: 4px; }}
  .detail-fail {{ background: #fde8e8; border-left: 4px solid #e74c3c; }}
  .badge {{ display: inline-block; padding: 2px 8px; border-radius: 12px;
            font-size: 0.8em; font-weight: bold; color: white; }}
  .badge-pass {{ background: #27ae60; }}
  .badge-fail {{ background: #e74c3c; }}
  .badge-na {{ background: #95a5a6; }}
  pre {{ background: #2c3e50; color: #ecf0f1; padding: 16px;
         border-radius: 8px; overflow-x: auto; font-size: 0.85em; }}
</style>
</head>
<body>
<h1>thttpd-rs Test Report</h1>
<p>Generated: <code>{timestamp}</code></p>
{summary}
{body}
</body>
</html>
"""

CATEGORY_LABELS = {
    "static": "Static File Serving",
    "errors": "Error Responses",
    "headers": "Header Handling",
    "connection": "Connection Handling",
    "edge": "Edge Cases",
    "malformed": "Malformed Input",
    "cgi": "CGI Execution",
    "throttling": "Throttling",
}


def categorize(test_name):
    """Extract category from test_name (e.g. 'static.get_index' -> 'static')."""
    return test_name.split(".")[0] if "." in test_name else "other"


def esc_html(text):
    """Escape HTML special characters."""
    return (str(text)
            .replace("&", "&amp;")
            .replace("<", "&lt;")
            .replace(">", "&gt;")
            .replace('"', "&quot;"))


def generate_baseline_report(baseline):
    """Generate HTML sections for baseline-only report."""
    categories = {}
    for entry in baseline:
        cat = categorize(entry["test_name"])
        categories.setdefault(cat, []).append(entry)

    sections = []
    for cat in sorted(categories.keys()):
        label = CATEGORY_LABELS.get(cat, cat.title())
        entries = categories[cat]

        rows = []
        for entry in entries:
            name = esc_html(entry["test_name"])
            resp = entry["response"]
            req = entry.get("request", {})
            method = esc_html(req.get("method", "?"))
            path = esc_html(req.get("path", "?"))
            code = resp["status_code"]
            code_class = "pass" if 200 <= code < 300 else ("fail" if code >= 400 else "neutral")
            body_len = resp["body_length"]

            rows.append(f"""<tr>
  <td><code>{name}</code></td>
  <td><code>{method} {path}</code></td>
  <td class="{code_class}">{code}</td>
  <td>{esc_html(resp['status_text'])}</td>
  <td>{esc_html(resp['connection_result'])}</td>
  <td>{body_len}</td>
  <td><code>{resp['body_sha256'][:16]}...</code></td>
</tr>""")

        table = f"""<table>
<tr><th>Test</th><th>Request</th><th>Status</th><th>Status Text</th>
    <th>Connection</th><th>Body Len</th><th>SHA-256</th></tr>
{"".join(rows)}
</table>"""

        sections.append(f"<h2>{esc_html(label)} ({len(entries)} tests)</h2>{table}")

    total = len(baseline)
    summary = f"""<div class="summary">
  <div class="summary-box"><div class="num neutral">{total}</div><div class="label">Total Tests</div></div>
</div>"""

    return summary, "\n".join(sections)


def generate_diff_report(baseline, diff_results):
    """Generate HTML sections for differential report with pass/fail."""
    # Build lookup of failures by test_name
    failure_map = {}
    for err in diff_results:
        failure_map[err["test_name"]] = err

    categories = {}
    for entry in baseline:
        cat = categorize(entry["test_name"])
        categories.setdefault(cat, []).append(entry)

    total_pass = 0
    total_fail = 0

    sections = []
    for cat in sorted(categories.keys()):
        label = CATEGORY_LABELS.get(cat, cat.title())
        entries = categories[cat]

        cat_pass = sum(1 for e in entries if e["test_name"] not in failure_map)
        cat_fail = sum(1 for e in entries if e["test_name"] in failure_map)
        total_pass += cat_pass
        total_fail += cat_fail

        rows = []
        for entry in entries:
            name = entry["test_name"]
            resp = entry["response"]
            req = entry.get("request", {})
            method = esc_html(req.get("method", "?"))
            path = esc_html(req.get("path", "?"))

            if name in failure_map:
                badge = '<span class="badge badge-fail">FAIL</span>'
                err = failure_map[name]
                details = []
                for d in err["failures"]:
                    details.append(
                        f"<b>{esc_html(d['field'])}</b>: "
                        f"expected=<code>{esc_html(d['expected'])}</code> "
                        f"actual=<code>{esc_html(d['actual'])}</code>"
                    )
                detail_html = f'<div class="detail detail-fail">{"<br>".join(details)}</div>'
            else:
                badge = '<span class="badge badge-pass">PASS</span>'
                detail_html = ""

            rows.append(f"""<tr>
  <td>{badge}</td>
  <td><code>{esc_html(name)}</code></td>
  <td><code>{method} {path}</code></td>
  <td>{resp['status_code']}</td>
  <td>{resp['body_length']}</td>
</tr>
{f'<tr><td colspan="5">{detail_html}</td></tr>' if detail_html else ''}""")

        table = f"""<table>
<tr><th>Result</th><th>Test</th><th>Request</th><th>Expected Status</th><th>Body Len</th></tr>
{"".join(rows)}
</table>"""

        cat_label = f"{esc_html(label)} — {cat_pass}/{len(entries)} passed"
        sections.append(f"<h2>{cat_label}</h2>{table}")

    summary = f"""<div class="summary">
  <div class="summary-box"><div class="num pass">{total_pass}</div><div class="label">Passed</div></div>
  <div class="summary-box"><div class="num fail">{total_fail}</div><div class="label">Failed</div></div>
  <div class="summary-box"><div class="num neutral">{total_pass + total_fail}</div><div class="label">Total</div></div>
</div>"""

    return summary, "\n".join(sections)


def main():
    parser = argparse.ArgumentParser(description="HTML report generator")
    parser.add_argument("--baseline", required=True,
                        help="Path to baseline.json")
    parser.add_argument("--diff-results",
                        help="Path to differential results JSON (optional)")
    parser.add_argument("--output", required=True,
                        help="Output HTML file path")
    args = parser.parse_args()

    if not os.path.isfile(args.baseline):
        print(f"ERROR: Baseline not found at {args.baseline}", file=sys.stderr)
        sys.exit(1)

    with open(args.baseline) as f:
        baseline = json.load(f)

    diff_results = None
    if args.diff_results:
        # An explicitly-requested diff file that is missing is an error, not a
        # silent fallback to a baseline-only report — otherwise a typo'd path
        # (or a failed differential run producing no output) hides the problem.
        if not os.path.isfile(args.diff_results):
            print(
                f"ERROR: Diff results not found at {args.diff_results}",
                file=sys.stderr,
            )
            sys.exit(1)
        with open(args.diff_results) as f:
            diff_results = json.load(f)

    from datetime import datetime
    timestamp = datetime.now().strftime("%Y-%m-%d %H:%M:%S")

    if diff_results:
        summary, body = generate_diff_report(baseline, diff_results)
    else:
        summary, body = generate_baseline_report(baseline)

    html = HTML_TEMPLATE.format(
        timestamp=timestamp,
        summary=summary,
        body=body,
    )

    os.makedirs(os.path.dirname(os.path.abspath(args.output)), exist_ok=True)
    with open(args.output, "w") as f:
        f.write(html)

    print(f"Report written to {args.output}")


if __name__ == "__main__":
    main()
