#!/usr/bin/env python3
"""依赖方向红线静态检查（02 §2.1 五条红线，T00 交付物）。

对各 workspace 成员 `Cargo.toml` 的依赖元数据做静态分析：
  R1  hzip-fmt-* 之间零依赖；
  R2  hzip-cli / hzip-gui 禁止依赖任何 hzip-fmt-* 与 hzip-codec；
  R3  hzip-format 禁止依赖任何 hzip-fmt-*（依赖倒置）；
  R4  hzip-error / hzip-progress / hzip-platform 不依赖 workspace 内其他业务 crate
      （hzip-error 为共享错误模型、允许被叶子依赖：02 §2.1 图 PLT/PRG/CODEC --> ERR）；
  R5  平台 crate（PLATFORM_ONLY）只允许出现在 hzip-platform 依赖里（02 §5 收口）。

红线按（运行时/编译期）dependencies 判定（含 target.'cfg(...)'.dependencies）；
dev-dependencies 违规仅告警不失败（内部补充决策：避免 testkit/dev 工具误伤）。
hzip-format-testkit 加入后（T21）不受 R1~R4 限制（02 §2.1 中 TK → FMTA 为合法边）。

依赖：Python 3.11+（标准库 tomllib）。有 violations → exit 1，否则 exit 0。
"""

from __future__ import annotations

import sys
import tomllib
from pathlib import Path

def _repo_root() -> Path:
    """向上找到带 [workspace] 的 Cargo.toml —— 脚本自己挪目录也不会找错根。"""
    for d in Path(__file__).resolve().parents:
        manifest = d / "Cargo.toml"
        if manifest.is_file() and "[workspace]" in manifest.read_text(encoding="utf-8"):
            return d
    raise SystemExit("找不到仓库根（带 [workspace] 的 Cargo.toml）")


ROOT = _repo_root()

PLATFORM_ONLY = {
    "windows-sys",
    "windows",
    "core-foundation",
    "core-foundation-sys",
    "objc",
    "objc2",
    "libc",
}  # 红线 5 清单（可随 02 §5 扩充）


def load(member: str) -> tuple[set[str], set[str]]:
    """解析一个成员 crate 的依赖元数据 →（运行时含 target 依赖, dev 依赖）。"""
    text = (ROOT / member / "Cargo.toml").read_text(encoding="utf-8")
    t = tomllib.loads(text)
    deps: set[str] = set(t.get("dependencies", {}))
    for cfg in t.get("target", {}).values():  # target.'cfg(windows)'.dependencies 等
        deps |= set(cfg.get("dependencies", {}))
    dev = set(t.get("dev-dependencies", {}))
    return deps, dev


def main() -> int:
    # Windows 控制台代码页可能非 UTF-8，尽力保证中文输出不乱码。
    if hasattr(sys.stdout, "reconfigure"):
        try:
            sys.stdout.reconfigure(encoding="utf-8", errors="replace")
            sys.stderr.reconfigure(encoding="utf-8", errors="replace")
        except (ValueError, OSError):
            pass

    root_toml = tomllib.loads((ROOT / "Cargo.toml").read_text(encoding="utf-8"))
    members: list[str] = root_toml["workspace"]["members"]
    names = {Path(m).name for m in members}
    hzip_set = {n for n in names if n.startswith("hzip-")}
    hzip_fmt_set = {n for n in names if n.startswith("hzip-fmt-")}

    graph = {m: load(m) for m in members}

    violations: list[tuple[str, str, str]] = []  # (红线, crate, 违规依赖)
    warnings: list[tuple[str, str, str]] = []
    for m, (deps, dev) in graph.items():
        name = Path(m).name
        if name in hzip_fmt_set:  # 红线 1
            for d in sorted(deps & hzip_fmt_set - {name}):
                violations.append(("R1", name, d))
        if name in ("hzip-cli", "hzip-gui"):  # 红线 2
            for d in sorted(deps & (hzip_fmt_set | {"hzip-codec"})):
                violations.append(("R2", name, d))
        if name == "hzip-format":  # 红线 3
            for d in sorted(deps & hzip_fmt_set):
                violations.append(("R3", name, d))
        if name in ("hzip-error", "hzip-progress", "hzip-platform"):  # 红线 4
            # 叶子 crate 允许依赖 hzip-error（02 §2.1 依赖图合法边 PLT/PRG/CODEC --> ERR）；
            # R4 禁止的是依赖其余 workspace 业务 crate（progress/platform/format/core/fmt-*）。
            leaf_banned = hzip_set - {name, "hzip-error"}
            for d in sorted(deps & leaf_banned):
                violations.append(("R4", name, d))
        for d in sorted(deps & PLATFORM_ONLY):  # 红线 5
            if m != "crates/hzip-platform":
                violations.append(("R5", name, d))
        for d in sorted(dev & PLATFORM_ONLY):  # dev 依赖仅告警（内部补充决策）
            if m != "crates/hzip-platform":
                warnings.append(("R5-dev", name, d))

    def dump(title: str, rows: list[tuple[str, str, str]]) -> None:
        print(f"\n{title}（红线 | crate | 违规依赖）")
        print("-" * 60)
        if rows:
            for rule, crate, dep in rows:
                print(f"{rule:<8}| {crate:<24}| {dep}")
        else:
            print("（无）")

    print(f"check-deps-direction：扫描 {len(members)} 个 workspace 成员，"
          f"红线 R1~R5（02 §2.1）")
    dump("VIOLATIONS", violations)
    dump("WARNINGS", warnings)
    print(f"\n结果：violations={len(violations)} warnings={len(warnings)}")
    if violations:
        print("依赖方向红线违规，CI 失败（exit 1）")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
