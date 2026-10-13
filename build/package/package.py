#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""hzip 多平台安装包驱动（设计依据 04 §5「平台打包与分发」、§6.1「CI/CD」）。

一条命令产出目标平台的安装包，产物一律落到 `--out-dir`（默认 `dist/`）并登记
sha256。CI 与开发机走**同一条**路径，所以本地试跑就是 CI 的预演。

    # Windows（在 Windows 上跑）：MSI 安装包（= setup 向导）+ 免安装 zip
    python build/package/package.py --target x86_64-pc-windows-msvc --formats msi,zip

    # macOS（在 macOS 上跑）：universal .app + dmg
    python build/package/package.py --target x86_64-apple-darwin,aarch64-apple-darwin --formats dmg

    # Linux（在 Linux 上跑）：tar.gz / deb / rpm / AppImage
    python build/package/package.py --target x86_64-unknown-linux-gnu --formats tar.gz,deb,rpm,appimage

    # 只看计划、不落地（含工具缺失检查）
    python build/package/package.py --target ... --dry-run

约定：
* 二进制从 `--bin-dir`（默认 `target/<triple>/release`）取；`--bin NAME=PATH` 可
  单独覆盖某个二进制（自定义构建/本地冒烟）。
* 版本号唯一来源 = 根 `Cargo.toml` 的 `[workspace.package].version`（02 §2.2）；
  发布时用 `--check-tag vX.Y.Z` 断言 tag 与之一致。
* 打包元数据（维护者/描述/依赖/工具版本与校验和）在 `metadata.toml`；模板在
  `templates/`。要加新格式，照 `build_<fmt>` 的样子加一个函数并在 `FORMATS` 注册。
* 许可：唯一正文来源 = 仓库根 `LICENSE`（GPL-3.0 全文），唯一 SPDX 标识 =
  `metadata.toml` 的 `[package] spdx`。`Ctx.licenses`（根 LICENSE*/COPYING*）由各
  `build_<fmt>` 装进产物——GPL 随二进制附全文的义务；缺文件只警告不阻断。
* 工具缺失 = 硬错误（不猜、不静默降级）；只有「可选校验工具」（desktop-file-validate、
  appstreamcli）在缺失时降级为警告，且 `--no-validate` 可显式跳过。
* 签名全部可选：不给凭据就产 unsigned 包并打印提示（04 §9：无证书期间发布
  unsigned 并显著声明）。
