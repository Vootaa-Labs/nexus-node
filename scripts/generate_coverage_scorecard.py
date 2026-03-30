#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from copy import deepcopy
from dataclasses import dataclass
from datetime import date
from pathlib import Path
from typing import Any


METRICS = ("lines", "functions", "regions", "instantiations")
PRIORITY_ORDER = {"P0": 0, "P1": 1, "P2": 2, "Support": 3}
DEFAULT_POLICY = {"priority": "Support", "target_lines": None, "description": "Support tooling or helper package"}
PACKAGE_POLICIES = {
    "nexus-consensus": {
        "priority": "P0",
        "target_lines": 90.0,
        "description": "Protocol safety core",
    },
    "nexus-crypto": {
        "priority": "P0",
        "target_lines": 95.0,
        "description": "Cryptography and KAT coverage",
    },
    "nexus-execution": {
        "priority": "P0",
        "target_lines": 85.0,
        "description": "Execution correctness and determinism",
    },
    "nexus-storage": {
        "priority": "P1",
        "target_lines": 85.0,
        "description": "Atomicity, recovery, and path safety",
    },
    "nexus-intent": {
        "priority": "P1",
        "target_lines": 85.0,
        "description": "Intent state machines and provenance",
    },
    "nexus-network": {
        "priority": "P1",
        "target_lines": 75.0,
        "description": "Transport contract and fail-closed behavior",
    },
    "nexus-rpc": {
        "priority": "P2",
        "target_lines": 80.0,
        "description": "DTO, middleware, and error mapping",
    },
    "nexus-node": {
        "priority": "P2",
        "target_lines": 70.0,
        "description": "Assembly layer with pragmatic threshold",
    },
    "nexus-config": {
        "priority": "P2",
        "target_lines": 80.0,
        "description": "Configuration parsing and validation",
    },
    "nexus-primitives": {
        "priority": "P2",
        "target_lines": 80.0,
        "description": "Shared domain primitives",
    },
}


@dataclass
class PackageRecord:
    name: str
    manifest_dir: Path
    relative_dir: str
    priority: str
    target_lines: float | None
    description: str
    files: int
    summary: dict[str, dict[str, float]]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Generate a crate-level coverage scorecard from cargo-llvm-cov JSON summary output.")
    parser.add_argument("--workspace-root", required=True)
    parser.add_argument("--summary-json", required=True)
    parser.add_argument("--output-scorecard-md", required=True)
    parser.add_argument("--output-scorecard-json", required=True)
    parser.add_argument("--output-report-en")
    parser.add_argument("--output-report-zh")
    return parser.parse_args()


def zero_summary() -> dict[str, dict[str, float]]:
    return {metric: {"count": 0.0, "covered": 0.0} for metric in METRICS}


def metric_percent(metric: dict[str, float]) -> float:
    count = metric["count"]
    if count <= 0:
        return 100.0
    return (metric["covered"] / count) * 100.0


def round_metric(metric: dict[str, float]) -> dict[str, float]:
    count = int(metric.get("count", 0))
    covered = int(metric.get("covered", 0))
    return {
        "covered": covered,
        "count": count,
        "percent": round(metric_percent({"covered": covered, "count": count}), 2),
    }


def is_relative_to(path: Path, other: Path) -> bool:
    try:
        path.relative_to(other)
        return True
    except ValueError:
        return False


def load_workspace_version(workspace_root: Path) -> str:
    cargo_toml = workspace_root / "Cargo.toml"
    lines = cargo_toml.read_text(encoding="utf-8").splitlines()
    in_workspace_package = False
    for line in lines:
        stripped = line.strip()
        if stripped.startswith("["):
            in_workspace_package = stripped == "[workspace.package]"
            continue
        if in_workspace_package and stripped.startswith("version"):
            _, value = stripped.split("=", 1)
            return value.strip().strip('"')
    raise RuntimeError("Unable to determine workspace version from Cargo.toml")


def load_workspace_packages(workspace_root: Path) -> list[PackageRecord]:
    result = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--format-version", "1"],
        cwd=workspace_root,
        check=True,
        capture_output=True,
        text=True,
    )
    metadata = json.loads(result.stdout)
    packages: list[PackageRecord] = []
    for package in metadata.get("packages", []):
        manifest_dir = Path(package["manifest_path"]).resolve().parent
        if not is_relative_to(manifest_dir, workspace_root):
            continue
        policy = deepcopy(DEFAULT_POLICY)
        policy.update(PACKAGE_POLICIES.get(package["name"], {}))
        packages.append(
            PackageRecord(
                name=package["name"],
                manifest_dir=manifest_dir,
                relative_dir=manifest_dir.relative_to(workspace_root).as_posix(),
                priority=policy["priority"],
                target_lines=policy["target_lines"],
                description=policy["description"],
                files=0,
                summary=zero_summary(),
            )
        )
    packages.sort(key=lambda package: len(package.relative_dir), reverse=True)
    return packages


