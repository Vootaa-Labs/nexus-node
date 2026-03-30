#!/usr/bin/env python3

from __future__ import annotations

import argparse
import json
from pathlib import Path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Render a compact CI coverage summary from package-scorecard.json.")
    parser.add_argument("--scorecard-json", required=True)
    parser.add_argument("--output", required=True)
    return parser.parse_args()


def render_percent(value: float | None) -> str:
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
    return f"{value:.2f}%"


def main() -> int:
    args = parse_args()
    payload = json.loads(Path(args.scorecard_json).read_text(encoding="utf-8"))

    totals = payload["workspace_totals"]
    packages = payload["packages"]
    hotspots = payload["hotspots"]
    below_target = [package for package in packages if package["status"] == "below-target"]

    lines = [
        f"## Coverage Summary v{payload['workspace_version']}",
        "",
        f"Generated on {payload['generated_on']} from `target/llvm-cov/package-scorecard.json`.",
        "",
        f"- Workspace line coverage: {totals['lines']['percent']:.2f}%",
        f"- Workspace function coverage: {totals['functions']['percent']:.2f}%",
        f"- Workspace region coverage: {totals['regions']['percent']:.2f}%",
        f"- Reported source files: {payload['reported_source_files']}",
        "",
        "### Package Gate Delta",
        "",
    ]

    if below_target:
        lines.extend(
            [
                "| Package | Priority | Target | Lines | Gap |",
                "| --- | --- | ---: | ---: | ---: |",
            ]
        )
        for package in below_target:
            lines.append(
                "| {package} | {priority} | {target} | {lines} | {gap} |".format(
                    package=package["package"],
                    priority=package["priority"],
                    target=render_target(package["target_lines"]),
                    lines=render_percent(package["metrics"]["lines"]["percent"]),
                    gap=render_gap(package["delta_to_target"]),
                )
            )
    else:
        lines.append("All tracked packages meet their current line-coverage targets.")

    lines.extend(["", "### Top Hotspots", "", "| Package | File | Lines | Uncovered Lines |", "| --- | --- | ---: | ---: |"])
    for hotspot in hotspots[:5]:
        lines.append(
            "| {package} | {file} | {lines} | {uncovered} |".format(
                package=hotspot["package"],
                file=hotspot["file"],
                lines=render_percent(hotspot["line_percent"]),
                uncovered=hotspot["uncovered_lines"],
            )
        )

    output_path = Path(args.output)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())