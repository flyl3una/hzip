#!/usr/bin/env python3
"""代码组织两条红线静态检查（T-STD，用户口径 2026-10-08）。

规则（对 `crates/*/src/**` 里的**业务文件**生效；`tests/` 树内不套用）：
  L1  `mod.rs` **只做模块导入导出**：不得出现定义（impl / fn / struct / enum / trait /
      const / static / type）。`mod` / `pub use` / `use` / 属性 / 注释 / 文档不算违规。
  L2  **禁止内联测试模块**：不允许 `#[cfg(test)] mod <name> { ... }`（带函数体）。
      允许 `#[cfg(test)] mod <name>;`（声明 + 独立文件）。
  L3  **测试代码必须住在 `tests` 名下**：`#[cfg(test)] mod <name>;` 解析到的文件路径
      须含 `tests/` 路径段，或文件名恰为 `tests.rs`
      （形态：`<dir>/tests/<file>.rs` / `<dir>/tests.rs` / `<dir>/tests/mod.rs` /
      crate 根 `src/tests/mod.rs`）。

告警级（不失败）：业务文件里挂在**非 mod 项**上的 `#[cfg(test)]`（如
`#[cfg(test)] fn helper()`）——也该随测试外移，但需可见性决策，先提示。

依赖：Python 3.11+（仅标准库）。有 violations → exit 1，否则 exit 0。
"""

from __future__ import annotations

import re
import sys
from pathlib import Path, PurePosixPath