def load_summary(summary_json_path: Path) -> dict[str, Any]:
    payload = json.loads(summary_json_path.read_text(encoding="utf-8"))
    data = payload.get("data", [])
    if not data:
        raise RuntimeError(f"Coverage summary {summary_json_path} did not contain any data entries")
    return data[0]


def select_package(file_path: Path, packages: list[PackageRecord]) -> PackageRecord | None:
    for package in packages:
        if is_relative_to(file_path, package.manifest_dir):
            return package
    return None


def collect_scorecard(workspace_root: Path, summary_data: dict[str, Any], packages: list[PackageRecord]) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    files = summary_data.get("files", [])
    hotspots: list[dict[str, Any]] = []
    reported_source_files = 0
    vendor_src_files = 0

    for file_entry in files:
        filename = file_entry.get("filename")
        if not filename:
            continue
        file_path = Path(filename).resolve()
        if not is_relative_to(file_path, workspace_root):
            continue
        reported_source_files += 1
        relative_file = file_path.relative_to(workspace_root).as_posix()
        if "/vendor-src/" in f"/{relative_file}/":
            vendor_src_files += 1
        package = select_package(file_path, packages)
        if package is None:
            continue

        summary = file_entry.get("summary", {})
        package.files += 1
        for metric in METRICS:
            metric_summary = summary.get(metric, {})
            package.summary[metric]["count"] += float(metric_summary.get("count", 0))
            package.summary[metric]["covered"] += float(metric_summary.get("covered", 0))

        line_metric = summary.get("lines", {})
        line_count = int(line_metric.get("count", 0))
        line_covered = int(line_metric.get("covered", 0))
        line_percent = metric_percent({"covered": line_covered, "count": line_count})
        uncovered_lines = max(line_count - line_covered, 0)
        target_lines = package.target_lines
        gap_to_target = None if target_lines is None else round(target_lines - line_percent, 2)
        hotspots.append(
            {
                "package": package.name,
                "priority": package.priority,
                "file": relative_file,
                "line_percent": round(line_percent, 2),
                "uncovered_lines": uncovered_lines,
                "target_lines": target_lines,
                "gap_to_target": gap_to_target,
            }
        )

    package_rows: list[dict[str, Any]] = []
    for package in sorted(packages, key=lambda item: (PRIORITY_ORDER[item.priority], item.name)):
        metrics = {metric: round_metric(package.summary[metric]) for metric in METRICS}
        line_percent = metrics["lines"]["percent"]
        target_lines = package.target_lines
        gap_to_target = None if target_lines is None else round(target_lines - line_percent, 2)
        if target_lines is None:
            status = "tracked"
        elif gap_to_target <= 0:
            status = "meets-target"
        else:
            status = "below-target"
        package_rows.append(
            {
                "package": package.name,
                "path": package.relative_dir,
                "priority": package.priority,
                "target_lines": target_lines,
                "delta_to_target": None if gap_to_target is None else round(max(gap_to_target, 0.0), 2),
                "status": status,
                "description": package.description,
                "source_files": package.files,
                "metrics": metrics,
            }
        )

    hotspot_rows = sorted(
        hotspots,
        key=lambda item: (
            PRIORITY_ORDER.get(item["priority"], 99),
            0 if item["gap_to_target"] is None else -item["gap_to_target"],
            item["line_percent"],
            -item["uncovered_lines"],
            item["file"],
        ),
    )[:10]

    totals = summary_data.get("totals", {})
    workspace_totals = {metric: round_metric(totals.get(metric, {})) for metric in METRICS}
    scorecard = {
        "generated_on": str(date.today()),
        "workspace_version": load_workspace_version(workspace_root),
        "workspace_totals": workspace_totals,
        "reported_source_files": reported_source_files,
        "vendor_src_files": vendor_src_files,
        "package_count": len(package_rows),
        "packages": package_rows,
        "hotspots": hotspot_rows,
    }
    return scorecard, hotspot_rows


def render_percentage(value: float | None) -> str:
    if value is None:
        return "n/a"
    return f"{value:.2f}%"


def render_target(value: float | None) -> str:
    if value is None:
        return "tracked"
    return f">= {value:.0f}%"


