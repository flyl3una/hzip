#!/usr/bin/env python3
"""本地复跑 CI「quality」作业的全部门禁——**推之前跑一条命令**。

与 `.github/workflows/ci.yml` 的 quality 步骤一一对应（顺序也一致）：

  1. cargo fmt --all -- --check                              ← 格式
  2. cargo clippy --workspace --all-targets -- -D warnings   ← lint（-D warnings 才是门禁）
  3. cargo deny check licenses advisories                    ← 许可 + 安全公告
  4. build/checks/deps-direction.py                          ← 依赖方向红线
  5. build/checks/layout-conventions.py                      ← 目录约定红线（L1~L4）

# 为什么值得单独有个脚本

这几条都不是「编译不过」级别的问题：格式、lint、代码组织红线、依赖方向。本地
`cargo test --workspace` 全绿也照样推上去红——2026-10-14 一天里连踩三次（fmt 6 个
文件、clippy `dead_code` 1 处、layout **L1** 2 处），每次都要等一轮 CI 才看见。
本脚本把它们一次跑全，且**不在第一个失败处停**（本地要一口气看全；CI 才按 step
逐个红，因为那里需要精确定位）。

quality 作业里剩下的两步（工具链安装、MSRV == rust-version 断言）是**环境**检查，
不随代码改动而变，故不在此复跑。

# 用法

    python3 build/checks/quality-gate.py              # 全跑
    python3 build/checks/quality-gate.py fmt clippy   # 只跑指定项
    python3 build/checks/quality-gate.py --list       # 看可选项名

# 可选：挂成 pre-push 钩子（每个 clone 做一次，之后 push 前自动跑）

    printf '#!/bin/sh\nexec python3 "$(git rev-parse --show-toplevel)/build/checks/quality-gate.py"\n' \\
      > .git/hooks/pre-push && chmod +x .git/hooks/pre-push

Windows（git-bash / PowerShell 下 git 自带 sh）同样可用；钩子非零退出即中止 push。

# 退出码

全部通过（含被跳过的项）→ 0；任一失败 → 1。
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

# 门禁项：名字 → 命令行。`checks` 是两条独立脚本，任一非零即该项失败
# （对应 CI 里合并成一个 step 的「build/checks（依赖方向 + 目录约定）」）。
GATES: dict[str, list[list[str]]] = {
    "fmt": [["cargo", "fmt", "--all", "--", "--check"]],
    "clippy": [["cargo", "clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]],
    "deny": [["cargo", "deny", "check", "licenses", "advisories"]],
    "checks": [
        [sys.executable, "build/checks/deps-direction.py"],
        [sys.executable, "build/checks/layout-conventions.py"],
    ],
}
ORDER = ["fmt", "clippy", "deny", "checks"]
HINTS = {
    "fmt": "本地一键修：`cargo fmt --all`（CI 上这一步就是 cargo fmt --check）",
    "clippy": "本地看全量：`cargo clippy --workspace --all-targets`（不带 -D warnings 只看告警）",
    "deny": "装工具：`cargo install cargo-deny --locked`（CI 钉 0.20.2）",
    "checks": "红线细则见脚本头部 docstring（L1 mod.rs 只做声明 / L2 禁内联测试模块 / L3 测试住 tests 下 / L4 测试文件须被 mod 引用）",
}


def repo_root() -> Path:
    """向上找带 `[workspace]` 的 Cargo.toml（与兄弟检查脚本同一套定位方式）。"""
    for d in Path(__file__).resolve().parents:
        manifest = d / "Cargo.toml"
        if manifest.is_file() and "[workspace]" in manifest.read_text(encoding="utf-8"):
            return d
    raise SystemExit("找不到 workspace 根（向上找带 [workspace] 的 Cargo.toml）")


def run(commands: list[list[str]], cwd: Path) -> tuple[str, int]:
    """跑一个门禁项；返回 (状态, 退出码)。输出直通终端（不捕获，方便看细节）。"""
    for cmd in commands:
        print(f"\n$ {' '.join(cmd)}", flush=True)
        try:
            rc = subprocess.run(cmd, cwd=cwd).returncode
        except FileNotFoundError:
            print(f"  ✗ 找不到可执行文件：{cmd[0]}", flush=True)
            return "fail", 127
        if rc != 0:
            return "fail", rc
    return "pass", 0


def main(argv: list[str]) -> int:
    if "--list" in argv:
        print("可选项名：" + " ".join(ORDER))
        return 0
    wanted = [a for a in argv if not a.startswith("-")] or list(ORDER)
    unknown = [w for w in wanted if w not in GATES]
    if unknown:
        print(f"未知项：{' '.join(unknown)}（可选：{' '.join(ORDER)}）")
        return 2

    root = repo_root()
    print("CI quality 作业门禁本地复跑（与 .github/workflows/ci.yml 一一对应）")
    print(f"仓库根：{root}")
    print(f"本次项：{' '.join(wanted)}")

    results: dict[str, str] = {}
    for name in wanted:
        if name == "deny" and shutil.which("cargo-deny") is None:
            print("\n== cargo deny：跳过（未装 cargo-deny；CI 由 taiki-e/install-action 装 0.20.2）")
            results[name] = "skip"
            continue
        print(f"\n{'=' * 68}\n== {name}\n{'=' * 68}")
        status, _ = run(GATES[name], root)
        results[name] = status

    print("\n" + "-" * 68)
    print("汇总")
    print("-" * 68)
    for name, status in results.items():
        mark = {"pass": "✓ 通过", "skip": "— 跳过", "fail": "✗ 失败"}[status]
        print(f"  {mark}  {name}")
        if status == "fail":
            print(f"          {HINTS[name]}")

    failed = [n for n, s in results.items() if s == "fail"]
    if failed:
        print(f"\n结果：{len(failed)} 项失败（{' '.join(failed)}）——修完再 push，否则 CI quality 必红")
        return 1
    skipped = [n for n, s in results.items() if s == "skip"]
    suffix = f"（跳过 {' '.join(skipped)}）" if skipped else ""
    print(f"\n结果：全部通过{suffix}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