DEF_KINDS = ("impl", "fn", "struct", "enum", "trait", "const", "static", "type")
DEF_RE = re.compile(
    r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?(?:default\s+)?(?:async\s+)?(?:unsafe\s+)?"
    r"(?:extern\s+\"[^\"]*\"\s+)?(?:" + "|".join(DEF_KINDS) + r")\b"
)
ATTR_RE = re.compile(r"^\s*#\[")
MOD_DECL_RE = re.compile(r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;\s*$")
MOD_BODY_RE = re.compile(r"^\s*(?:pub(?:\s*\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{")
PATH_ATTR_RE = re.compile(r'#\[path\s*=\s*"([^"]+)"\s*\]')


def repo_root() -> Path:
    for d in Path(__file__).resolve().parents:
        manifest = d / "Cargo.toml"
        if manifest.is_file() and "[workspace]" in manifest.read_text(encoding="utf-8"):
            return d
    raise SystemExit("找不到 workspace 根（向上找带 [workspace] 的 Cargo.toml）")


def is_test_path(rel: Path) -> bool:
    """路径是否属于测试树（`tests/` 目录段，或文件名为 `tests.rs`）。"""
    return "tests" in rel.parts[:-1] or rel.name == "tests.rs"


def strip_code(line: str) -> str:
    """粗略去掉注释/字符串（审计用途足够；含 raw string `r#"…"#` 与字节串）。"""
    line = re.sub(r'//.*$', "", line)
    # raw string：r"..." / r#"..."# / r##"..."##
    line = re.sub(r'r#*"', '""', line)
    line = re.sub(r'"#*', '""', line)
    # 普通/字节字符串与字符字面量
    line = re.sub(r'b?"(\\.|[^"\\])*"', '""', line)
    line = re.sub(r"b?'(\\.|[^'\\])'", "''", line)
    return line


def candidates(rel: Path, name: str, explicit: str | None) -> list[Path]:
    """`mod <name>;` 可能的落点（按 Rust 解析顺序给出候补，用于判定/报错）。"""
    if explicit is not None:
        return [(rel.parent / explicit)]
    if rel.name in ("mod.rs", "lib.rs", "main.rs"):
        d = rel.parent
    else:
        # file+dir 形态：foo.rs 的子模块在 foo/ 下（Rust 2018+）
        d = rel.parent / rel.stem
    return [d / f"{name}.rs", d / name / "mod.rs"]


def scan(rel: Path, lines: list[str], violations: list[str], warnings: list[str]) -> None:
    test_tree = is_test_path(rel)

    # L1 —— mod.rs 只做声明
    if rel.name == "mod.rs" and not test_tree:
        for i, line in enumerate(lines, 1):
            if DEF_RE.match(line) and not line.lstrip().startswith(("//", "///", "//!")):
                violations.append(f"L1 {rel.as_posix()}:{i} mod.rs 内出现定义：{line.strip()[:80]}")

    if test_tree:
        return

    # L2/L3 —— 测试模块声明形态
    pending: list[str] = []
    cfg_test = False
    i = 0
    while i < len(lines):
        line = lines[i]
        stripped = line.strip()
        if not stripped or stripped.startswith(("//", "///", "//!")):
            i += 1
            continue
        if ATTR_RE.match(line):
            pending.append(stripped)
            if stripped.replace(" ", "").startswith("#[cfg(test)]"):
                cfg_test = True
            i += 1
            continue
        if cfg_test:
            m_body = MOD_BODY_RE.match(line)
            if m_body:
                start = i + 1
                depth, j = 0, i
                while j < len(lines):
                    depth += strip_code(lines[j]).count("{") - strip_code(lines[j]).count("}")
                    if depth <= 0 and j >= i:
                        break
                    j += 1
                violations.append(
                    f"L2 {rel.as_posix()}:{start} 内联测试模块 `mod {m_body.group(1)} {{`"
                    f"（{start}-{j + 1} 行）：须外移到 tests 下的独立文件（T-STD 规则②）"
                )
                i = j + 1
                pending, cfg_test = [], False
                continue
            m_decl = MOD_DECL_RE.match(line)
            if m_decl:
                explicit = None
                for a in pending:
                    pm = PATH_ATTR_RE.search(a)
                    if pm:
                        explicit = pm.group(1)
                cands = candidates(rel, m_decl.group(1), explicit)
                if not any(is_test_path(c) for c in cands):
                    shown = (rel.parent / cands[0]).as_posix() if explicit else cands[0].as_posix()
                    violations.append(
                        f"L3 {rel.as_posix()}:{i + 1} `mod {m_decl.group(1)};` 落在 {shown}"
                        f"（不在 tests 名下）：测试文件须放同目录 `tests/` 子目录"
                        f"（`tests/<file>.rs`）、`tests.rs` 或 crate 根 `src/tests/`"
                    )
            else:
                # `#[cfg(test)] use …`（为测试模块保留命名空间）无害，不计告警；
                # 只报「业务文件里内联了测试专属的定义」。
                if not re.match(r"\s*(?:pub(?:\s*\([^)]*\))?\s+)?use\b", line):
                    warnings.append(
                        f"W1 {rel.as_posix()}:{i + 1} 业务文件内联 `#[cfg(test)]` 挂在非测试模块项上："
                        f"{stripped[:70]}"
                    )
        pending, cfg_test = [], False
        i += 1


def main() -> int:
    root = repo_root()
    violations: list[str] = []
    warnings: list[str] = []
    declared_targets: set[str] = set()
    test_files: set[str] = set()

    files = [p for p in sorted((root / "crates").glob("*/src/**/*.rs")) if "target" not in p.parts]
    # 先建索引：所有 mod 声明解析出的目标文件（供 L4 判定「测试文件是否被声明」）
    for path in files:
        rel = path.relative_to(root)
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except UnicodeDecodeError:
            continue
        if is_test_path(rel):
            test_files.add(rel.as_posix())
        pending_path_attr: str | None = None
        for line in lines:
            stripped = line.strip()
            if ATTR_RE.match(line):
                m = PATH_ATTR_RE.search(stripped)
                if m:
                    pending_path_attr = m.group(1)
                continue
            m_decl = MOD_DECL_RE.match(line)
            if m_decl:
                for c in candidates(rel, m_decl.group(1), pending_path_attr):
                    declared_targets.add(PurePosixPath(c.as_posix()).as_posix())
            pending_path_attr = None

    for path in files:
        rel = path.relative_to(root)
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except UnicodeDecodeError:
            continue
        scan(rel, lines, violations, warnings)

    # L4 —— 测试文件必须被某个 mod 声明引用（否则测试**静默不编译**）
    for tf in sorted(test_files):
        if PurePosixPath(tf).as_posix() not in declared_targets:
            violations.append(
                f"L4 {tf} 无任何 `mod` 声明指向它：测试文件不会被编译"
                f"（业务文件里补 `#[cfg(test)] #[path = \"…\"] mod tests;`）"
            )

    print("-" * 68)
    print(f"VIOLATIONS（红线，共 {len(violations)} 条）")
    print("-" * 68)
    for v in violations:
        print(v)
    print("\n" + "-" * 68)
    print(f"WARNINGS（告警，不失败，共 {len(warnings)} 条）")
    print("-" * 68)
    for w in warnings:
        print(w)
    print(f"\n结果：violations={len(violations)} warnings={len(warnings)}")
    return 1 if violations else 0


if __name__ == "__main__":
    sys.exit(main())