"""

from __future__ import annotations

import argparse
import hashlib
import os
import platform
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import uuid
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PKG_DIR = Path(__file__).resolve().parent
TPL_DIR = PKG_DIR / "templates"
META_FILE = PKG_DIR / "metadata.toml"

# 程序图标逐尺寸源（04 §5.5）：Linux hicolor 目录与 macOS .iconset 都从这里取
LOGO_SIZES = (16, 32, 48, 64, 128, 256)
# lipo 的架构名与 rust triple 的架构名不完全同名（rust `aarch64` = lipo `arm64`）
LIPO_ARCH = {"aarch64": "arm64"}
# 组件 GUID 的固定命名空间——**永不修改**（改了等于换了组件身份，升级会留垃圾）
GUID_NS = uuid.UUID("6f2b1d84-3c6a-4f57-9b1e-0d7c5a2e8f31")

# 目标三元组 -> (系统, 架构)。只列发布矩阵用到的（04 §5）
TARGETS = {
    "x86_64-pc-windows-msvc": ("windows", "x86_64"),
    "aarch64-pc-windows-msvc": ("windows", "aarch64"),
    "x86_64-unknown-linux-gnu": ("linux", "x86_64"),
    "aarch64-unknown-linux-gnu": ("linux", "aarch64"),
    "x86_64-apple-darwin": ("macos", "x86_64"),
    "aarch64-apple-darwin": ("macos", "aarch64"),
}


# --------------------------------------------------------------------------- 输出


def _configure_stdio() -> None:
    """把 stdout/stderr 配成「不会因为某个字符编不出来就崩掉」的形态。

    Windows 上 CPython 按**系统 ANSI 代码页**编码 stdout（CI 的 windows runner 是
    cp1252），而本驱动的日志既有 `→` 也有中文——一 print 就是 `UnicodeEncodeError`：
    CI 实测，Windows 腿连第一步 `hzip {ver} → {triple}` 都过不去，`package.py`
    退出码 1（打包根本没开始）。`log()` 只接 `OSError`，而 `UnicodeEncodeError` 是
    `ValueError` 子类，所以旧代码包不住它。

    两种形态分开处理，各自取最优：

    * **输出被重定向/管道**（CI 抓日志、`> log.txt`）：改成 UTF-8——日志在 Actions
      网页、文件、编辑器里都是可读的，非 ASCII 一个不丢；
    * **交互式终端**（本机 cmd/PowerShell）：保留终端自己的编码（cmd 的 cp936/cp437
      认得中文与 `→`），只加 `errors="replace"` 兜底——绝不为了一个字符崩掉整次打包。

    改不动就静默跳过（有些环境把 stdout 换成了不可 reconfigure 的包装对象）。
    """
    for stream in (sys.stdout, sys.stderr):
        try:
            if stream.isatty():
                stream.reconfigure(errors="replace")
            else:
                stream.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, OSError, ValueError):
            pass


_configure_stdio()


def xml_escape(s: str) -> str:
    import xml.sax.saxutils

    return xml.sax.saxutils.escape(s, {'"': "&quot;", "'": "&apos;"})


def _emit(msg: str, *, err: bool = False) -> None:
    """打印一行，双重兜底：**编码**与**管道**都不能把驱动搞崩。

    `_configure_stdio()` 已把编码调好，这里是最后一道保险——万一某个环境把 stdout
    换成了不可 reconfigure 的包装对象，退化成 ASCII（`?` 替掉编不出来的字符）再打一次；
    仍被下游关掉（`| head`）就安静退出。`die()` 用它尤其重要：报错信息本身**不能崩**，
    否则用户看到的是一条 Python traceback 而不是「哪一步错了」。
    """
    stream = sys.stderr if err else sys.stdout
    for text in (msg, msg.encode("ascii", "replace").decode("ascii")):
        try:
            print(text, file=stream, flush=True)
            return
        except UnicodeEncodeError:
            continue
        except OSError:
            sys.exit(0)


def log(msg: str) -> None:
    _emit(msg)


def warn(msg: str) -> None:
    _emit(f"[WARN] {msg}")


def die(msg: str) -> None:
    _emit(f"[ERROR] {msg}", err=True)
    sys.exit(1)


class Ctx:
    """一次打包运行的全部上下文。"""

    def __init__(self, args) -> None:
        self.args = args
        self.meta = read_toml(META_FILE)
        self.version = workspace_version()
        self.work = Path(tempfile.mkdtemp(prefix="hzip-package-"))
        self.out_dir = Path(args.out_dir).resolve()
        self.out_dir.mkdir(parents=True, exist_ok=True)
        self.artifacts: list[Path] = []
        self.logo_dir = ROOT / "crates/hzip-gui/assets/logo"
        self.ico = self.logo_dir / "hzip.ico"
        self.licenses = sorted(
            p for p in ROOT.glob("LICENSE*") if p.is_file()
        ) + sorted(p for p in ROOT.glob("COPYING*") if p.is_file())
        self.cache = Path(
            args.cache_dir
            or os.environ.get("HZIP_PKG_CACHE")
            or (Path.home() / ".cache" / "hzip-package")
        )
        self.verbose = args.verbose
        if args.check_tag:
            tag = args.check_tag.lstrip("v")
            if tag != self.version:
                die(
                    f"--check-tag {args.check_tag} 与 Cargo.toml 版本 {self.version} 不一致"
                    "（发版前先同步版本号，02 §2.2）"
                )
        self.check_metadata_placeholders()

    # ---- 元数据体检：占位符/缺失法律文本会真的漏到用户机器上，必须显式提示
    def check_metadata_placeholders(self) -> None:
        flat = repr(self.meta)
        hits = [m for m in ("<org>", "example.com") if m in flat]
        if hits:
            warn(f"metadata.toml 仍含占位符 {hits}——发布前必须替换（04 §5.1）")
        if not self.licenses:
            warn(
                "仓库根没有 LICENSE*/COPYING* 文件，安装包不含许可文本"
                "（GPL-3.0 要求随二进制附全文；补上根 LICENSE 后本驱动会自动装进各包）"
            )

    def bin_path(self, name: str, exe: str, target: Target) -> Path:
        for ov in self.args.bin or []:
            k, _, v = ov.partition("=")
            if k == name:
                p = Path(v)
                if not p.exists():
                    die(f"--bin {ov} 指向的文件不存在")
                return p
        base = Path(self.args.bin_dir) if self.args.bin_dir else ROOT / "target" / target.triple / "release"
        p = base / f"{name}{exe}"
        if not p.exists() and not self.args.bin_dir and target.os == host_os():
            # 同平台开发时常用 `cargo build --release`（不带 --target）→ target/release
            alt = ROOT / "target/release" / f"{name}{exe}"
            if alt.exists():
                log(f"  （{target.triple}/release 缺 {p.name}，回退用 {alt.relative_to(ROOT)}）")
                p = alt
        if not p.exists():
            die(
                f"缺少二进制 {p}\n"
                f"  先构建：cargo build --release --target {target.triple} -p hzip-cli -p hzip-gui\n"
                f"  或用 --bin {name}=<path> 指定"
            )
        return p

    triples: list[str] = []

    def bin_dirs(self, targets: list[Target]) -> str:
        return "  ".join(str(Path(self.args.bin_dir) if self.args.bin_dir else ROOT / "target" / t.triple / "release") for t in targets)

    # ---- 模板渲染：占位符漏填会变成用户可见的空字符串，故一律硬校验
    # XML 模板（AppStream/plist）里替换值必须转义——否则 `--target <org>` 这类占位符、
    # 或用户填的 `A & B` 会把文档写成非法 XML（appstreamcli 会直接报 xml-markup-invalid）。
    # 例外：模板里明确要注入**标记**的键（见 XML_RAW）。
    XML_RAW = frozenset({"CATEGORIES_XML", "EXT_LIST"})

    def render(self, name: str, **kw) -> str:
        text = (TPL_DIR / name).read_text(encoding="utf-8")
        is_xml = name.endswith(("xml.in", "plist.in"))
        for k, v in kw.items():
            if is_xml and k not in self.XML_RAW:
                v = xml_escape(str(v))
            text = text.replace("{{" + k + "}}", str(v))
        left = sorted(set(re.findall(r"\{\{([A-Z0-9_]+)\}\}", text)))
        if left:
            die(f"模板 {name} 占位符未提供：{left}")
        return text

    def write(self, path: Path, text: str, mode: int | None = None) -> Path:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8", newline="\n")
        if mode is not None:
            path.chmod(mode)
        return path

    def copy(self, src: Path, dst: Path, mode: int | None = None) -> Path:
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(src, dst)
        if mode is not None:
            dst.chmod(mode)
        return dst

    def artifact(self, path: Path) -> Path:
        digest = hashlib.sha256(path.read_bytes()).hexdigest()
        self.artifacts.append(path)
        log(f"  → {path.name}  ({path.stat().st_size/1048576:.2f} MiB, sha256 {digest[:16]}…)")
        return path

    def write_sha256sums(self) -> None:
        if not self.artifacts:
            return
        lines = []
        for p in self.artifacts:
            digest = hashlib.sha256(p.read_bytes()).hexdigest()
            lines.append(f"{digest}  {p.name}")
        out = self.out_dir / "sha256sums.txt"
        prev = out.read_text(encoding="utf-8").splitlines() if out.exists() else []
        keep = [l for l in prev if l.split("  ", 1)[-1] not in {p.name for p in self.artifacts}]
        out.write_text("\n".join(sorted(set(keep + lines))) + "\n", encoding="utf-8")
        log(f"  → sha256sums.txt ({len(lines)} 条)")


# ----------------------------------------------------------------------- 小工具


def read_toml(path: Path) -> dict:
    """极简 TOML 读取（3.10 无 tomllib；只支持 metadata.toml 用到的子集）。

    支持：`[section]` / `key = "str"` / `key = 123` / `key = true|false` /
    `key = ["a", "b"]`（可跨行）。其他语法一律报错——宁可炸，也不静默读错。
    """
    data: dict = {}
    section = data
    buf: list[str] = []
    pending: str | None = None

    def flush(line_no: int) -> None:
        nonlocal pending, buf
        if pending is None:
            return
        raw = " ".join(x.strip() for x in [pending] + buf).rstrip(",")
        key, _, val = raw.partition("=")
        key, val = key.strip(), val.strip()
        if val.startswith("["):
            items = [s.strip().strip('"') for s in val.strip("[]").split(",") if s.strip()]
            section[key] = items
        elif val.startswith('"'):
            section[key] = val.strip('"')
        elif val in ("true", "false"):
            section[key] = val == "true"
        elif re.fullmatch(r"-?\d+", val):
            section[key] = int(val)
        else:
            die(f"{path}:{line_no} 不支持的 TOML 值：{val!r}")
        pending, buf = None, []

    for i, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        stripped = line.strip()
        if pending is None and (not stripped or stripped.startswith("#")):
            continue
        if stripped.startswith("[") and stripped.endswith("]") and pending is None:
            name = stripped.strip("[]")
            if "." in name:
                die(f"{path}:{i} 不支持嵌套表 {name}")
            data.setdefault(name, {})
            section = data[name]
            continue
        if pending is not None:
            buf.append(stripped)
            if "] " in "" or stripped.endswith("]"):
                flush(i)
            continue
        if "=" not in stripped:
            die(f"{path}:{i} 不是 key = value：{stripped!r}")
        pending = stripped
        if "[" in stripped and not stripped.rstrip().endswith("]"):
            continue
        flush(i)
    return data


def workspace_version() -> str:
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    m = re.search(r"^\[workspace\.package\](.*?)(?=^\[)", text, re.S | re.M)
    if not m:
        die("根 Cargo.toml 里找不到 [workspace.package]")
    v = re.search(r'^version\s*=\s*"([^"]+)"', m.group(1), re.M)
    if not v:
        die("根 Cargo.toml 的 [workspace.package] 里找不到 version")
    return v.group(1)


def iso_date() -> str:
    import datetime

    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")


def rpm_date() -> str:
    import datetime

    return datetime.datetime.now(datetime.timezone.utc).strftime("%a %b %d %Y")


def run(cmd: list[str], cwd: Path | None = None, env: dict | None = None) -> str:
    printable = " ".join(f'"{c}"' if " " in str(c) else str(c) for c in cmd)
    log(f"  $ {printable}")
    full_env = {**os.environ, **(env or {})}
    # 用户级 .NET 的 DOTNET_ROOT 只在「新装的终端」里才生效——老终端里 wix.exe
    # 的 apphost 会报「Failed to resolve hostfxr.dll」。这里按探测结果补上。
    if _user_dotnet_root():
        full_env.setdefault("DOTNET_ROOT", _user_dotnet_root())
    r = subprocess.run(
        [str(c) for c in cmd],
        cwd=str(cwd) if cwd else None,
        env=full_env,
        capture_output=True,
        text=True,
    )
    if r.returncode != 0:
        tail = "\n".join((r.stdout + r.stderr).strip().splitlines()[-25:])
        die(f"命令失败（exit {r.returncode}）：{printable}\n{tail}")
    return r.stdout + r.stderr


# 每个格式的宿主平台与外部工具（--dry-run 会据此体检；缺工具 = 硬错误）
FORMAT_TOOLS: dict[str, tuple[str, tuple[str, ...]]] = {
    "msi": ("windows", ("wix",)),
    "setup": ("windows", ("wix",)),
    "zip": (None, ()),
    "tar.gz": (None, ()),
    "deb": ("linux", ("dpkg-deb",)),
    "rpm": ("linux", ("rpmbuild", "rpm")),
    "appimage": ("linux", ()),   # appimagetool/runtime 由驱动按 pin 下载
    "dmg": ("macos", ("hdiutil", "iconutil", "sips")),
}


def host_os() -> str:
    """宿主平台名，用 TARGETS 的词汇（windows/linux/macos），不是 sys.platform 的原值。

    macOS 的 sys.platform 是 "darwin"，与这里的 "macos" 不同名——直接返回它会让
    `require_host`/`bin_path` 的同平台判断在 macOS 上恒假（04 §5.3 打不出 dmg）。
    """
    if sys.platform.startswith("win"):
        return "windows"
    if sys.platform == "darwin":
        return "macos"
    if sys.platform.startswith("linux"):
        return "linux"
    return sys.platform  # 其他宿主（freebsd 等）：不匹配任何格式，require_host 会拒绝


def require_host(fmt: str, why: str) -> None:
    want = FORMAT_TOOLS[fmt][0]
    if want and host_os() != want:
        die(f"{fmt} 只能在 {want} 上打（{why}）；当前宿主 {host_os()}")


def require_tool(name: str, hint: str = "") -> str:
    p = shutil.which(name)
    if not p:
        die(f"缺少工具 {name}。{hint}")
    return p


def optional_tool(name: str) -> str | None:
    return shutil.which(name)


def download(url: str, sha256: str, dest: Path) -> Path:
    """带 sha256 校验的下载（工具/运行时按 metadata.toml 里的版本+校验和锁定）。"""
    if dest.exists() and hashlib.sha256(dest.read_bytes()).hexdigest() == sha256:
        return dest
    dest.parent.mkdir(parents=True, exist_ok=True)
    import urllib.request

    log(f"  下载 {url}")
    tmp = dest.with_suffix(dest.suffix + ".part")
    with urllib.request.urlopen(url, timeout=180) as r, tmp.open("wb") as f:
        shutil.copyfileobj(r, f)
    got = hashlib.sha256(tmp.read_bytes()).hexdigest()
    if got != sha256:
        tmp.unlink(missing_ok=True)
        die(f"sha256 不匹配：{url}\n  期望 {sha256}\n  实得 {got}\n  （工具换版本时应更新 metadata.toml 的 pin）")
    tmp.replace(dest)
    dest.chmod(0o755)
    return dest


def validate_desktop(path: Path) -> None:
    tool = optional_tool("desktop-file-validate")
    if not tool:
        warn("缺少 desktop-file-validate，跳过 .desktop 校验（CI 上装 desktop-file-utils）")
        return
    run([tool, str(path)])


def validate_metainfo(path: Path) -> None:
    tool = optional_tool("appstreamcli")
    if not tool:
        warn("缺少 appstreamcli，跳过 AppStream 校验（CI 上装 appstream）")
        return
    out = run([tool, "validate", "--no-net", str(path)])
    if "W:" in out or "E:" in out:
        for line in out.splitlines():
            if re.match(r"^\s*[WE]:", line):
                log(f"    {line.strip()}")


# ------------------------------------------------------------------- 目标与载荷


class Target:
    def __init__(self, triple: str) -> None:
        if triple not in TARGETS:
            die(f"未知目标 {triple}；支持：{', '.join(TARGETS)}")
        self.triple = triple
        self.os, self.arch = TARGETS[triple]

    def __str__(self) -> str:
        return self.triple

    @property
    def exe(self) -> str:
        return ".exe" if self.os == "windows" else ""

    @property
    def label(self) -> str:
        """产物名里的架构标签（04 §5 的命名：x86_64 / arm64 / aarch64）。"""
        if self.os == "windows":
            return "x86_64" if self.arch == "x86_64" else "arm64"
        return self.arch

    @property
    def deb_arch(self) -> str:
        return {"x86_64": "amd64", "aarch64": "arm64"}[self.arch]

    @property
    def rpm_arch(self) -> str:
        return self.arch

    @property
    def wix_arch(self) -> str:
        return {"x86_64": "x64", "aarch64": "arm64"}[self.arch]


class Payload:
    """一次打包要用的落地文件（二进制/图标/许可）。"""

    def __init__(self, ctx: Ctx, target: Target) -> None:
        self.ctx = ctx
        self.target = target
        self.cli = ctx.bin_path("hzip", target.exe, target)
        self.gui = ctx.bin_path("hzip-gui", target.exe, target)

    @property
    def icons(self) -> list[tuple[int, Path]]:
        found = []
        for n in LOGO_SIZES:
            p = self.ctx.logo_dir / f"hzip-{n}.png"
            if p.exists():
                found.append((n, p))
        if not found:
            die(f"找不到逐尺寸图标 {self.ctx.logo_dir}/hzip-<N>.png（跑 build/icons/make-icons.ps1）")
        return found


def fmt_ctx(ctx: Ctx) -> dict:
    """模板公共占位符。"""
    m = ctx.meta
    app_id = m["package"]["app_id"]
    # AppStream <developer id> 要的是「厂商」反向域名（应用 id 去掉最后一段）
    developer_id = app_id.rsplit(".", 1)[0] if app_id.count(".") >= 2 else app_id
    return dict(
        DEVELOPER_ID=developer_id,
        NAME=m["package"]["name"],
        VERSION=ctx.version,
        APP_ID=m["package"]["app_id"],
        BUNDLE_ID=m["package"]["bundle_id"],
        VENDOR=m["package"]["vendor"],
        MAINTAINER=m["package"]["maintainer"],
        HOMEPAGE=m["package"]["homepage"],
        ISSUES_URL=m["package"]["issues_url"],
        SPDX=m["package"]["spdx"],
        COPYRIGHT=m["package"]["copyright"],
        SUMMARY=m["package"]["summary"],
        SUMMARY_ZH=m["package"]["summary_zh"],
        CATEGORIES=";".join(m["package"]["categories"]),
        CATEGORIES_XML="\n".join(f"    <category>{c}</category>" for c in m["package"]["categories"]),
        MIME_TYPES=";".join(m["package"]["mime_types"]),
        CLI_BIN="hzip",
        GUI_BIN="hzip-gui",
        DATE=iso_date(),
        DATE_RPM=rpm_date(),
        MIN_MACOS=m["macos"]["min_system_version"],
    )


# ------------------------------------------------------------------ 各格式实现


def label_for(targets: list[Target]) -> str:
    return "universal" if len(targets) > 1 else targets[0].triple


def _lipo_archs(binary: Path) -> list[str] | None:
    """取（fat 或 thin）Mach-O 里的架构列表；两种调用形式都试，任一成功即返回。

    `lipo` 的参数顺序在 Xcode 版本间不一致：老 lipo 容忍 `-verify_arch <arch…> <file>`，
    Xcode 26 的 `<file>` 会被当成**第三个架构名**并直接报
    `unknown architecture specification flag: <file>`（CI 实测，打包在这条命令上失败）。
    用法串是 `<input_file> <command>`，`-archs` 本身不吃参数——两种写法都试最省事，
    也不必把打包成败绑在某个 Xcode 版本的解析习惯上。
    """
    for cmd in (["lipo", "-archs", str(binary)], ["lipo", str(binary), "-archs"]):
        log("  $ " + " ".join(cmd))
        r = subprocess.run(cmd, capture_output=True, text=True)
        if r.returncode == 0:
            return r.stdout.split()
    return None


def stage_bins(ctx: Ctx, targets: list[Target], dest: Path) -> tuple[Path, Path]:
    """把二进制放进 dest 并返回 (cli, gui)。

    单目标 = 拷贝；macOS 并列多目标 = `lipo -create` 合 universal（04 §5.3）。
    """
    dest.mkdir(parents=True, exist_ok=True)
    exe = targets[0].exe
    out = {}
    for name in ("hzip", "hzip-gui"):
        paths = [ctx.bin_path(name, exe, t) for t in targets]
        merged = dest / f"{name}{exe}"
        if len(paths) > 1:
            run(["lipo", "-create", *[str(p) for p in paths], "-output", str(merged)])
            # `lipo -create` 对「输入里没有的架构」是**静默忽略**：漏了 arm64 的包照样
            # 是个合法 fat 文件，装到 M 系机器上才发现跑不了。故按目标逐个核验产物
            # 真的含全部架构（universal 的验收口径，04 §4.2 / §5.3）——失败信息里带上
            # 「实得 vs 期望」，比 lipo 自己那句更可读。
            want = [LIPO_ARCH.get(t.arch, t.arch) for t in targets]
            got = _lipo_archs(merged)
            if got is None:
                die(f"lipo -archs 两种调用形式都失败，无法核验 {merged} 的架构")
            missing = [a for a in want if a not in got]
            if missing:
                die(
                    f"universal 产物缺架构 {missing}（实得 {got}，期望 {want}）："
                    f"先确认两个目标的二进制都构建过（lipo -create 对缺失架构是静默忽略）"
                )
        else:
            ctx.copy(paths[0], merged)
        merged.chmod(0o755)
        out[name] = merged
    return out["hzip"], out["hzip-gui"]


def stage_fhs(ctx: Ctx, payload: Payload, root: Path) -> list[str]:
    """铺 Linux FHS 树（deb/rpm 共用），返回相对路径清单。"""
    t = payload.target
    ctx.copy(payload.cli, root / "usr/bin/hzip", 0o755)
    ctx.copy(payload.gui, root / "usr/bin/hzip-gui", 0o755)
    var = fmt_ctx(ctx)
    desktop_name = f'{ctx.meta["package"]["app_id"]}.desktop'
    d = root / "usr/share/applications" / desktop_name
    ctx.write(d, ctx.render("linux/hzip.desktop.in", **var), 0o644)
    if not ctx.args.no_validate:
        validate_desktop(d)
    mi = root / "usr/share/metainfo" / f'{ctx.meta["package"]["app_id"]}.metainfo.xml'
    ctx.write(
        mi,
        ctx.render(
            "linux/hzip.metainfo.xml.in",
            DESC_LINE1=ctx.meta["package"]["description"][0],
            DESC_LINE2=ctx.meta["package"]["description"][1],
            DESC_LINE3=ctx.meta["package"]["description"][2],
            **var,
        ),
        0o644,
    )
    if not ctx.args.no_validate:
        validate_metainfo(mi)
    for n, src in payload.icons:
        ctx.copy(src, root / f"usr/share/icons/hicolor/{n}x{n}/apps/{ctx.meta['package']['app_id']}.png", 0o644)
    for lic in ctx.licenses:
        ctx.copy(lic, root / "usr/share/doc/hzip" / lic.name, 0o644)
    return [str(p.relative_to(root)) for p in sorted(root.rglob("*")) if p.is_file()]


def build_tar_gz(ctx: Ctx, targets: list[Target]) -> None:
    t = targets[0]
    base = f"hzip-{ctx.version}-{label_for(targets)}"
    root = ctx.work / "portable" / base
    stage_bins(ctx, targets, root)
    ctx.write(root / "README.txt", ctx.render("portable/README.txt.in", **fmt_ctx(ctx)), 0o644)
    for lic in ctx.licenses:
        ctx.copy(lic, root / lic.name, 0o644)
    out = ctx.out_dir / f"{base}.tar.gz"
    epoch = int(os.environ.get("SOURCE_DATE_EPOCH", "0")) or None
    with tarfile.open(out, "w:gz") as tf:
        for p in sorted(root.rglob("*")):
            info = tf.gettarinfo(str(p), arcname=str(p.relative_to(root.parent)))
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            if epoch:
                info.mtime = epoch
            if p.is_file():
                with p.open("rb") as f:
                    tf.addfile(info, f if info.size else None)
    ctx.artifact(out)


def build_zip(ctx: Ctx, targets: list[Target]) -> None:
    t = targets[0]
    base = f"hzip-{ctx.version}-{label_for(targets)}"
    root = ctx.work / "portable" / base
    cli, gui = stage_bins(ctx, targets, root)
    if t.os == "windows":
        sign_windows(ctx, [cli, gui])
    ctx.write(root / "README.txt", ctx.render("portable/README.txt.in", **fmt_ctx(ctx)), 0o644)
    for lic in ctx.licenses:
        ctx.copy(lic, root / lic.name, 0o644)
    out = ctx.out_dir / f"{base}.zip"
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as zf:
        for p in sorted(root.rglob("*")):
            if p.is_file():
                zf.write(p, str(p.relative_to(root.parent)))
    ctx.artifact(out)


def build_deb(ctx: Ctx, targets: list[Target]) -> None:
    require_host("deb", "dpkg-deb 是 Debian 系工具")
    require_tool("dpkg-deb", "Debian/Ubuntu：apt install dpkg-dev")
    t = targets[0]
    payload = Payload(ctx, t)
    stage = ctx.work / "deb"
    root = stage / "root"
    files = stage_fhs(ctx, payload, root)
    var = fmt_ctx(ctx)
    control = ctx.render(
        "linux/deb-control.in",
        DEB_ARCH=t.deb_arch,
        DEPENDS=ctx.meta["deb"]["depends"],
        RECOMMENDS=ctx.meta["deb"]["recommends"],
        SECTION=ctx.meta["deb"]["section"],
        PRIORITY=ctx.meta["deb"]["priority"],
        INSTALLED_SIZE_KB=max(
            1, sum((root / f).stat().st_size for f in files) // 1024
        ),
        DESCRIPTION="\n".join((" " + l) if l.strip() else " ." for l in ctx.meta["package"]["description"]),
        **var,
    )
    (root / "DEBIAN").mkdir(parents=True, exist_ok=True)
    ctx.write(root / "DEBIAN/control", control, 0o644)
    for name in ("postinst", "postrm"):
        ctx.write(root / f"DEBIAN/{name}", ctx.render(f"linux/deb-{name}.in", **var), 0o755)
    out = ctx.out_dir / f"hzip_{ctx.version}_{t.deb_arch}.deb"
    run(["dpkg-deb", "--root-owner-group", "--build", root, out])
    info = run(["dpkg-deb", "--info", out])
    log("    " + " | ".join(l.strip() for l in info.splitlines() if l.startswith(" Version") or l.startswith(" Architecture") or l.startswith(" Installed-Size")))
    ctx.artifact(out)


def build_rpm(ctx: Ctx, targets: list[Target]) -> None:
    require_host("rpm", "rpmbuild 是 RPM 系工具")
    rpmbuild = require_tool("rpmbuild", "Fedora/RHEL：dnf install rpm-build；Debian/Ubuntu：apt install rpm")
    t = targets[0]
    payload = Payload(ctx, t)
    top = ctx.work / "rpmbuild"
    for d in ("BUILD", "RPMS", "SOURCES", "SPECS", "SRPMS"):
        (top / d).mkdir(parents=True, exist_ok=True)
    src = top / "SOURCES"
    files = stage_fhs(ctx, payload, src / "root")
    spec = ctx.render(
        "linux/hzip.spec.in",
        RPM_ARCH=t.rpm_arch,
        GROUP=ctx.meta["rpm"]["group"],
        REQUIRES=ctx.meta["rpm"]["requires"],
        # 弱依赖（中文字体）：整行由这里成形——模板里留 `Recommends:` 空值会产出
        # 一个空字段行，rpm 会当成语法错误。
        RPM_RECOMMENDS="Recommends:     " + ctx.meta["rpm"]["recommends"],
        DESCRIPTION="\n".join(ctx.meta["package"]["description"]),
        FILE_LIST="\n".join("/" + f for f in files),
        **fmt_ctx(ctx),
    )
    spec_path = ctx.write(top / "SPECS/hzip.spec", spec, 0o644)
    run([rpmbuild, "-bb", "--define", f"_topdir {top}", spec_path])
    produced = sorted((top / "RPMS").rglob("*.rpm"))
    if not produced:
        die("rpmbuild 没产出 rpm")
    out = ctx.out_dir / f"hzip-{ctx.version}-1.{t.rpm_arch}.rpm"
    shutil.copy2(produced[0], out)
    listing = run(["rpm", "-qp", "--qf", "%{NAME} %{VERSION}-%{RELEASE} %{ARCH} %{SIZE}\\n", str(out)])
    log(f"    {listing.strip()}")
    ctx.artifact(out)


def ensure_appimagetool(ctx: Ctx, host_arch: str) -> Path:
    m = ctx.meta["appimage"]
    url = m[f"appimagetool_url_{host_arch}"]
    sha = m[f"appimagetool_sha256_{host_arch}"]
    return download(url, sha, ctx.cache / f"appimagetool-{host_arch}-{m['appimagetool_version']}.AppImage")


def ensure_runtime(ctx: Ctx, arch: str) -> Path:
    m = ctx.meta["appimage"]
    return download(
        m[f"runtime_url_{arch}"],
        m[f"runtime_sha256_{arch}"],
        ctx.cache / f"runtime-{arch}-{m['runtime_version']}",
    )


def build_appimage(ctx: Ctx, targets: list[Target]) -> None:
    require_host("appimage", "appimagetool 是 Linux ELF 工具")
    t = targets[0]
    payload = Payload(ctx, t)
    appdir = ctx.work / "AppDir"
    files = stage_fhs(ctx, payload, appdir)
    var = fmt_ctx(ctx)
    app_id = ctx.meta["package"]["app_id"]
    # AppImage 规范要求 .desktop 与图标出现在 **AppDir 根**（appimagetool 找不到就
    # "Desktop file not found, aborting"）；usr/share 下那份是给发行版集成用的，两份都要有。
    ctx.copy(appdir / "usr/share/applications" / f"{app_id}.desktop", appdir / f"{app_id}.desktop", 0o644)
    # 兼容名：appimagetool 只认 <id>.appdata.xml，桌面环境认 <id>.metainfo.xml（都是同一份内容）
    ctx.copy(appdir / "usr/share/metainfo" / f"{app_id}.metainfo.xml", appdir / "usr/share/metainfo" / f"{app_id}.appdata.xml", 0o644)
    ctx.write(appdir / "AppRun", ctx.render("linux/AppRun.in", **var), 0o755)
    ctx.copy(payload.cli, appdir / "usr/bin/hzip", 0o755)
    ctx.copy(payload.gui, appdir / "usr/bin/hzip-gui", 0o755)
    big = max(payload.icons)[1]
    ctx.copy(big, appdir / f"{app_id}.png", 0o644)
    link = appdir / ".DirIcon"
    if link.exists() or link.is_symlink():
        link.unlink()
    link.symlink_to(f"{app_id}.png")
    host_arch = "aarch64" if platform.machine() in ("aarch64", "arm64") else "x86_64"
    tool = ensure_appimagetool(ctx, host_arch)
    out = ctx.out_dir / f"hzip-{ctx.version}-{t.arch}.AppImage"
    # runtime 恒由驱动按**目标架构**取用（pin 了 URL+sha256）：appimagetool 自己去下
    # 既不锁版本，又让打包变成联网操作；交叉架构时它还必然拿错架构的 runtime。
    # --no-appstream：appimagetool 自带的 AppStream 校验要联网、且对"URL 暂时不可达"这类
    # 无关警告直接中止构建；驱动在打包前已用 `appstreamcli --no-net` 做过确定性校验。
    cmd = [str(tool), "--appimage-extract-and-run", "--no-appstream",
           "--runtime-file", str(ensure_runtime(ctx, t.arch))]
    cmd += [str(appdir), str(out)]
    run(cmd, env={"ARCH": t.arch})
    if t.arch == host_arch:
        # 自证：解包看关键路径（跨架构产物本机跑不了，跳过；04 §8 AppImage 行）
        probe = ctx.work / "appimage-probe"
        probe.mkdir(parents=True, exist_ok=True)
        run([str(out), "--appimage-extract"], cwd=probe)
        for want in ("squashfs-root/AppRun", "squashfs-root/usr/bin/hzip", "squashfs-root/usr/bin/hzip-gui"):
            if not (probe / want).exists():
                die(f"AppImage 解包后缺 {want}")
    ctx.artifact(out)


def sign_windows(ctx: Ctx, paths: list[Path]) -> None:
    """可选：signtool 双签（SHA-256 + 时间戳，04 §5.1）。无凭据则只提示一次。"""
    thumb = ctx.args.sign_thumbprint or os.environ.get("HZIP_SIGN_THUMBPRINT")
    pfx = ctx.args.sign_pfx or os.environ.get("HZIP_SIGN_PFX")
    if not (thumb or pfx):
        warn("未提供代码签名凭据（--sign-thumbprint / --sign-pfx）：发布 unsigned 包（04 §9）")
        return
    if pfx and not Path(pfx).exists():
        warn(f"签名证书文件不存在：{pfx} → 产 unsigned 包")
        return
    signtool = require_tool("signtool", "装 Windows SDK / 打开「签名工具」组件")
    base = [signtool, "sign", "/fd", "sha256", "/tr", "http://timestamp.digicert.com", "/td", "sha256"]
    if pfx:
        base += ["/f", pfx]
        pw = ctx.args.sign_password or os.environ.get("HZIP_SIGN_PASSWORD")
        if pw:
            base += ["/p", pw]
    else:
        base += ["/sha1", thumb]
    for path in paths:
        run(base + [str(path)])
        run([signtool, "verify", "/pa", "/v", str(path)])


def _user_dotnet_root() -> str | None:
    """用户级 .NET 运行时/SDK 位置（dotnet-install -InstallDir %LOCALAPPDATA%）。"""
    if os.environ.get("DOTNET_ROOT"):
        return os.environ["DOTNET_ROOT"]
    for base in (os.environ.get("LOCALAPPDATA"), os.environ.get("UserProfile")):
        if not base:
            continue
        d = Path(base) / ("Microsoft/dotnet" if base.endswith(("Local", "local")) or "Local" in base else "dotnet")
        if (d / "dotnet.exe").exists():
            return str(d)
    return None


def _wix_env() -> dict:
    """framework-dependent 的 wix.exe 靠 DOTNET_ROOT 找运行时；老终端里该变量往往还没生效。"""
    env = {**os.environ}
    root = _user_dotnet_root()
    if root and not env.get("DOTNET_ROOT"):
        env["DOTNET_ROOT"] = root
    return env


def _probe_wix(path: str) -> tuple[bool, str, str]:
    try:
        r = subprocess.run([path, "--version"], capture_output=True, text=True, env=_wix_env(), timeout=60)
    except (OSError, subprocess.TimeoutExpired) as e:
        return False, "", str(e)
    if r.returncode != 0:
        lines = (r.stderr or r.stdout).strip().splitlines()
        return False, "", (lines[0] if lines else f"exit {r.returncode}")
    return True, (r.stdout or r.stderr).strip(), ""


def find_wix(ctx: Ctx, want: str) -> tuple[str, str]:
    r"""按候选顺序找一个**能跑起来**的 WiX，而不是只看 PATH 上第一个。

    本机踩过的坑：PATH 上先出现的是 scoop 的 wix（framework-dependent，需要 .NET），
    装在 `%USERPROFILE%\.dotnet\tools` 的 v5 反而排在后面 —— 只认 PATH 就会
    卡在「You must install .NET」上。
    """
    cands: list[str] = []
    if ctx.args.wix:
        cands.append(str(ctx.args.wix))
    if os.environ.get("HZIP_WIX"):
        cands.append(os.environ["HZIP_WIX"])
    dotnet_tools = Path(os.environ.get("USERPROFILE", "")) / ".dotnet" / "tools" / "wix.exe"
    if dotnet_tools.exists():
        cands.append(str(dotnet_tools))
    for name in ("wix", "wix.exe"):
        found = shutil.which(name)
        if found:
            cands.append(found)
            break
    tried, mismatch = [], None
    for cand in dict.fromkeys(cands):
        if not Path(cand).exists():
            tried.append(f"{cand} → 文件不存在")
            continue
        ok, ver, why = _probe_wix(cand)
        if not ok:
            tried.append(f"{cand} → {why}")
            continue
        if ver.startswith(want):
            log(f"  WiX：{cand}（{ver}）")
            return cand, ver
        mismatch = (cand, ver)
        tried.append(f"{cand} → 可用但版本 {ver} ≠ pin {want}")
    if mismatch:
        return mismatch[0], mismatch[1]
    die(
        "没有可用的 WiX。已尝试：\n  " + "\n  ".join(tried or ["（没有任何候选）"])
        + f"\n  装法：dotnet tool install --global wix --version {want}"
        "（.NET SDK 与工具装在用户级即可；见 build/README.md「前置工具」）"
    )
    raise AssertionError("unreachable")


def build_msi(ctx: Ctx, targets: list[Target]) -> None:
    t = targets[0]
    if t.os != "windows":
        die("msi 只能在 Windows 上打")
    want = ctx.meta["windows"]["wix_version"]
    wix, ver = find_wix(ctx, want)
    if not ver.startswith(want):
        warn(f"WiX 版本 {ver} != metadata 里 pin 的 {want}（产物可能有差异）")
    for ext in ("WixToolset.UI.wixext", "WixToolset.Util.wixext"):
        try:
            run([wix, "extension", "add", "-g", f"{ext}/{want}"])
        except SystemExit:
            die(f"WiX 扩展 {ext} 安装失败（首次需要网络：nuget.org）")
    b = ctx.work / "msi"
    bin_stage = b / "bin"
    stage_bins(ctx, targets, bin_stage)
    sign_windows(ctx, sorted(bin_stage.iterdir()))   # MSI 里必须是已签名的 exe（04 §5.1）
    b.mkdir(parents=True, exist_ok=True)
    shutil.copy2(TPL_DIR / "windows/hzip.wxs.in", b / "hzip.wxs")
    # GPL-3.0 随附义务：许可全文要**落进安装目录**（INSTALLFOLDER）。`--license-rtf`
    # 只是可选的 WixUI「安装前显示」页，二者独立——没给 rtf 时安装目录里也必须有全文。
    # 许可文件名可能变（如将来加 COPYING），故与 extensions.wxi 一样由驱动生成清单，
    # 再用 `HasLicenses` 条件在模板里 include/引用（空 ComponentGroup 会导致 WiX 报错）。
    lic_stage = b / "licenses"
    lic_stage.mkdir(parents=True, exist_ok=True)
    lic_comps = []
    for lic in ctx.licenses:
        ctx.copy(lic, lic_stage / lic.name, 0o644)
        sid = re.sub(r"[^A-Za-z0-9_.]", "_", lic.name)
        guid = str(uuid.uuid5(GUID_NS, f"hzip.installer.license.{lic.name}")).upper()
        lic_comps.append(
            f'    <Component Id="Lic_{sid}" Guid="{guid}">\n'
            f'      <File Id="filLic_{sid}" Source="$(var.LicenseDir)\\{xml_escape(lic.name)}" KeyPath="yes"/>\n'
            f"    </Component>"
        )
    ctx.write(
        b / "licenses.wxi",
        '<?xml version="1.0" encoding="utf-8"?>\n<Include xmlns="http://wixtoolset.org/schemas/v4/wxs">\n'
        '  <Fragment>\n    <ComponentGroup Id="LicenseComponents" Directory="INSTALLFOLDER">\n'
        + "\n".join(lic_comps)
        + "\n    </ComponentGroup>\n  </Fragment>\n</Include>\n",
        0o644,
    )
    exts = ctx.meta["package"]["extensions"]
    rows = "\n".join(
        f'''  <Component Id="Ext{e.replace('.', '_')}" Guid="{str(uuid.uuid5(GUID_NS, f"hzip.installer.ext.{e}")).upper()}">
    <RegistryValue Root="HKLM" Key="SOFTWARE\\Classes\\.{e}\\OpenWithProgids" Name="hzip.Archive" Type="string" Value="" KeyPath="yes"/>
  </Component>'''
        for e in exts
    )
    # 组件组必须落在 <Fragment> 里（WiX v4/v5 schema），而 <?include?> 在 <Wix> 层
    # Capabilities\FileAssociations：「默认应用」候选清单，逐扩展名指向我们的 ProgID。
    # 注意用 %-格式化而非嵌套引号的 f-string——驱动要能在 Python 3.10（Ubuntu 22.04）跑。
    cap_key = "SOFTWARE\\%s\\Capabilities\\FileAssociations" % ctx.meta["package"]["name"]
    assert exts, "metadata.toml 的 extensions 不能为空（组件需要一个 KeyPath）"
    cap_rows = "\n".join(
        # 首个扩展名的值兼作组件 KeyPath（同一组件里不能有重复值）
        '    <RegistryValue Root="HKLM" Key="%s" Name=".%s" Type="string" Value="hzip.Archive"%s/>'
        % (cap_key, e, ' KeyPath="yes"' if i == 0 else "")
        for i, e in enumerate(exts)
    )
    cap_guid = str(uuid.uuid5(GUID_NS, "hzip.installer.fileassoc.capabilities")).upper()
    ctx.write(
        b / "extensions.wxi",
        '<?xml version="1.0" encoding="utf-8"?>\n<Include xmlns="http://wixtoolset.org/schemas/v4/wxs">\n'
        "  <Fragment>\n    <ComponentGroup Id=\"FileAssocExtensions\">\n"
        + rows
        # 注意：这个 <Component> 必须在**被 Feature 引用的 ComponentGroup 里**，
        # 否则 WiX 会把它当孤儿丢掉（静默不注册 Capabilities\FileAssociations）。
        + '    <Component Id="FileAssocCapabilities" Guid="%s">\n' % cap_guid
        + cap_rows
        + "\n    </Component>\n    </ComponentGroup>\n  </Fragment>\n</Include>\n",
        0o644,
    )
    shell_dir = Path(ctx.args.shell_ext_dir) if ctx.args.shell_ext_dir else ROOT / "shell/hzip-shell/target/shell-ext"
    with_shell = 0
    if (shell_dir / "hzip-shell.dll").exists() and (shell_dir / "hzip-shellmenu.msix").exists():
        with_shell = 1
    elif ctx.args.require_shell_ext:
        die(f"--require-shell-ext 但 {shell_dir} 下缺 hzip-shell.dll / hzip-shellmenu.msix（跑 build/shell-ext/build-shell-ext.ps1）")
    else:
        warn(f"未找到 {shell_dir} 的 hzip-shell.dll / hzip-shellmenu.msix——MSI 不含右键菜单扩展（04 §5.1）")
    out = ctx.out_dir / f"hzip-{ctx.version}-setup-{t.label}.msi"
    cmd = [
        wix, "build", str(b / "hzip.wxs"),
        "-arch", t.wix_arch, "-culture", "zh-CN",
        "-ext", "WixToolset.UI.wixext", "-ext", "WixToolset.Util.wixext",
        "-d", f"Version={ctx.version}",
        "-d", f"Vendor={ctx.meta['package']['vendor']}",
        "-d", f"ProductName={ctx.meta['package']['name']}",
        "-d", f"UpgradeCode={ctx.meta['windows']['upgrade_code']}",
        "-d", f"BinDir={bin_stage}",
        "-d", f"IcoPath={ctx.ico}",
        "-d", f"ArchLabel={t.label}",
        "-d", f"WithShellExt={with_shell}",
        "-d", f"HasLicenseRtf={1 if ctx.args.license_rtf else 0}",
        "-d", f"LicenseRtf={Path(ctx.args.license_rtf).resolve() if ctx.args.license_rtf else ''}",
        "-d", f"HasLicenses={1 if ctx.licenses else 0}",
        "-d", f"LicenseDir={lic_stage}",
        "-o", str(out),
    ]
    if with_shell:
        cmd += ["-d", f"ShellExtDir={shell_dir}"]
    run(cmd, cwd=b)
    sign_windows(ctx, [out])
    ctx.artifact(out)


def build_dmg(ctx: Ctx, targets: list[Target]) -> None:
    require_host("dmg", "hdiutil/iconutil 是 macOS 工具")
    for tool in ("hdiutil", "iconutil", "sips"):
        require_tool(tool)
    univ = len(targets) > 1
    lab = "universal" if univ else targets[0].label
    app = ctx.work / "dmg" / "hzip.app"
    res = app / "Contents/Resources"
    macos_bin = app / "Contents/MacOS"
    res.mkdir(parents=True, exist_ok=True)
    macos_bin.mkdir(parents=True, exist_ok=True)
    stage_bins(ctx, targets, macos_bin)
    # GPL-3.0 随二进制附许可全文的义务：放进 Contents/Resources（macOS 约定位置），
    # 用户把 .app 拖进「应用程序」即带走全文。
    for lic in ctx.licenses:
        ctx.copy(lic, res / lic.name, 0o644)
    iconset = ctx.work / "hzip.iconset"
    iconset.mkdir(parents=True, exist_ok=True)
    src_icons = dict(Payload(ctx, targets[0]).icons)
    # iconutil 要求的档位名 -> 像素尺寸（04 §5.5：macOS 的 .icns 由 PNG 集生成）
    need = [
        ("icon_16x16.png", 16), ("icon_16x16@2x.png", 32),
        ("icon_32x32.png", 32), ("icon_32x32@2x.png", 64),
        ("icon_128x128.png", 128), ("icon_128x128@2x.png", 256),
        ("icon_256x256.png", 256), ("icon_256x256@2x.png", 512),
    ]
    largest = max(src_icons)
    for name, px in need:
        if px in src_icons:
            ctx.copy(src_icons[px], iconset / name)
        else:
            # 没有该档：由最近的更大档缩放（全无更大档则放大 256，并告警）
            up = min((s for s in src_icons if s >= px), default=largest)
            if up < px:
                warn(f"图标缺 {px}px 档，由 {up}px 放大（04 §5.5：建议补 crates/hzip-gui/assets/logo/hzip-{px}.png）")
            run(["sips", "-z", str(px), str(px), str(src_icons[up]), "--out", str(iconset / name)])
    run(["iconutil", "-c", "icns", str(iconset), "-o", str(res / "AppIcon.icns")])
    ext_lines = "\n".join(f"                <string>{e}</string>" for e in ctx.meta["package"]["extensions"])
    ctx.write(app / "Contents/Info.plist", ctx.render("macos/Info.plist.in", EXT_LIST=ext_lines, **fmt_ctx(ctx)), 0o644)
    sign_id = ctx.args.macos_sign_id or os.environ.get("HZIP_MACOS_SIGN_ID")
    if sign_id and not optional_tool("codesign"):
        die(f"给了签名身份 {sign_id} 但找不到 codesign")
    if sign_id:
        for name in ("hzip", "hzip-gui"):
            run(["codesign", "--force", "--options", "runtime", "--timestamp", "--sign", sign_id, str(macos_bin / name)])
        run(["codesign", "--force", "--options", "runtime", "--timestamp", "--sign", sign_id, str(app)])
        run(["codesign", "--verify", "--deep", "--strict", "--verbose=2", str(app)])
    else:
        warn("未提供 --macos-sign-id / HZIP_MACOS_SIGN_ID：产 unsigned .app（04 §9）")
    staging = ctx.work / "dmg/staging"
    staging.mkdir(parents=True, exist_ok=True)
    if (staging / "hzip.app").exists():
        shutil.rmtree(staging / "hzip.app")
    shutil.copytree(app, staging / "hzip.app", symlinks=True)
    link = staging / "Applications"
    if not link.exists():
        link.symlink_to("/Applications")
    ctx.write(staging / "README.txt", ctx.render("macos/dmg-readme.txt", **fmt_ctx(ctx)), 0o644)
    out = ctx.out_dir / f"hzip-{ctx.version}-{lab}.dmg"
    run(["hdiutil", "create", "-volname", f"hzip {ctx.version}", "-srcfolder", str(staging),
         "-ov", "-format", "UDZO", str(out)])
    profile = ctx.args.macos_notary_profile or os.environ.get("HZIP_MACOS_NOTARY_PROFILE")
    if sign_id and profile:
        run(["xcrun", "notarytool", "submit", str(out), "--keychain-profile", profile, "--wait"])
        run(["xcrun", "stapler", "staple", str(out)])
        run(["xcrun", "stapler", "validate", str(out)])
    elif sign_id:
        warn("已签名但未提供公证凭据（--macos-notary-profile）：dmg 未公证（04 §5.3）")
    ctx.artifact(out)


FORMATS = {
    "msi": (build_msi, ("windows",)),
    "setup": (build_msi, ("windows",)),  # 文档/口语里的「setup」就是 MSI 安装向导（04 §5.1）
    "zip": (build_zip, ("windows", "linux", "macos")),
    "tar.gz": (build_tar_gz, ("linux", "macos")),
    "deb": (build_deb, ("linux",)),
    "rpm": (build_rpm, ("linux",)),
    "appimage": (build_appimage, ("linux",)),
    "dmg": (build_dmg, ("macos",)),
}


# ------------------------------------------------------------------------ 主流程


def default_formats(osname: str) -> list[str]:
    return [f for f, (_, oss) in FORMATS.items() if osname in oss and f != "setup"]


def main() -> int:
    ap = argparse.ArgumentParser(
        description="hzip 多平台安装包驱动（04 §5）",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    ap.add_argument("--target", help="rust 目标三元组；macOS 打 universal 包用逗号并列两个")
    ap.add_argument("--formats", help="逗号分隔：msi/setup,zip,tar.gz,deb,rpm,appimage,dmg")
    ap.add_argument("--bin-dir", help="二进制目录（默认 target/<triple>/release）")
    ap.add_argument("--bin", action="append", metavar="NAME=PATH", help="覆盖单个二进制（可多次）")
    ap.add_argument("--out-dir", default="dist", help="产物目录（默认 dist/）")
    ap.add_argument("--cache-dir", help="工具/运行时缓存目录（默认 ~/.cache/hzip-package）")
    ap.add_argument("--check-tag", metavar="vX.Y.Z", help="断言 tag 与 Cargo.toml 版本一致")
    ap.add_argument("--shell-ext-dir", help="hzip-shell.dll/hzip-shellmenu.msix 所在目录（默认 shell/hzip-shell/target/shell-ext）")
    ap.add_argument("--require-shell-ext", action="store_true", help="缺 shell 扩展则报错（发布用）")
    ap.add_argument("--wix", help="wix.exe 路径（默认 PATH 上的 wix）")
    ap.add_argument("--license-rtf", help="WixUI 许可页用的 .rtf（可选）")
    ap.add_argument("--sign-pfx", help="Windows 代码签名 .pfx（可选）")
    ap.add_argument("--sign-thumbprint", help="Windows 代码签名证书指纹（可选）")
    ap.add_argument("--sign-password", help=".pfx 口令（建议用环境变量）")
    ap.add_argument("--macos-sign-id", help="macOS 签名身份（可选，见 04 §5.3）")
    ap.add_argument("--macos-notary-profile", help="notarytool keychain profile（可选）")
    ap.add_argument("--no-validate", action="store_true", help="跳过 .desktop / AppStream 校验")
    ap.add_argument("--dry-run", action="store_true", help="只打印计划（含缺什么工具），不落地")
    ap.add_argument("--list", action="store_true", help="列出支持的格式与目标")
    ap.add_argument("-v", "--verbose", action="store_true")
    args = ap.parse_args()

    if args.list:
        log("目标：")
        for t, (o, a) in TARGETS.items():
            log(f"  {t:28s} {o:8s} {a}")
        log("格式：")
        for f, (_, oss) in FORMATS.items():
            log(f"  {f:10s} 平台：{','.join(oss)}")
        return 0

    if not args.target:
        ap.error("需要 --target（或用 --list 看支持的目标）")

    triples = [x.strip() for x in args.target.split(",") if x.strip()]
    targets = [Target(t) for t in triples]
    if len({t.os for t in targets}) != 1:
        die("逗号并列的目标必须同属一个平台（macOS universal 用两个 darwin 目标）")
    if len(targets) > 1 and targets[0].os != "macos":
        die("只有 macOS 支持并列多目标（lipo 出 universal）")

    ctx = Ctx(args)
    ctx.triples = triples
    osname = targets[0].os
    formats = [f.strip() for f in (args.formats or ",".join(default_formats(osname))).split(",") if f.strip()]
    for f in formats:
        if f not in FORMATS:
            die(f"未知格式 {f}；支持：{', '.join(FORMATS)}")
        if osname not in FORMATS[f][1]:
            die(f"格式 {f} 不支持 {osname}（支持：{','.join(FORMATS[f][1])}）")

    log(f"hzip {ctx.version}  →  {', '.join(triples)}")
    log(f"格式：{', '.join(formats)}    产物目录：{ctx.out_dir}")
    log(f"二进制目录：{ctx.bin_dirs(targets)}")
    if args.dry_run:
        for f in formats:
            want_host, tools = FORMAT_TOOLS[f]
            host_ok = not want_host or host_os() == want_host
            missing = [x for x in tools if not optional_tool(x)]
            state = "可打" if host_ok and not missing else "不可打"
            why = "" if host_ok else f"（需在 {want_host} 上跑）"
            why += f" 缺工具：{','.join(missing)}" if missing else ""
            log(f"  [dry-run] {f:9s} {state} {why}")
        log("（--dry-run：未落地任何产物）")
        return 0

    for f in formats:
        log(f"[{f}]")
        try:
            FORMATS[f][0](ctx, targets)
        except KeyError as e:
            die(f"metadata.toml 缺配置项 {e}（格式 {f} 需要；见 build/package/README.md）")
    ctx.write_sha256sums()
    log("完成。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
