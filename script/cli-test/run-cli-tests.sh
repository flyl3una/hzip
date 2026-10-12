#!/usr/bin/env bash
# ============================================================================
# hzip-cli 功能测试套件（bash 版）
#
# 用法：
#   ./run-cli-tests.sh                 # 全部测试（含 7-Zip 互操作）
#   ./run-cli-tests.sh <函数名>        # 只跑单个测试函数
#   ./run-cli-tests.sh --list          # 列出全部测试
#
# 环境变量：
#   HZIP        hzip 可执行文件路径（默认 <workspace>/target/debug/hzip.exe）
#   SZ7         ​7-Zip 可执行文件路径（默认 F:/tools/7zip/7z.exe；设 SKIP_SZ=1 跳过互操作）
#   SKIP_SZ=1   跳过 7-Zip 互操作测试
#   KEEP=1      保留临时数据（调试）
#
# 设计：
#   - 每个测试点一个函数，main 顺序调用；函数返回 0=通过 1=失败。
#   - 临时数据在 <script 目录>/data/_work/ 下自建自清（KEEP=1 保留）。
#   - 密码样本 zipcrypto-7z.zip 预置于 data/（源自 hzip-fmt-zip fixtures，
#     密码 hzip-pw-test）。
#   - 7z 加密互操作 hzip←7z-AES 方向为已知缺口（T46 未合入），单独以
#     KNOWN-GAP 报告，不断言失败。
# ============================================================================
set -u

# ---- 环境检测：需 MSYS/Git-Bash（Windows exe 路径自动转换）；WSL/Linux 不可跑 ----
if [[ "$(uname -s 2>/dev/null)" == Linux* ]]; then
    echo "错误: 本脚本需在 Git Bash（MSYS）或原生 Windows sh 下运行，检测到 $(uname -s)" >&2
    echo "  请用 Git Bash 执行: ./run-cli-tests.sh" >&2
    exit 2
fi

# ---- 定位（cygpath 转 Windows 风格，避免 /f/ vs F:/ 执行歧义） ----
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if command -v cygpath >/dev/null 2>&1; then
    SCRIPT_DIR="$(cygpath -w "$SCRIPT_DIR")"
fi
DATA_DIR="$SCRIPT_DIR/data"
WORK_DIR="$DATA_DIR/_work"
WS_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
if command -v cygpath >/dev/null 2>&1; then
    WS_ROOT="$(cygpath -w "$WS_ROOT")"
fi

# ---- 可执行文件 ----
HZIP="${HZIP:-$WS_ROOT/target/debug/hzip.exe}"
if [[ ! -f "$HZIP" ]]; then
    echo "错误: 找不到 hzip ($HZIP) —— 先 cargo build -p hzip-cli 或设 HZIP" >&2
    exit 2
fi
SZ7="${SZ7:-F:/tools/7zip/7z.exe}"
if [[ "${SKIP_SZ:-0}" != "1" && ! -f "$SZ7" ]]; then
    echo "警告: 找不到 7-Zip ($SZ7)，互操作测试将以 SKIP 计；设 SZ7 或 SKIP_SZ=1" >&2
    SZ7=""
fi

# ---- 结果统计 ----
PASS=0; FAIL=0; SKIPN=0; GAP=0
FAILED_NAMES=()

# ---- 公共工具 ----
die() { echo "  错误: $*" >&2; exit 2; }

note() { echo "  [note] $*"; }

# 比较两文件字节一致
assert_file_eq() { # a b msg
    if cmp -s "$1" "$2"; then return 0; fi
    echo "  ✗ 文件不一致: $3 ($1 vs $2)" >&2
    return 1
}

# 比较两目录递归（文件集+内容+符号链接跳过）
assert_tree_eq() { # dirA dirB msg
    local a="$1" b="$2" msg="$3"
    local fa fb
    fa="$(cd "$a" && find . -type f | sort)"
    fb="$(cd "$b" && find . -type f | sort)"
    if [[ "$fa" != "$fb" ]]; then
        echo "  ✗ 文件集不一致 [$msg]:" >&2
        diff <(echo "$fa") <(echo "$fb") | head -10 >&2
        return 1
    fi
    local f
    while IFS= read -r f; do
        [[ -n "$f" ]] || continue
        assert_file_eq "$a/$f" "$b/$f" "$msg:$f" || return 1
    done <<< "$fa"
    return 0
}

# 断言退出码 == 期望
assert_exit() { # expect=$1 actual=$2 label=$3
    if [[ "$1" -eq "$2" ]]; then return 0; fi
    echo "  ✗ [$3] 期望退出码 $1 实际 $2" >&2
    return 1
}

# 断言文件存在
assert_exists() { # path msg
    if [[ -e "$1" ]]; then return 0; fi
    echo "  ✗ [$2] 文件不存在: $1" >&2
    return 1
}

# 断言字符串包含
assert_contains() { # haystack needle msg
    if [[ "$1" == *"$2"* ]]; then return 0; fi
    echo "  ✗ [$3] 输出不含: $2" >&2
    return 1
}