def render_gap(value: float | None) -> str:
    if value is None:
        return "n/a"
    if value <= 0:
        return "0.00%"
    return f"{value:.2f}%"


def render_metric_rows(scorecard: dict[str, Any]) -> str:
    totals = scorecard["workspace_totals"]
    rows = []
    labels = {
        "lines": "Lines",
        "functions": "Functions",
        "regions": "Regions",
        "instantiations": "Instantiations",
    }
    for metric in METRICS:
        snapshot = totals[metric]
        rows.append(
            f"| {labels[metric]} | {snapshot['covered']:,} | {snapshot['count']:,} | {snapshot['percent']:.2f}% |"
        )
    return "\n".join(rows)


def render_metric_rows_zh(scorecard: dict[str, Any]) -> str:
    totals = scorecard["workspace_totals"]
    labels = {
        "lines": "行覆盖率",
        "functions": "函数覆盖率",
        "regions": "区域覆盖率",
        "instantiations": "实例化覆盖率",
    }
    rows = []
    for metric in METRICS:
        snapshot = totals[metric]
        rows.append(
            f"| {labels[metric]} | {snapshot['covered']:,} | {snapshot['count']:,} | {snapshot['percent']:.2f}% |"
        )
    return "\n".join(rows)


def render_package_table(packages: list[dict[str, Any]]) -> str:
    rows = []
    for package in packages:
        rows.append(
            "| {package} | {priority} | {target} | {lines} | {gap} | {functions} | {regions} | {files} | {status} |".format(
                package=package["package"],
                priority=package["priority"],
                target=render_target(package["target_lines"]),
                lines=render_percentage(package["metrics"]["lines"]["percent"]),
                gap=render_gap(package["delta_to_target"]),
                functions=render_percentage(package["metrics"]["functions"]["percent"]),
                regions=render_percentage(package["metrics"]["regions"]["percent"]),
                files=package["source_files"],
                status=package["status"],
            )
        )
    return "\n".join(rows)


def render_hotspot_table(hotspots: list[dict[str, Any]]) -> str:
    rows = []
    for hotspot in hotspots:
        rows.append(
            "| {package} | {priority} | {file} | {lines} | {gap} | {uncovered} |".format(
                package=hotspot["package"],
                priority=hotspot["priority"],
                file=hotspot["file"],
                lines=render_percentage(hotspot["line_percent"]),
                gap=render_gap(hotspot["gap_to_target"]),
                uncovered=hotspot["uncovered_lines"],
            )
        )
    return "\n".join(rows)


def render_scorecard_markdown(scorecard: dict[str, Any]) -> str:
    packages = scorecard["packages"]
    hotspots = scorecard["hotspots"]
    return f"""# Package Coverage Scorecard v{scorecard['workspace_version']}

Generated on {scorecard['generated_on']} from `target/llvm-cov/coverage-summary.json`.

## Workspace Totals

| Metric | Covered | Total | Percent |
| --- | ---: | ---: | ---: |
{render_metric_rows(scorecard)}

## Package Scorecard

| Package | Priority | Target | Lines | Gap | Functions | Regions | Files | Status |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
{render_package_table(packages)}

## Top 10 Coverage Hotspots

| Package | Priority | File | Lines | Gap | Uncovered Lines |
| --- | --- | --- | ---: | ---: | ---: |
{render_hotspot_table(hotspots)}
"""


def render_report_en(scorecard: dict[str, Any]) -> str:
    return f"""# Coverage Report v{scorecard['workspace_version']}

## Scope

This report records the current Rust test coverage baseline for the Nexus `v{scorecard['workspace_version']}` workspace.

- Scope includes first-party workspace packages only.
- Vendored sources under `vendor-src/` are excluded from both LCOV and HTML coverage output.
- Generated files under `target/` are excluded from report rendering.
- Coverage was collected from the repository root on {scorecard['generated_on']}.

## Commands

From the repository root:

```bash
make coverage
make coverage-html
make coverage-json
make coverage-scorecard
make coverage-docs
```

`make coverage-docs` executes the coverage sampling pass once, exports LCOV, HTML, and JSON artifacts without rerunning tests, then refreshes this report and the crate-level scorecard.

## Measured Results

| Metric | Covered | Total | Percent |
| --- | ---: | ---: | ---: |
{render_metric_rows(scorecard)}

Additional scope checks for this run:

- Reported source files: {scorecard['reported_source_files']}
- `vendor-src` files present in summary: {scorecard['vendor_src_files']}
- Package scorecard rows: {scorecard['package_count']}

## Package Scorecard

| Package | Priority | Target | Lines | Gap | Functions | Regions | Files | Status |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
{render_package_table(scorecard['packages'])}

## Top 10 Coverage Hotspots

| Package | Priority | File | Lines | Gap | Uncovered Lines |
| --- | --- | --- | ---: | ---: | ---: |
{render_hotspot_table(scorecard['hotspots'])}

## Artifacts

- LCOV output: `lcov.info`
- HTML output: `target/llvm-cov/html/index.html`
- Machine-readable summary used for this report: `target/llvm-cov/coverage-summary.json`
- Crate-level scorecard: `target/llvm-cov/package-scorecard.md`

## Notes

- This is a point-in-time baseline, not a permanent quality gate.
- Core crate targets in the scorecard reflect the v0.1.15 coverage governance plan.
- CI coverage now calls `make coverage-docs`, publishes a package summary in the GitHub Actions step summary, and uploads the refreshed report and scorecard as workflow artifacts.
"""


