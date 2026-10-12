# hzip-cli 功能测试脚本

对 hzip-cli（`target/debug/hzip.exe`）做全命令功能测试：压缩与解压配套闭环，
覆盖 7-Zip 双向互操作。每测试点一个独立函数，由 main 顺序调度。

## 文件

| 文件 | 说明 |
|---|---|
| `run-cli-tests.sh` | Git Bash / MSYS bash 版 |
| `run-cli-tests.ps1` | Windows PowerShell 5.1+ 版（UTF-16 编码，原生跑） |
| `data/` | 预置测试数据（密码样本，源自 `crates/hzip-fmt-zip/tests/fixtures/`，勿删） |

临时产物在 `data/_work/` 自建自清（`-Keep`/`KEEP=1` 保留调试）。

## 路径语义（v2）

- **压缩 c**：目录输入**保留目录名**（`c src` → 档内 `src/` + `src/...`，含 src
  目录条目自身；与 7-Zip/标准 zip 一致）。空目录、软链接、元数据（权限/mtime）
  均收编。
- **解压 x（默认）**：档内条目**原样**落到 `-o` 目标（不存在自动创建；不指定则
  档案所在目录）——**不剥任何顶层目录、不额外包壳**。
- **`x --smart`（优雅解压）**：档内**多顶层/散文件**（会污染目标根）→ 自动以
  压缩包名（剥后缀 stem）建目录再展开（`x.zip` 内 `a/`+`b.txt` → `x/a/...`）；
  **恰 1 个目录或 1 个文件** → 原样直展（不包不剥）。
- `--flat` 已移除（默认即原样直展，无壳可展平）。

## 用法

```bash
# Git Bash
./run-cli-tests.sh            # 全部
./run-cli-tests.sh t05_preview   # 单测
./run-cli-tests.sh --list     # 列全部
HZIP=/path/to/hzip.exe SKIP_SZ=1 ./run-cli-tests.sh   # 指定 hzip / 跳过 7z

# PowerShell
powershell -ExecutionPolicy Bypass -File run-cli-tests.ps1
powershell -ExecutionPolicy Bypass -File run-cli-tests.ps1 t05-Preview
powershell -ExecutionPolicy Bypass -File run-cli-tests.ps1 -List
```

退出码：0 = 全过（known-gap/skip 不计失败）；1 = 有失败；2 = 环境错误。
先 `cargo build -p hzip-cli` 再跑。7-Zip 默认 `F:\tools\7zip\7z.exe`（可 `SZ7`/`-SzPath` 覆盖）。

## 覆盖矩阵（19 测试点）

| # | 函数 | 覆盖 |
|---|---|---|
| 01 | zip 全闭环 | `c -f zip` → `t` → `x`，目录树内容比对 |
| 02 | 格式矩阵 | zip/7z/tar/gz/bz2/xz/zst 7 格式 c+t+x 闭环（gz/bz2/xz/zst 为单流） |
| 03 | 压缩级别 | store~ultra 6 级全部可 t；ultra < store 体积 |
| 04 | 列表 | `l` 平铺 / `--tree` / `--select` / `--json`（schema 字段） |
| 05 | 预览 | `p` 条目内容 / `--max-bytes` 截断 |
| 06 | 测试 | `t` 好档 0；中部翻转损坏档 2 + stderr 指明条目 |
| 07 | 解包形态 | 默认原样保目录名（src/… 直落 -o）；`--smart` 多顶层自动包档案 stem 壳 / 单顶层不包；`--select` |
| 08 | 冲突 | `--on-conflict overwrite/skip/rename`（` (2)` 副本） |
| 09 | 退出码 | 成功 0 / 未知格式 2 / 不存在档 2 / 非法级别 2 |
| 10 | stdin | `x -`/`l -`/`p -` 管道（stdin 魔数嗅探） |
| 11 | 密码 | zipcrypto 黄金档正确密码解出 sha 一致；错密码 HZ-3101 退出 2 |
| 12 | JSON | `c/x/t --json` stdout 单档（exit_code 字段）+ stderr NDJSON 逐行合法 |
| 13 | 转换 | `cv` zip→7z→tar 链条目集与内容一致 |
| 14 | 排除 | `--exclude '*.tmp'` 段匹配 + `sub/keep.log` |
| 15 | 分卷 | `--split 100k` 产 .z0N 卷；z01 salvage 可列（见缺陷注） |
| 16 | 7z 互操作 | hzip 写 7z ↔ 7-Zip 解/测；7-Zip 写 7z → hzip 解 |
| 17 | zip 互操作 | hzip 写 zip → 7z t；7-Zip 写 zip → hzip 解 sha 一致 |
| 18 | 7z 加密互操作 | hzip 自产加密 7z → 7z 解 + hzip 自解；7z AES 档→hzip = KNOWN-GAP |
| 19 | cv 边界 | tar→gz 多条目转单流契约不支持（note） |

## 已知缺口（KNOWN-GAP / note，非脚本失败）

- **7z AES/头加密读（7-Zip 产）→ hzip 解失败**（T46 fmt-7z AES 读未合入，
  tests-assets runner golden 同 SKIP）。t18 只报告状态不断言；修复后自动转 ✓。
- **分卷 zip 主档无中央目录**（hzip 写入缺陷）：跨卷完整解不可达，7-Zip 仅能救
  首卷。t15 验证产物形态 + z01 salvage 可见条目。
- **cv tar→gz**：多条目档案转单流无容器，契约不支持面（退出 2）。

## 密码样本来源

`data/zipcrypto-7z.zip`（密码 `hzip-pw-test`）与明文拷贝自
`crates/hzip-fmt-zip/tests/fixtures/`，只读使用。