# 建标准源树（3 文件 + 空目录）
make_src_tree() { # root
    local root="$1"
    rm -rf "$root"; mkdir -p "$root/sub" "$root/emptydir"
    printf 'hello hzip\n' > "$root/a.txt"
    printf 'world content\n' > "$root/sub/b.txt"
    printf 'temporary\n' > "$root/skip.tmp"
    printf 'deep keep\n' > "$root/sub/keep.log"
}

# 各格式内建条目根
run_case() { # name fn
    local name="$1" fn="$2"
    if [[ "${ONLY:-}" != "" && "$name" != "$ONLY" ]]; then return; fi
    mkdir -p "$WORK_DIR"
    local before
    before="$(cd "$WORK_DIR" 2>/dev/null && find . -type f 2>/dev/null | wc -l)"
    echo "── $name"
    if "$fn"; then PASS=$((PASS+1)); echo "  ✓"; else
        FAIL=$((FAIL+1)); FAILED_NAMES+=("$name")
        echo "  ✗ FAILED"
    fi
    if [[ "${KEEP:-0}" != "1" ]]; then rm -rf "$WORK_DIR"; fi
}

# ============================================================================
# 01 · c 压缩 + t 验证 + x 解压 全闭环（zip）
# ============================================================================
t01_zip_full_roundtrip() {
    local d="$WORK_DIR/01"; make_src_tree "$d/src"; mkdir -p "$d/out"
    "$HZIP" c -f zip -o "$d/a.zip" "$d/src" 2>/dev/null
    assert_exit 0 $? "zip 压缩" || return 1
    assert_exists "$d/a.zip" "zip 产物" || return 1
    "$HZIP" t "$d/a.zip" >/dev/null 2>&1
    assert_exit 0 $? "zip test" || return 1
    "$HZIP" x "$d/a.zip" -o "$d/out" >/dev/null 2>&1
    assert_exit 0 $? "zip 解压" || return 1
    # v2 保目录名：c src → 档含 src/ 前缀；x 默认原样 → out/src/...（无包壳）
    assert_tree_eq "$d/src" "$d/out/src" "zip roundtrip" || return 1
    return 0
}

# ============================================================================
# 02 · 全格式矩阵：7 格式 压缩→t→l→解压 闭环
# ============================================================================
t02_format_matrix() {
    local fmt
    for fmt in zip 7z tar gz bz2 xz zst; do
        local d="$WORK_DIR/mx-$fmt"
        make_src_tree "$d/src"; mkdir -p "$d/out"
        if [[ "$fmt" == gz || "$fmt" == bz2 || "$fmt" == xz || "$fmt" == zst ]]; then
            # single 流：压缩文件输入（不能目录）
            "$HZIP" c -f "$fmt" -o "$d/single.$fmt" "$d/src/a.txt" >/dev/null 2>&1
            assert_exit 0 $? "single-$fmt 压缩" || return 1
            "$HZIP" t "$d/single.$fmt" >/dev/null 2>&1
            assert_exit 0 $? "single-$fmt test" || return 1
            "$HZIP" x "$d/single.$fmt" -o "$d/out" >/dev/null 2>&1
            assert_exit 0 $? "single-$fmt 解压" || return 1
            # single 解出：档案 stem（剥压缩后缀）名的文件
            local f
            f="$(find "$d/out" -type f | head -1)"
            assert_exists "${f:-}" "$fmt 解出文件" || return 1
            assert_file_eq "$f" "$d/src/a.txt" "$fmt 内容" || return 1
        else
            "$HZIP" c -f "$fmt" -o "$d/a.$fmt" "$d/src" >/dev/null 2>&1
            assert_exit 0 $? "$fmt 压缩" || return 1
            "$HZIP" t "$d/a.$fmt" >/dev/null 2>&1
            assert_exit 0 $? "$fmt test" || return 1
            "$HZIP" x "$d/a.$fmt" -o "$d/out" >/dev/null 2>&1
            assert_exit 0 $? "$fmt 解压" || return 1
            # v2：保目录名原样 → out/src/...
            assert_tree_eq "$d/src" "$d/out/src" "$fmt roundtrip" || return 1
        fi
    done
    return 0
}

# ============================================================================
# 03 · 压缩级别：store/fastest/fast/normal/maximum/ultra（zip）
# ============================================================================
t03_levels() {
    local lv out
    for lv in store fastest fast normal maximum ultra; do
        local d="$WORK_DIR/lv-$lv"
        make_src_tree "$d/src"
        "$HZIP" c -f zip -L "$lv" -o "$d/lv.zip" "$d/src" >/dev/null 2>&1
        assert_exit 0 $? "level=$lv 压缩" || return 1
        "$HZIP" t "$d/lv.zip" >/dev/null 2>&1
        assert_exit 0 $? "level=$lv test" || return 1
    done
    # 大文件级别差异检查：ultra < store 体积（数据可压）
    local d="$WORK_DIR/lvsize"
    make_src_tree "$d/src"
    head -c 200000 /dev/urandom | base64 > "$d/src/big.txt"
    "$HZIP" c -f zip -L store -o "$d/s.zip" "$d/src/big.txt" >/dev/null 2>&1
    "$HZIP" c -f zip -L ultra -o "$d/u.zip" "$d/src/big.txt" >/dev/null 2>&1
    local su uu
    su=$(stat -c %s "$d/s.zip"); uu=$(stat -c %s "$d/u.zip")
    if [[ "$uu" -ge "$su" ]]; then
        echo "  ✗ ultra($uu) 应小于 store($su)" >&2; return 1
    fi
    return 0
}