def render_report_zh(scorecard: dict[str, Any]) -> str:
    return f"""# 覆盖率报告 v{scorecard['workspace_version']}

## 统计范围

本报告记录 Nexus `v{scorecard['workspace_version']}` 工作区当前的 Rust 测试覆盖率基线。

- 统计范围仅包含 first-party 工作区包。
- `vendor-src/` 下的 vendored 源码已从 LCOV 与 HTML 覆盖率输出中排除。
- `target/` 下的生成内容不进入覆盖率展示。
- 本次覆盖率数据于 {scorecard['generated_on']} 在仓库根目录采集。

## 使用命令

在仓库根目录执行：

```bash
make coverage
make coverage-html
make coverage-json
make coverage-scorecard
make coverage-docs
```

`make coverage-docs` 会先执行一次覆盖率测试采样，再在不重跑测试的前提下导出 LCOV、HTML 与 JSON 汇总，并刷新本报告与 crate 级 scorecard。

## 实测结果

| 指标 | 已覆盖 | 总数 | 覆盖率 |
| --- | ---: | ---: | ---: |
{render_metric_rows_zh(scorecard)}

本次运行的额外范围校验：

- 纳入统计的源码文件数：{scorecard['reported_source_files']}
- 汇总结果中 `vendor-src` 文件数：{scorecard['vendor_src_files']}
- crate 级 scorecard 行数：{scorecard['package_count']}

## Package Scorecard

| Package | 优先级 | 目标 | 行覆盖率 | 差值 | 函数覆盖率 | 区域覆盖率 | 文件数 | 状态 |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
{render_package_table(scorecard['packages']).replace('meets-target', '达标').replace('below-target', '未达标').replace('tracked', '跟踪')}

## Top 10 覆盖率热点

| Package | 优先级 | 文件 | 行覆盖率 | 差值 | 未覆盖行数 |
| --- | --- | --- | ---: | ---: | ---: |
{render_hotspot_table(scorecard['hotspots'])}

## 产物位置

- LCOV 输出：`lcov.info`
- HTML 输出：`target/llvm-cov/html/index.html`
- 本报告使用的机器可读汇总：`target/llvm-cov/coverage-summary.json`
- crate 级 scorecard：`target/llvm-cov/package-scorecard.md`

## 说明

- 这是一份当前时点的覆盖率基线，不代表永久冻结的质量门槛。
- scorecard 中的核心 crate 目标对齐 v0.1.15 覆盖率治理计划。
- CI 覆盖率任务现在调用 `make coverage-docs`，会在 GitHub Actions step summary 中输出 package 摘要，并把刷新后的中英文覆盖率报告与 scorecard 作为 workflow artifact 保留。
"""


def write_text(path_str: str | None, content: str) -> None:
    if not path_str:
        return
    path = Path(path_str)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content.rstrip() + "\n", encoding="utf-8")


def main() -> int:
    args = parse_args()
    workspace_root = Path(args.workspace_root).resolve()
    summary_json_path = Path(args.summary_json).resolve()
    packages = load_workspace_packages(workspace_root)
    summary_data = load_summary(summary_json_path)
    scorecard, _ = collect_scorecard(workspace_root, summary_data, packages)

    output_json_path = Path(args.output_scorecard_json)
    output_json_path.parent.mkdir(parents=True, exist_ok=True)
    output_json_path.write_text(json.dumps(scorecard, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    write_text(args.output_scorecard_md, render_scorecard_markdown(scorecard))
    write_text(args.output_report_en, render_report_en(scorecard))
    write_text(args.output_report_zh, render_report_zh(scorecard))

    return 0


if __name__ == "__main__":
    sys.exit(main())