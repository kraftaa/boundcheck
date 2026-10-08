#!/usr/bin/env python3
"""Compare a boundarycheck JSON report with a recorded compatibility baseline.

    check_expected.py REPORT EXPECTED

The baseline lists the verdict (and, for FAIL, the classifications) observed
for a pinned runtime version, optionally with a "stream" override for
streamed (SSE) runs. Scenarios not listed are expected to have
`default_verdict`. Exits 1 if any verdict or classification differs, in either
direction, so a fix or a regression in the runtime is noticed.
"""
import json
import sys


def main(report_path, expected_path):
    report = json.load(open(report_path))
    expected = json.load(open(expected_path))
    default = expected.get("default_verdict", "PASS")
    problems = []
    if report.get("harness_error"):
        problems.append(f"harness error: {report['harness_error']}")
    # A scenario entry may carry a "stream" override for runs whose provider
    # requests were streamed (SSE).
    streamed = any(q.get("stream") for sc in report["scenarios"] for q in sc.get("provider_requests", []))
    seen = set()
    for s in report["scenarios"]:
        seen.add(s["id"])
        exp = expected["scenarios"].get(s["id"], {"verdict": default})
        if streamed and "stream" in exp:
            exp = exp["stream"]
        if s["verdict"] != exp["verdict"]:
            problems.append(f"{s['id']}: expected {exp['verdict']}, got {s['verdict']}")
        elif "classifications" in exp and sorted(s["classifications"]) != sorted(exp["classifications"]):
            problems.append(f"{s['id']}: expected {exp['classifications']}, got {s['classifications']}")
    for sid in expected["scenarios"]:
        if sid not in seen:
            problems.append(f"{sid}: listed in the baseline but not run")
    if expected.get("protocol") and report.get("provider_protocol") != expected["protocol"]:
        problems.append(f"provider protocol {report.get('provider_protocol')} != baseline {expected['protocol']}")
    runtime = report.get("runtime", {})
    if runtime.get("version") != expected.get("version"):
        problems.append(f"runtime version {runtime.get('version')} != baseline {expected.get('version')}")
    for p in problems:
        print(f"MISMATCH {p}")
    print(f"{len(report['scenarios'])} scenarios checked against {expected_path}: "
          f"{'OK' if not problems else str(len(problems)) + ' mismatch(es)'}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:3]))