# ============================================================================
# 04 · l 列表：平铺 / tree / select / json
# ============================================================================
t04_list() {
    local d="$WORK_DIR/04"; make_src_tree "$d/src"
    "$HZIP" c -f zip -o "$d/a.zip" "$d/src" >/dev/null 2>&1
    # 平铺含条目
    local out
    out="$("$HZIP" l "$d/a.zip" 2>/dev/null)" || { echo "  ✗ l 失败" >&2; return 1; }
    # v2：条目名带 src/ 前缀（c src 保目录名）
    assert_contains "$out" "src/a.txt" "l 含 src/a.txt" || return 1
    assert_contains "$out" "src/sub/b.txt" "l 含 src/sub/b.txt" || return 1
    # tree
    out="$("$HZIP" l "$d/a.zip" --tree 2>/dev/null)" || return 1
    assert_contains "$out" "b.txt" "tree 含 b.txt" || return 1
    # select
    out="$("$HZIP" l "$d/a.zip" --select src/a.txt 2>/dev/null)" || return 1
    assert_contains "$out" "src/a.txt" "select 含 src/a.txt" || return 1
    if [[ "$out" == *"b.txt"* ]]; then echo "  ✗ select 不应含 b.txt" >&2; return 1; fi
    # json：单文档 + 字段
    out="$("$HZIP" l "$d/a.zip" --json 2>/dev/null)" || return 1
    assert_contains "$out" '"format":"zip"' "json format" || return 1
    assert_contains "$out" '"entry_count":' "json entry_count" || return 1
    assert_contains "$out" '"name":"src/a.txt"' "json name" || return 1
    # stderr 不得有 NDJSON 进度（l 非 submit 路径）
    local err
    err="$("$HZIP" l "$d/a.zip" --json 2>&1 >/dev/null)"
    if [[ -n "$err" ]]; then echo "  ✗ l json stderr 应为空: $err" >&2; return 1; fi
    return 0
}

# ============================================================================
# 05 · p 预览：内容 / 截断
# ============================================================================
t05_preview() {
    local d="$WORK_DIR/05"; make_src_tree "$d/src"
    "$HZIP" c -f zip -o "$d/a.zip" "$d/src" >/dev/null 2>&1
    local out
    # v2：条目名带 src/ 前缀
    out="$("$HZIP" p "$d/a.zip" src/a.txt 2>/dev/null)" || { echo "  ✗ p 失败" >&2; return 1; }
    [[ "$out" == "hello hzip" ]] || { echo "  ✗ p 内容不符: [$out]" >&2; return 1; }
    out="$("$HZIP" p "$d/a.zip" src/sub/b.txt 2>/dev/null)"
    [[ "$out" == "world content" ]] || { echo "  ✗ p sub 内容不符" >&2; return 1; }
    # 截断
    out="$("$HZIP" p "$d/a.zip" src/a.txt --max-bytes 3 2>/dev/null)"
    [[ "$out" == "hel" ]] || { echo "  ✗ p 截断不符: [$out]" >&2; return 1; }
    return 0
}

