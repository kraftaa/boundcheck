#!/usr/bin/env python3
"""Compare a boundarycheck JSON report with a recorded compatibility baseline.

    check_expected.py REPORT EXPECTED --mode {stream,non-stream}

The baseline lists the verdict (and, for FAIL, the classifications) observed
for a pinned runtime version, optionally with a "stream" override for
streamed (SSE) runs. Scenarios not listed are expected to have
`default_verdict`. Exits 1 if any verdict or classification differs, in either
direction, so a fix or a regression in the runtime is noticed.
"""
import argparse
import json
import sys


def main(report_path, expected_path, mode):
    with open(report_path, encoding="utf-8") as handle:
        report = json.load(handle)
    with open(expected_path, encoding="utf-8") as handle:
        expected = json.load(handle)
    default = expected.get("default_verdict", "PASS")
    problems = []
    if report.get("harness_error"):
        problems.append(f"harness error: {report['harness_error']}")
    streamed = mode == "stream"
    observed_streaming = any(
        q.get("stream") is True
        for sc in report["scenarios"]
        for q in sc.get("provider_requests", [])
    )
    if streamed != observed_streaming:
        problems.append(f"requested mode {mode}, report streaming={observed_streaming}")
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
        if exp["verdict"] == "UNKNOWN":
            actual_reasons = sorted({note["reason"] for note in s.get("unknowns", [])})
            expected_reasons = sorted(exp.get("unknown_reasons", []))
            if not expected_reasons:
                problems.append(f"{s['id']}: UNKNOWN baseline must list unknown_reasons")
            elif actual_reasons != expected_reasons:
                problems.append(f"{s['id']}: expected UNKNOWN reasons {expected_reasons}, got {actual_reasons}")
    for sid in expected["scenarios"]:
        if sid not in seen:
            problems.append(f"{sid}: listed in the baseline but not run")
    if expected.get("protocol") and report.get("provider_protocol") != expected["protocol"]:
        problems.append(f"provider protocol {report.get('provider_protocol')} != baseline {expected['protocol']}")
    runtime = report.get("runtime", {})
    if runtime.get("name") != expected.get("runtime"):
        problems.append(f"runtime name {runtime.get('name')} != baseline {expected.get('runtime')}")
    if runtime.get("version") != expected.get("version"):
        problems.append(f"runtime version {runtime.get('version')} != baseline {expected.get('version')}")
    for p in problems:
        print(f"MISMATCH {p}")
    print(f"{len(report['scenarios'])} scenarios checked against {expected_path}: "
          f"{'OK' if not problems else str(len(problems)) + ' mismatch(es)'}")
    return 1 if problems else 0


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("report")
    parser.add_argument("expected")
    parser.add_argument("--mode", required=True, choices=("stream", "non-stream"))
    args = parser.parse_args()
    sys.exit(main(args.report, args.expected, args.mode))
