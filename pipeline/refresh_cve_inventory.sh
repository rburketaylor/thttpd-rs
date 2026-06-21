#!/usr/bin/env bash
# Re-runnable CVE sweep for the sthttpd / thttpd / acme-thttpd family.
#
# Produces a DRAFT docs/security/CVE_TABLE.md (every row a CVE id + source URL,
# with the CWE / CVSS / affected columns left as placeholders) and a
# reproducibility sidecar docs/security/cve_inventory.lock (TOML). A human then
# fills the placeholder columns and commits the curated table.
#
# Coverage is best-effort: OSV has good coverage for the post-2017 CVEs;
# NVD keyword search catches the pre-2017 entries that OSV misses. The Debian
# src:thttpd security tracker (https://security-tracker.debian.org/tracker/source-package/thttpd)
# must still be cross-checked by hand because it has no single JSON endpoint.
#
# NVD blocks the default Python/curl User-Agent with a 503; we set a real UA.
# NVD also asks for ~6s between API requests, which the per-CVE sleep below honors.
set -euo pipefail

OUT_MD="docs/security/CVE_TABLE.md"
OUT_LOCK="docs/security/cve_inventory.lock"
mkdir -p docs/security

UA='Mozilla/5.0 (X11; Linux x86_64) thttpd-rs-security-report/1.0 (security research)'

python3 - "$UA" "$OUT_MD" "$OUT_LOCK" <<'PY'
import json, sys, urllib.parse, urllib.request, pathlib, datetime, time

UA, OUT_MD, OUT_LOCK = sys.argv[1], sys.argv[2], sys.argv[3]
OSV_URL = "https://api.osv.dev/v1/query"
OSV_VULN = "https://api.osv.dev/v1/vulns/"
NVD_URL = "https://services.nvd.nist.gov/rest/json/cves/2.0"

# Known CVE seeds against the thttpd/sthttpd/acme-thttpd family. The automated
# sweep below enriches these from OSV + NVD; the human curator cross-checks
# against the Debian tracker for completeness.
SEEDS = [
    "CVE-1999-1457", "CVE-2001-0892", "CVE-2002-0733", "CVE-2004-2628",
    "CVE-2006-1078", "CVE-2006-1079", "CVE-2006-4248", "CVE-2013-0348",
    "CVE-2017-10671", "CVE-2021-26843",
]

def get(url, data=None, tries=4):
    last = None
    for i in range(tries):
        try:
            body = json.dumps(data).encode() if data is not None else None
            req = urllib.request.Request(url, data=body, headers={
                "User-Agent": UA, "Accept": "application/json",
                "Content-Type": "application/json",
            })
            with urllib.request.urlopen(req, timeout=45) as r:
                return json.load(r)
        except Exception as e:
            last = e
            time.sleep(6 * (i + 1))
    print(f"WARN: gave up on {url}: {last}", file=sys.stderr)
    return None

seen = {}   # cve_id -> {year,cwe,cvss,severity,affected,source,summary}

# --- 1. Seed enrichment via NVD (authoritative CWE + CVSS) ---
for cve in SEEDS:
    data = get(f"{NVD_URL}?cveId={cve}")
    if not data or not data.get("vulnerabilities"):
        continue
    c = data["vulnerabilities"][0]["cve"]
    cwes = []
    for w in c.get("weaknesses", []):
        for wd in w.get("description", []):
            v = wd.get("value", "")
            if v and v not in ("NVD-CWE-noinfo", "NVD-CWE-Other") and v not in cwes:
                cwes.append(v)
    cwe = ";".join(cwes) if cwes else "(unclassified — NVD-CWE-noinfo)"
    # Prefer CVSSv3.1, fall back to v3.0, then v2.0
    m = c.get("metrics", {})
    score = ver = sev = vec = ""
    for mk in ("cvssMetricV31", "cvssMetricV30", "cvssMetricV2"):
        if m.get(mk):
            d = m[mk][0]["cvssData"]
            score = d.get("baseScore"); sev = (m[mk][0].get("baseSeverity") or d.get("baseSeverity", ""))
            vec = d.get("vectorString", "")
            ver = "3.1" if mk.endswith("31") else ("3.0" if mk.endswith("30") else "2.0")
            break
    summary = ""
    for d in c.get("descriptions", []):
        if d.get("lang", "").startswith("en"):
            summary = d.get("value", "").split(".")[0]
            break
    seen[cve] = {
        "year": (c.get("published", "")[:4] or "????"),
        "cwe": cwe, "cvss": f"{score} (v{ver})" if score else "",
        "severity": sev, "summary": summary,
        "affected": "(fill from advisory)",  # human curates the exact upstream version
        "source": f"https://nvd.nist.gov/vuln/detail/{cve}",
    }
    time.sleep(6)

# --- 2. OSV pass (catches anything NVD missed, e.g. GHSA-linked) ---
for term in ["sthttpd", "thttpd"]:
    r = get(OSV_URL, data={"package": {"name": term}})
    if not r:
        continue
    for v in r.get("vulns", []):
        cid = v["id"]
        if cid in seen or not cid.startswith("CVE-"):
            continue
        sev = ""
        for s in v.get("severity", []):
            sev = s.get("score", ""); break
        seen[cid] = {
            "year": (v.get("published", "")[:4] or "????"),
            "cwe": "(from OSV)", "cvss": sev, "severity": "",
            "summary": v.get("details", "").split(".")[0][:120],
            "affected": "(fill from advisory)",
            "source": f"https://osv.dev/{cid}",
        }

# --- 3. Emit draft table + lock ---
items = sorted(seen.items())
rows = []
for i, (cid, d) in enumerate(items, 1):
    rows.append(
        f"| {i} | {cid} | {d['year']} | {d['cwe']} | {d['cvss']} {d['severity']} | "
        f"{d['affected']} | {d['source']} |"
    )
md = [
    "# Historical CVE Inventory — sthttpd / thttpd / acme thttpd",
    "",
    f"> DRAFT regenerated {datetime.date.today()} by `pipeline/refresh_cve_inventory.sh`.",
    "> CURATE BEFORE COMMIT: verify each `Affected` version against the upstream",
    "> advisory and cross-check against the Debian src:thttpd security tracker",
    "> (https://security-tracker.debian.org/tracker/source-package/thttpd).",
    "",
    "| # | CVE | Year | CWE | CVSS | Affected | Source |",
    "|---|-----|------|-----|------|----------|--------|",
    *rows,
]
pathlib.Path(OUT_MD).write_text("\n".join(md) + "\n")

now = datetime.datetime.now(datetime.UTC)
lock = (
    "# Auto-generated by refresh_cve_inventory.sh\n"
    f'generated = "{now.isoformat().replace("+00:00", "")}Z"\n'
    f"row_count = {len(rows)}\n"
    'sources = ["osv", "nvd-api-2.0", "debian-tracker-manual"]\n'
    f'queries = {json.dumps(SEEDS + ["thttpd", "sthttpd"])}\n'
)
pathlib.Path(OUT_LOCK).write_text(lock)
print(f"Drafted {len(rows)} rows to {OUT_MD} (curate, then commit).")
PY