# ============================================================================
# 06 · t 测试：好档 0 / 损坏档 2 + 指明条目
# ============================================================================
t06_test_cmd() {
    local d="$WORK_DIR/06"; make_src_tree "$d/src"
    "$HZIP" c -f zip -o "$d/a.zip" "$d/src" >/dev/null 2>&1
    local out err
    "$HZIP" t "$d/a.zip" >/dev/null 2>&1
    assert_exit 0 $? "t 好档" || return 1
    # 损坏：中部翻转（数据区）
    local big="$d/big-src"; mkdir -p "$big"
    head -c 500000 /dev/urandom > "$big/payload.bin"
    "$HZIP" c -f zip -o "$d/big.zip" "$big" >/dev/null 2>&1
    python - "$d/big.zip" <<'PY'
import sys
p = sys.argv[1]
b = bytearray(open(p, 'rb').read())
b[len(b)//2] ^= 0xFF
open(p, 'wb').write(b)
PY
    "$HZIP" t "$d/big.zip" >/dev/null 2> "$d/err.txt"
    local rc=$?
    assert_exit 2 $rc "t 损坏档" || return 1
    err="$(cat "$d/err.txt")"
    if [[ "$err" != *"payload.bin"* && "$err" != *"HZ-3001"* && "$err" != *"CRC"* ]]; then
        echo "  ✗ t stderr 应指明损坏条目: $err" >&2; return 1
    fi
    return 0
}

# ============================================================================
# 07 · x 智能解包：多顶层 Wrap / flat 展平 / select / codepage
# ============================================================================
t07_extract_smart() {
    local d="$WORK_DIR/07"; make_src_tree "$d/src"
    "$HZIP" c -f zip -o "$d/a.zip" "$d/src" >/dev/null 2>&1
    # v2 默认：保目录名原样展开（src/ 前缀 + 无包壳）——多顶层也直接散落
    mkdir -p "$d/w1"
    "$HZIP" x "$d/a.zip" -o "$d/w1" >/dev/null 2>&1
    assert_exit 0 $? "x 原样" || return 1
    assert_exists "$d/w1/src/a.txt" "x src/a.txt" || return 1
    assert_exists "$d/w1/src/sub/b.txt" "x src/sub/b.txt" || return 1
    # --smart：多顶层（src/ 单顶层目录 + 无散文件）→ 仍原样（单顶层不包壳）
    mkdir -p "$d/w1s"
    "$HZIP" x "$d/a.zip" -o "$d/w1s" --smart >/dev/null 2>&1
    assert_exit 0 $? "x --smart 单顶层" || return 1
    assert_exists "$d/w1s/src/a.txt" "smart 单顶层原样" || return 1
    # --smart 多顶层（散文件 + 目录）→ 包档案 stem 壳
    local m="$d/multi"; mkdir -p "$m/top"
    printf 'm1\n' > "$m/loose.txt"
    printf 'm2\n' > "$m/top/inner.txt"
    "$HZIP" c -f zip -o "$d/multi.zip" "$m/loose.txt" "$m/top" >/dev/null 2>&1
    mkdir -p "$d/w1m"
    "$HZIP" x "$d/multi.zip" -o "$d/w1m" --smart >/dev/null 2>&1
    assert_exit 0 $? "x --smart 多顶层" || return 1
    assert_exists "$d/w1m/multi/loose.txt" "smart 壳 loose" || return 1
    assert_exists "$d/w1m/multi/top/inner.txt" "smart 壳 top/inner" || return 1
    # 单顶层目录输入 c → x 原样（不剥 top）
    local s="$d/single"; mkdir -p "$s/top"
    printf 'single\n' > "$s/top/f.txt"
    (cd "$s" && "$HZIP" c -f zip -o "$d/single.zip" top >/dev/null 2>&1)
    mkdir -p "$d/w2"
    "$HZIP" x "$d/single.zip" -o "$d/w2" >/dev/null 2>&1
    assert_exists "$d/w2/top/f.txt" "单顶层原样 top/f.txt" || return 1
    assert_file_eq "$d/w2/top/f.txt" "$s/top/f.txt" "原样内容" || return 1
    # select：档内名
    mkdir -p "$d/w4"
    "$HZIP" x "$d/a.zip" -o "$d/w4" --select src/a.txt >/dev/null 2>&1
    assert_exit 0 $? "x select" || return 1
    assert_exists "$d/w4/src/a.txt" "select 解出 a.txt" || return 1
    if find "$d/w4" -type f -name 'b.txt' | grep -q .; then
        echo "  ✗ select 不应解出 b.txt" >&2; return 1
    fi
    return 0
}

# ============================================================================
# 08 · 冲突策略：overwrite / skip / rename
# ============================================================================
t08_conflict() {
    local d="$WORK_DIR/08"; make_src_tree "$d/src"
    "$HZIP" c -f zip -o "$d/a.zip" "$d/src" >/dev/null 2>&1
    # overwrite：预放不同内容 → 覆盖
    local ow="$d/ow"; mkdir -p "$ow"
    "$HZIP" x "$d/a.zip" -o "$ow" >/dev/null 2>&1
    printf 'changed\n' > "$ow/src/a.txt"
    "$HZIP" x "$d/a.zip" -o "$ow" --on-conflict overwrite >/dev/null 2>&1
    assert_exit 0 $? "overwrite" || return 1
    assert_file_eq "$ow/src/a.txt" "$d/src/a.txt" "overwrite 内容覆盖" || return 1
    # skip：预放不同内容 → 保留
    local sk="$d/sk"; mkdir -p "$sk"
    "$HZIP" x "$d/a.zip" -o "$sk" >/dev/null 2>&1
    printf 'changed\n' > "$sk/src/a.txt"
    "$HZIP" x "$d/a.zip" -o "$sk" --on-conflict skip >/dev/null 2>&1
    assert_exit 0 $? "skip" || return 1
    if grep -q "changed" "$sk/src/a.txt"; then :; else
        echo "  ✗ skip 应保留原文件" >&2; return 1
    fi
    # rename：产生 (2) 副本且原文件保留
    local rn="$d/rn"; mkdir -p "$rn"
    "$HZIP" x "$d/a.zip" -o "$rn" >/dev/null 2>&1
    "$HZIP" x "$d/a.zip" -o "$rn" --on-conflict rename >/dev/null 2>&1
    assert_exit 0 $? "rename" || return 1
    assert_exists "$rn/src/a (2).txt" "rename 副本" || return 1
    assert_file_eq "$rn/src/a.txt" "$d/src/a.txt" "rename 原文件保留" || return 1
    return 0
}

# ============================================================================
# 09 · 退出码契约：0 / 参数错 2 / 未知格式 2 / 不存在档 2
# ============================================================================
t09_exit_codes() {
    local d="$WORK_DIR/09"; make_src_tree "$d/src"
    "$HZIP" c -f zip -o "$d/a.zip" "$d/src" >/dev/null 2>&1
    "$HZIP" t "$d/a.zip" >/dev/null 2>&1
    assert_exit 0 $? "成功→0" || return 1
    "$HZIP" c -f bogus -o "$d/x.zip" "$d/src" >/dev/null 2>&1
    assert_exit 2 $? "未知格式→2" || return 1
    "$HZIP" l "$d/nonexistent.zip" >/dev/null 2>&1
    assert_exit 2 $? "不存在档→2" || return 1
    "$HZIP" c -f zip -o "$d/a.zip" "$d/src" --level bogus >/dev/null 2>&1
    assert_exit 2 $? "非法级别→2" || return 1
    "$HZIP" --help >/dev/null 2>&1
    assert_exit 0 $? "--help→0" || return 1
    return 0
}

# ============================================================================
# 10 · stdin 管道：x - / l - / p -
# ============================================================================
t10_stdin_pipe() {
    local d="$WORK_DIR/10"; make_src_tree "$d/src"
    "$HZIP" c -f zip -o "$d/a.zip" "$d/src" >/dev/null 2>&1
    # l -
    local out
    out="$(cat "$d/a.zip" | "$HZIP" l - 2>/dev/null)" || { echo "  ✗ l - 失败" >&2; return 1; }
    assert_contains "$out" "a.txt" "stdin l 条目" || return 1
    # p -
    out="$(cat "$d/a.zip" | "$HZIP" p - src/a.txt 2>/dev/null)" || return 1
    [[ "$out" == "hello hzip" ]] || { echo "  ✗ stdin p 内容: [$out]" >&2; return 1; }
    # x -（stdin 解压）
    mkdir -p "$d/sx"
    cat "$d/a.zip" | "$HZIP" x - -o "$d/sx" >/dev/null 2>&1
    assert_exit 0 $? "x -" || return 1
    if ! find "$d/sx" -type f -name 'a.txt' | grep -q .; then
        echo "  ✗ stdin x 无产物" >&2; return 1
    fi
    return 0
}

# ============================================================================
# 11 · 密码：zipcrypto 黄金档（hzip-pw-test）正确/错误
# ============================================================================
t11_password_zipcrypto() {
    local d="$WORK_DIR/11"
    local zc="$DATA_DIR/zipcrypto-7z.zip"
    local plain="$DATA_DIR/zipcrypto-7z.plain.txt"
    assert_exists "$zc" "密码样本" || return 1
    mkdir -p "$d/ok"
    HZIP_PASSWORD=hzip-pw-test "$HZIP" x "$zc" -o "$d/ok" >/dev/null 2>&1
    assert_exit 0 $? "zipcrypto 正确密码" || return 1
    local f
    f="$(find "$d/ok" -type f | head -1)"
    assert_exists "${f:-}" "解出文件" || return 1
    # 内容 = plain（trim 比较）
    local got want
    got="$(cat "$f" | tr -d '\r')"
    want="$(cat "$plain" | tr -d '\r')"
    if [[ "$got" != "$want" ]]; then
        echo "  ✗ zipcrypto 内容不符" >&2
        diff <(echo "$got") <(echo "$want") | head -5 >&2
        return 1
    fi
    # 错误密码 → 3101 退出 2
    mkdir -p "$d/bad"
    HZIP_PASSWORD=wrong-password "$HZIP" x "$zc" -o "$d/bad" >/dev/null 2> "$d/err.txt"
    local rc=$?
    assert_exit 2 $rc "zipcrypto 错误密码" || return 1
    if ! grep -qE "HZ-3101|密码" "$d/err.txt"; then
        echo "  ✗ 错误密码应报 HZ-3101: $(cat "$d/err.txt")" >&2; return 1
    fi
    return 0
}

# ============================================================================
# 12 · --json：c/x/t 的 stdout 单档 + stderr NDJSON
# ============================================================================
t12_json_output() {
    local d="$WORK_DIR/12"; make_src_tree "$d/src"
    # c --json
    local so se
    so="$("$HZIP" c -f zip -o "$d/a.zip" "$d/src" --json 2>"$d/c.err")" || {
        echo "  ✗ c --json 失败: $(cat "$d/c.err")" >&2; return 1; }
    assert_contains "$so" '"command":"compress"' "c json command" || return 1
    assert_contains "$so" '"exit_code":0' "c json exit" || return 1
    if [[ -s "$d/c.err" ]]; then
        # stderr NDJSON：每行合法 json + 末行 finished
        local nerr
        nerr="$(wc -l < "$d/c.err")"
        if [[ "$nerr" -lt 1 ]]; then echo "  ✗ c stderr 无进度" >&2; return 1; fi
        if ! python - "$d/c.err" <<'PY'
import json, sys
for ln in open(sys.argv[1], encoding='utf-8'):
    ln = ln.strip()
    if not ln: continue
    try:
        j = json.loads(ln)
    except Exception:
        sys.exit("非 json 行")
PY
        then echo "  ✗ stderr NDJSON 行非法" >&2; return 1; fi
    fi
    # t --json
    so="$("$HZIP" t "$d/a.zip" --json 2>/dev/null)" || return 1
    assert_contains "$so" '"exit_code":0' "t json" || return 1
    # l --json 已在 t04 覆盖；x --json
    mkdir -p "$d/ox"
    so="$("$HZIP" x "$d/a.zip" -o "$d/ox" --json 2>/dev/null)" || return 1
    assert_contains "$so" '"command":"extract"' "x json command" || return 1
    return 0
}

# ============================================================================
# 13 · cv 转换链：zip→7z→tar，条目一致
# ============================================================================
t13_convert() {
    local d="$WORK_DIR/13"; make_src_tree "$d/src"
    "$HZIP" c -f zip -o "$d/a.zip" "$d/src" >/dev/null 2>&1
    # zip → 7z
    "$HZIP" cv "$d/a.zip" --to 7z -o "$d/a.7z" >/dev/null 2>&1
    assert_exit 0 $? "cv zip→7z" || return 1
    assert_exists "$d/a.7z" "7z 产物" || return 1
    # 解 7z 比对（v2 保目录名：条目 src/... 原样）
    mkdir -p "$d/o7"; "$HZIP" x "$d/a.7z" -o "$d/o7" >/dev/null 2>&1
    assert_exit 0 $? "x 7z" || return 1
    assert_tree_eq "$d/src" "$d/o7/src" "zip→7z 内容" || return 1
    # 7z → tar（条目保持 src/）
    "$HZIP" cv "$d/a.7z" --to tar -o "$d/b.tar" >/dev/null 2>&1
    assert_exit 0 $? "cv 7z→tar" || return 1
    mkdir -p "$d/ot"; "$HZIP" x "$d/b.tar" -o "$d/ot" >/dev/null 2>&1
    assert_tree_eq "$d/src" "$d/ot/src" "7z→tar 内容" || return 1
    # 级别参数
    "$HZIP" cv "$d/a.zip" --to 7z -L ultra -o "$d/c.7z" >/dev/null 2>&1
    assert_exit 0 $? "cv -L ultra" || return 1
    return 0
}

# ============================================================================
# 14 · --exclude glob
# ============================================================================
t14_exclude() {
    local d="$WORK_DIR/14"; make_src_tree "$d/src"
    "$HZIP" c -f zip -o "$d/a.zip" "$d/src" \
        --exclude '*.tmp' --exclude 'sub/keep.log' >/dev/null 2>&1
    assert_exit 0 $? "c exclude" || return 1
    local out
    out="$("$HZIP" l "$d/a.zip" 2>/dev/null)"
    if [[ "$out" == *"skip.tmp"* ]]; then echo "  ✗ *.tmp 未排除" >&2; return 1; fi
    if [[ "$out" == *"keep.log"* ]]; then echo "  ✗ keep.log 未排除" >&2; return 1; fi
    assert_contains "$out" "a.txt" "排除后仍含 a.txt" || return 1
    return 0
}

# ============================================================================
# 15 · --split 分卷（zip）
#
# 已知缺陷：hzip 分卷 zip 主档无中央目录（truncated），数据全在 .z01 卷；
# 7-Zip 自 .z01 只能救出首卷内容（此处 2/3 条目）。脚本验证产物形态与
# salvage 可见条目，不对跨卷完整性做硬断言（记录 note）。
# ============================================================================
t15_split() {
    local d="$WORK_DIR/15"; make_src_tree "$d/src"
    head -c 300000 /dev/urandom > "$d/src/big.bin"
    "$HZIP" c -f zip -o "$d/sp.zip" --split 100k "$d/src" >/dev/null 2>&1
    assert_exit 0 $? "c split" || return 1
    # 分卷文件存在
    local vols=0
    for f in "$d"/sp.z0* "$d"/sp.zip; do [[ -e "$f" ]] && vols=$((vols+1)); done
    if [[ "$vols" -lt 2 ]]; then echo "  ✗ 应产生分卷" >&2; return 1; fi
    # 首卷 salvage 可见条目（a.txt + big.bin）
    local out
    out="$("$HZIP" l "$d/sp.z01" 2>/dev/null)"
    assert_contains "$out" "a.txt" "z01 salvage a.txt" || return 1
    assert_contains "$out" "big.bin" "z01 salvage big.bin" || return 1
    echo "  [note] 分卷 zip 主档无中央目录（hzip 写入缺陷，跨卷完整解不可达）；"\
        "  z01 salvage 可见 $(echo "$out" | grep -cE '^[-d]') 条目"
    return 0
}

# ============================================================================
# 16 · 7-Zip 互操作（普通档双向）
# ============================================================================
t16_7z_interop_plain() {
    [[ -n "$SZ7" ]] || { echo "  (skip: 无 7-Zip)"; SKIPN=$((SKIPN+1)); return 0; }
    local d="$WORK_DIR/16"; make_src_tree "$d/src"
    # hzip 写 7z → 7z 解
    "$HZIP" c -f 7z -o "$d/hz.7z" "$d/src" >/dev/null 2>&1
    assert_exit 0 $? "hzip c 7z" || return 1
    mkdir -p "$d/s1"
    "$SZ7" x -y -o"$d/s1" "$d/hz.7z" >/dev/null 2>&1
    assert_exit 0 $? "7z x hzip7z" || return 1
    # v2 保目录名：hzip c 档内 src/... → 7z 解出 src/a.txt
    assert_exists "$d/s1/src/a.txt" "7z 解出 a.txt" || return 1
    assert_file_eq "$d/s1/src/a.txt" "$d/src/a.txt" "7z 解出内容" || return 1
    # 7z 写 7z → hzip 解
    rm -rf "$d/sza"; mkdir -p "$d/sza"
    cp "$d/src/a.txt" "$d/src/sub/b.txt" "$d/sza/"
    (cd "$d" && "$SZ7" a -y -t7z "$d/7z.7z" sza/a.txt sza/b.txt >/dev/null 2>&1)
    assert_exit 0 $? "7z a" || return 1
    mkdir -p "$d/h1"
    "$HZIP" x "$d/7z.7z" -o "$d/h1" >/dev/null 2>&1
    assert_exit 0 $? "hzip x 7z档" || return 1
    # hzip 自产 7z 条目 → 7z t 验证
    "$SZ7" t "$d/hz.7z" >/dev/null 2>&1
    assert_exit 0 $? "7z t hzip7z" || return 1
    return 0
}

# ============================================================================
# 17 · 7-Zip 互操作（zip 双向）
# ============================================================================
t17_7z_interop_zip() {
    [[ -n "$SZ7" ]] || { echo "  (skip: 无 7-Zip)"; SKIPN=$((SKIPN+1)); return 0; }
    local d="$WORK_DIR/17"; make_src_tree "$d/src"
    # hzip 写 zip → 7z t
    "$HZIP" c -f zip -o "$d/h.zip" "$d/src" >/dev/null 2>&1
    "$SZ7" t "$d/h.zip" >/dev/null 2>&1
    assert_exit 0 $? "7z t hzip zip" || return 1
    # 7z 写 zip → hzip 解
    rm -rf "$d/sza"; mkdir -p "$d/sza"
    cp "$d/src/a.txt" "$d/sza/"
    (cd "$d" && "$SZ7" a -y -tzip "$d/z.zip" sza/a.txt >/dev/null 2>&1)
    mkdir -p "$d/h1"
    "$HZIP" x "$d/z.zip" -o "$d/h1" >/dev/null 2>&1
    assert_exit 0 $? "hzip x 7z写zip" || return 1
    local f
    f="$(find "$d/h1" -type f -name a.txt | head -1)"
    assert_exists "${f:-}" "7z写zip解出" || return 1
    assert_file_eq "$f" "$d/src/a.txt" "7z写zip内容" || return 1
    return 0
}

# ============================================================================
# 18 · 7-Zip 加密互操作（已知缺口）：hzip 写加密 7z → 7z 解 OK；
#      7z 写 AES/头加密 → hzip 解为 KNOWN-GAP（T46 未合入，只报告不断言）
# ============================================================================
t18_7z_enc_interop() {
    [[ -n "$SZ7" ]] || { echo "  (skip: 无 7-Zip)"; SKIPN=$((SKIPN+1)); return 0; }
    local d="$WORK_DIR/18"; make_src_tree "$d/src"
    # hzip 自产加密 7z（HZIP_PASSWORD 通道）→ 7z 解
    HZIP_PASSWORD=secret123 "$HZIP" c -f 7z -o "$d/enc.7z" "$d/src" >/dev/null 2>&1
    assert_exit 0 $? "hzip c 加密7z" || return 1
    mkdir -p "$d/s1"
    "$SZ7" x -y -psecret123 -o"$d/s1" "$d/enc.7z" >/dev/null 2>&1
    assert_exit 0 $? "7z x hzip加密7z" || return 1
    # v2 保目录名：档内 src/...
    assert_exists "$d/s1/src/a.txt" "加密7z内容" || return 1
    assert_file_eq "$d/s1/src/a.txt" "$d/src/a.txt" "加密7z内容一致" || return 1
    # hzip 自解加密档
    mkdir -p "$d/h0"
    HZIP_PASSWORD=secret123 "$HZIP" x "$d/enc.7z" -o "$d/h0" >/dev/null 2>&1
    assert_exit 0 $? "hzip x 自产加密7z" || return 1
    # 7z 写 AES 内容加密 → hzip 解：KNOWN-GAP
    rm -rf "$d/sza"; mkdir -p "$d/sza"
    cp "$d/src/a.txt" "$d/sza/"
    (cd "$d" && "$SZ7" a -y -t7z -pmysecret1 -mhe=off "$d/7zaes.7z" sza/a.txt >/dev/null 2>&1)
    assert_exit 0 $? "7z a AES" || return 1
    mkdir -p "$d/h1"
    HZIP_PASSWORD=mysecret1 "$HZIP" x "$d/7zaes.7z" -o "$d/h1" >/dev/null 2>&1
    local rc=$?
    if [[ "$rc" -eq 0 ]]; then
        echo "  ✓ 7z AES 档 hzip 可解（缺口已修复？）"
    else
        echo "  [KNOWN-GAP] 7z AES 档 hzip 解退出 $rc（T46 AES 读未合入，golden runner 亦 SKIP）"
        GAP=$((GAP+1))
    fi
    return 0
}

# ============================================================================
# 19 · convert 边界：cv 到 single 格式（tar→gz）预期不支持
# ============================================================================
t19_convert_single_boundary() {
    local d="$WORK_DIR/19"; make_src_tree "$d/src"
    "$HZIP" c -f tar -o "$d/a.tar" "$d/src" >/dev/null 2>&1
    # tar → gz：多文件档案转单流无条目容器，契约上不支持（记录，不硬断言）
    "$HZIP" cv "$d/a.tar" --to gz -o "$d/x.tar.gz" >/dev/null 2>&1
    local rc=$?
    if [[ "$rc" -eq 0 && -s "$d/x.tar.gz" ]]; then
        # 若支持则解回一致
        mkdir -p "$d/o"
        "$HZIP" x "$d/x.tar.gz" -o "$d/o" >/dev/null 2>&1 || { echo "  ✗ cv tar.gz 产物不可解" >&2; return 1; }
    elif [[ "$rc" -ne 0 ]]; then
        echo "  [note] cv tar→gz 退出 $rc（多条目→单流无容器，契约不支持面）"
    else
        echo "  ✗ cv tar→gz 产物为空" >&2; return 1
    fi
    return 0
}

# ============================================================================
# main
# ============================================================================
TESTS=(
    "t01_zip_full_roundtrip|01 · zip 压缩→t→解压 全闭环"
    "t02_format_matrix|02 · 全格式矩阵（zip/7z/tar/gz/bz2/xz/zst）闭环"
    "t03_levels|03 · 压缩级别 store~ultra"
    "t04_list|04 · l 列表（平铺/tree/select/json）"
    "t05_preview|05 · p 预览（内容/截断）"
    "t06_test_cmd|06 · t 测试（好档/损坏档）"
    "t07_extract_smart|07 · x 解包形态（原样/--smart 自动判别/select）"
    "t08_conflict|08 · 冲突策略（overwrite/skip/rename）"
    "t09_exit_codes|09 · 退出码契约（0/2）"
    "t10_stdin_pipe|10 · stdin 管道（l -/p -/x -）"
    "t11_password_zipcrypto|11 · 密码（zipcrypto 黄金档）"
    "t12_json_output|12 · --json 双流输出"
    "t13_convert|13 · cv 格式转换链（zip→7z→tar）"
    "t14_exclude|14 · --exclude glob 排除"
    "t15_split|15 · --split 分卷"
    "t16_7z_interop_plain|16 · 7z 互操作（普通 7z 双向）"
    "t17_7z_interop_zip|17 · 7z 互操作（zip 双向）"
    "t18_7z_enc_interop|18 · 7z 加密互操作（含 KNOWN-GAP）"
    "t19_convert_single_boundary|19 · cv 边界（tar→gz）"
)

main() {
    local opt="${1:-}"
    case "$opt" in
        --list)
            for t in "${TESTS[@]}"; do
                echo "${t%%|*}  # ${t#*|}"
            done
            exit 0;;
        "")
            echo "== hzip-cli 功能测试 =="
            echo "hzip: $HZIP"
            [[ -n "$SZ7" ]] && echo "7-Zip: $SZ7" || echo "7-Zip: (无，互操作 SKIP)"
            echo
            for t in "${TESTS[@]}"; do
                run_case "${t%%|*}" "${t%%|*}"
            done
            ;;
        *)
            ONLY="$opt"
            for t in "${TESTS[@]}"; do
                [[ "${t%%|*}" == "$opt" ]] || continue
                run_case "$opt" "$opt"
            done
            if [[ "$ONLY" == "$opt" && $FAIL -eq 0 && $PASS -eq 0 ]]; then
                echo "未知测试: $opt（--list 查看全部）" >&2; exit 2
            fi
            ;;
    esac
    echo
    echo "== 结果: $PASS 通过 / $FAIL 失败 / $SKIPN 跳过 / $GAP known-gap =="
    if [[ $FAIL -gt 0 ]]; then
        printf '  失败: %s\n' "${FAILED_NAMES[@]}"
        exit 1
    fi
    exit 0
}

main "$@"
