#!/usr/bin/env python3
"""Trusted default-branch reporter; never executes PR code or artifact content."""
import argparse
import json
import os
from pathlib import Path
import subprocess
from urllib.parse import urlencode
import zipfile
from performance_feedback import (MAX_ZIP, SUITES, assemble, read_zip, normalize_suite,
                                  render_comment, sha)

MARKER = "<!-- herdr-benchmark-feedback:v1 -->"
BOT_ID = 41898282

def api(path, method="GET", payload=None, binary=False):
    command = ["gh", "api", "--method", method, path]
    if payload is not None:
        command += ["--input", "-"]
    result = subprocess.run(command, input=json.dumps(payload).encode() if payload is not None else None,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=90, check=True)
    return result.stdout if binary else json.loads(result.stdout)

def paged(path, query=None, limit=20):
    for page in range(1, limit + 1):
        params = dict(query or {}, per_page=100, page=page)
        rows = api(path + "?" + urlencode(params))
        if not isinstance(rows, list):
            raise ValueError("expected paginated list")
        yield from rows
        if len(rows) < 100:
            return
    raise ValueError("pagination limit exceeded")

def eligible_run(run, repo, workflow_id):
    return (run.get("workflow_id") == workflow_id and run.get("path") == ".github/workflows/performance.yml"
            and run.get("repository", {}).get("full_name") == repo
            and run.get("status") == "completed" and run.get("event") in ("pull_request", "push", "workflow_dispatch")
            and sha(run.get("head_sha")))

def match_pr(run, repo):
    if run["event"] == "push" or (run.get("head_repository") or {}).get("full_name") != repo:
        return None
    owner = repo.split("/")[0]
    matches = [p for p in paged(f"repos/{repo}/pulls", {"state": "all", "base": "main", "head": owner + ":" + run["head_branch"]})
               if p.get("head", {}).get("sha") == run["head_sha"]
               and (p.get("head", {}).get("repo") or {}).get("full_name") == repo
               and p.get("base", {}).get("repo", {}).get("full_name") == repo
               and (p.get("state") == "open" or p.get("merged_at"))]
    return matches[0] if len(matches) == 1 else None

def current_pr(pr, run, suites, repo):
    if pr["head"]["sha"] != run["head_sha"] or not (pr["state"] == "open" or pr.get("merged_at")):
        return False
    # An open PR must still target the base actually measured. Merged PRs retain
    # historical evidence even if the base branch has since advanced.
    for s in suites.values():
        if run["event"] == "pull_request":
            if pr["state"] == "open" and s["pr_base_sha"] != pr["base"]["sha"]:
                return False
        elif pr["state"] == "open":
            comparison = api(f"repos/{repo}/compare/{pr['base']['sha']}...{run['head_sha']}")
            if comparison.get("behind_by") != 0:
                return False
    return True

def newest_run(run, repo):
    query = urlencode({"head_sha": run["head_sha"], "per_page": 100})
    latest = api(f"repos/{repo}/actions/workflows/performance.yml/runs?{query}")["workflow_runs"]
    matched = [r for r in latest if r.get("head_sha") == run["head_sha"] and r.get("head_branch") == run["head_branch"]
               and r.get("event") in ("pull_request", "workflow_dispatch")]
    if not matched:
        return False
    newest = max(matched, key=lambda r: (r["run_number"], r["run_attempt"]))
    return newest["id"] == run["id"] and newest["run_attempt"] == run["run_attempt"]

def verify_candidate(run, suite, repo):
    if run["event"] != "pull_request":
        return suite["candidate_sha"] == run["head_sha"]
    commit = api(f"repos/{repo}/commits/{suite['candidate_sha']}")
    parents = [p["sha"] for p in commit.get("parents", [])]
    return parents == [suite["pr_base_sha"], run["head_sha"]]

