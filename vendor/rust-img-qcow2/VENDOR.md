# vendor/rust-img-qcow2 —— 为什么在这里

## 一句话

上游 `rust-img-qcow2 0.6.1` 把 `ruzstd` 锁在 `^0.8`，而 `ruzstd 0.8.3` 带着
**RUSTSEC-2026-0344**（解压炸弹：<128 KB 的 `.zst` 输入可让解码器一次分配数 GB
⇒ 进程 OOM）。修好的是 `ruzstd >= 0.9.1`；上游 crate 到 2026-10-17 仍是 0.6.1、
无新版本、也无 `ruzstd` 放开版本的分支。qcow2 的 zstd 簇是**用户输入**，不能靠
ignore 混过去 ⇒ 把 `rust-img-qcow2` 就地 vendor，只改那一行版本约束。

## 与上游的差异（必须保持"一行"）

| 项 | 上游 0.6.1（crates.io） | 本目录 |
|---|---|---|
| `[dependencies] ruzstd` | `"0.8"` | `"0.9"` |
| `cli`/`clap`/bin/example/test 目标 | 有 | 删（不参与 `img_qcow2` 库目标） |
| `[lib] crate-type` | `["staticlib","rlib"]` | 默认（`rlib`；hzip 只用 Rust API） |
| `[profile.*]` | 有 | 删（依赖包的 profile 本就不生效） |
| `qemu-validation` feature | 有 | 删（只被上游测试用） |

`src/*.rs` **逐字节等于上游**（`lib.rs` / `capi.rs` / `error.rs` / `header.rs` /
`reader.rs`）。`ruzstd 0.9` 的 API 与 0.8 在本 crate 用到的那一处
（`ruzstd::decoding::StreamingDecoder::new`）相同，所以没有任何源码改动。

## 怎么摘掉

上游发布带 `ruzstd >= 0.9.1` 的版本（0.6.2 或更高）后：

1. 把根 `Cargo.toml` 的 `[patch.crates-io] rust-img-qcow2 = { path = ... }` 删掉；
2. `cargo update -p rust-img-qcow2`（同时确认 `cargo tree -i ruzstd` 只剩 0.9+）；
3. 删除本目录；
4. `cargo deny check licenses advisories` 复核 RUSTSEC-2026-0344 不再出现。

## 复现这次 vendor

```sh
cargo vendor --versioned-dirs  # 不需要：本目录只取上游 .crate 解包后的 src/
# 上游源：registry/src/index.crates.io-*/rust-img-qcow2-0.6.1/
```

`Cargo.toml` 按上表手工裁剪；`LICENSE` 原样拷贝（MIT）。