def find_existing(comments):
    matching = [c for c in comments if c.get("user", {}).get("id") == BOT_ID and
                isinstance(c.get("body"), str) and c["body"].startswith(MARKER)]
    if len(matching) > 1:
        raise ValueError("multiple bot-owned benchmark comments; refusing ambiguous update")
    return matching[0] if matching else None

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--run-id", type=int, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    repo = os.environ["GITHUB_REPOSITORY"]
    reporter_id = int(os.environ["GITHUB_RUN_ID"])
    workflow = api(f"repos/{repo}/actions/workflows/performance.yml")
    run = api(f"repos/{repo}/actions/runs/{args.run_id}")
    if not eligible_run(run, repo, workflow["id"]):
        raise ValueError("not a completed adapter-performance run of this repository")
    # Ignore fork-origin artifacts altogether. They may still have their normal
    # unprivileged benchmark artifacts, but never reach this privileged reporter.
    if (run.get("head_repository") or {}).get("full_name") != repo:
        print("Skipping external-repository run")
        return 0
    pr = match_pr(run, repo)
    attempt = run["run_attempt"]
    artifacts = api(f"repos/{repo}/actions/runs/{run['id']}/artifacts?per_page=100")["artifacts"]
    suites, problems = {}, []
    for name in SUITES:
        expected = f"adapter-performance-{name}-{attempt}"
        matches = [a for a in artifacts if a["name"] == expected and not a["expired"]]
        if len(matches) != 1:
            problems.append(name + ":missing_or_duplicate_artifact")
            continue
        artifact = matches[0]
        try:
            if not 0 < artifact["size_in_bytes"] <= MAX_ZIP:
                raise ValueError("artifact size out of bounds")
            blob = api(f"repos/{repo}/actions/artifacts/{artifact['id']}/zip", binary=True)
            data = read_zip(blob)
            normalized = normalize_suite(data, name, run["id"], attempt, run["event"], run["head_sha"])
            if not verify_candidate(run, normalized, repo):
                raise ValueError("candidate commit parent mismatch")
            normalized["artifact_id"] = artifact["id"]
            suites[name] = normalized
        except (ValueError, TypeError, KeyError, AttributeError, OverflowError, OSError, subprocess.SubprocessError, zipfile.BadZipFile):
            # Never copy artifact-controlled exception text into comments.
            problems.append(name + ":invalid_or_unavailable_evidence")
    if len({s["candidate_sha"] for s in suites.values()}) > 1 or len({s["pr_base_sha"] for s in suites.values()}) > 1:
        suites = {}
        problems.append("cross_suite_revision_mismatch")
    if len({json.dumps(s["workload_sha256"], sort_keys=True) for s in suites.values()}) > 1:
        suites = {}
        problems.append("cross_suite_workload_mismatch")
    report = assemble(run, suites, problems, pr["number"] if pr else None)
    report["source_run_url"] = f"https://github.com/{repo}/actions/runs/{run['id']}"
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "benchmark-feedback.json").write_text(json.dumps(report, indent=2, allow_nan=False) + "\n")
    body = render_comment(report, repo, reporter_id)
    (args.out / "benchmark-feedback.md").write_text(body)
    if os.getenv("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
            summary.write(body)
    if not pr:
        print("No unique eligible PR; artifact only")
        return 0
    if not newest_run(run, repo):
        print("Superseded performance run; artifact only")
        return 0
    fresh = api(f"repos/{repo}/pulls/{pr['number']}")
    if not current_pr(fresh, run, suites, repo):
        print("PR moved; artifact only")
        return 0
    comments = paged(f"repos/{repo}/issues/{pr['number']}/comments")
    existing = find_existing(comments)
    # Recheck immediately before changing the bot-owned comment. Workflow
    # concurrency also serializes automatic reports for this head branch.
    if not newest_run(run, repo) or not current_pr(api(f"repos/{repo}/pulls/{pr['number']}"), run, suites, repo):
        print("PR or run changed; artifact only")
        return 0
    if existing and existing["body"] == body:
        print("Feedback already posted")
        return 0
    if existing:
        api(f"repos/{repo}/issues/comments/{existing['id']}", "PATCH", {"body": body})
    else:
        api(f"repos/{repo}/issues/{pr['number']}/comments", "POST", {"body": body})
    print(f"Published benchmark feedback on PR #{pr['number']}")
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
