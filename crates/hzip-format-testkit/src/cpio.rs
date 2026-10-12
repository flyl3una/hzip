//! cpio **测试夹具构造器**（四变体字节流；02 §8.7）。
//!
//! hzip 的 cpio 支持是**只读**的（[`hzip-format::FormatCaps::write`] = false），
//! 没有可用的写侧来生成测试素材；而 cpio 头是自描述定长 ASCII/字序列，直接
//! 按 GNU cpio 口径拼字节比引入第三方实现更可控、更可读。本模块就是这份
//! 「最小写侧」——**仅供测试夹具**，不进任何 handler，也不做合法性校验
//! （要造坏档的用例正是靠它「不校验」）。
//!
//! # 变体与字段布局（GNU cpio 口径）
//!
//! | 变体 | 字节 | 字段 |
//! |---|---|---|
//! | [`CpioVariant::Newc`] / [`CpioVariant::Crc`] | 110 | 6 魔数 + 13×8 十六进制（`crc` 的末域 `check` = 数据字节和） |
//! | [`CpioVariant::Odc`] | 76 | 6 魔数 + 7×6 + 11 + 6 + 11 八进制（无对齐） |
//! | [`CpioVariant::BinLe`] / [`CpioVariant::BinBe`] | 26 | 13 个 16 位字（`mtime`/`filesize` 各占高、低两字） |
//!
//! 名字**含尾 NUL**（`namesize` 计入该 NUL）；newc/crc 的「头+名字」与「数据」
//! 各按 4 字节对齐（偏移自头起点计），bin 按 2 字节，odc 不填充。尾部零填充块
//! 由 [`build_blocked`] 追加（GNU cpio `-B` / bsdtar 的 512 块对齐形态）。

/// 夹具变体（与 `hzip-fmt-cpio::variant::CpioVariant` 同口径的**独立**枚举：
/// testkit 不依赖任何 `hzip-fmt-*`，02 §2.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpioVariant {
    /// SVR4 newc（`070701`）。
    Newc,
    /// SVR4 crc（`070702`；写 `check` = 数据字节和）。
    Crc,
    /// 旧可移植 ASCII（`070707`）。
    Odc,
    /// 旧二进制，小端字序（`c7 71`）。
    BinLe,
    /// 旧二进制，大端字序（`71 c7`）。
    BinBe,
}

impl CpioVariant {
    /// 头字节数。
    pub fn header_bytes(self) -> usize {
        match self {
            Self::Newc | Self::Crc => 110,
            Self::Odc => 76,
            Self::BinLe | Self::BinBe => 26,
        }
    }

    /// 对齐粒度。
    pub fn align(self) -> usize {
        match self {
            Self::Newc | Self::Crc => 4,
            Self::BinLe | Self::BinBe => 2,
            Self::Odc => 1,
        }
    }

    /// 16 位字是否小端（仅二进制变体有意义）。
    fn little(self) -> bool {
        !matches!(self, Self::BinBe)
    }
}

/// 一个待写条目。
#[derive(Debug, Clone)]
pub struct EntrySpec {
    /// 条目名（原样写入，含 `./`、尾 `/` 等由调用方决定）。
    pub name: String,
    /// 数据区字节（目录应为空；符号链接为目标路径字节）。
    pub data: Vec<u8>,
    /// 完整 `st_mode`（含文件类型位：`0o100644` / `0o040755` / `0o120777`）。
    pub mode: u32,
    /// unix 秒。
    pub mtime: u64,
    /// inode（newc/crc/odc/bin 都写；硬链接组用例靠它对齐）。
    pub ino: u32,
    /// 硬链接数（默认 1）。
    pub nlink: u32,
}

impl EntrySpec {
    /// 普通文件（`0o100644`）。
    pub fn file(name: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            data: data.into(),
            mode: 0o100644,
            mtime: 0,
            ino: 1,
            nlink: 1,
        }
    }

    /// 目录（`0o040755`；名尾是否带 `/` 由调用方给 `name`）。
    pub fn dir(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            data: Vec::new(),
            mode: 0o040755,
            mtime: 0,
            ino: 1,
            nlink: 1,
        }
    }

    /// 符号链接（`0o120777`；目标写在数据区）。
    pub fn symlink(name: impl Into<String>, target: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            data: target.into(),
            mode: 0o120777,
            mtime: 0,
            ino: 1,
            nlink: 1,
        }
    }

    /// 覆盖 `st_mode`（造特殊权限/只读位用例）。
    pub fn with_mode(mut self, mode: u32) -> Self {
        self.mode = mode;
        self
    }

    /// 覆盖 mtime。
    pub fn with_mtime(mut self, mtime: u64) -> Self {
        self.mtime = mtime;
        self
    }

    /// 覆盖 inode / 硬链接数（硬链接组用例）。
    pub fn with_link(mut self, ino: u32, nlink: u32) -> Self {
        self.ino = ino;
        self.nlink = nlink;
        self
    }
}

/// 归档尾条目的名字（`namesize` = 该串长 + 1）。
pub const TRAILER_NAME: &str = "TRAILER!!!";

/// 拼一个完整归档（条目 + `TRAILER!!!`，无尾部块填充）。
pub fn build(variant: CpioVariant, entries: &[EntrySpec]) -> Vec<u8> {
    let mut v = build_without_trailer(variant, entries);
    v.extend_from_slice(&record(variant, TRAILER_NAME, &[], 0, 0, 1, 1));
    v
}

/// 拼一个**不带尾条目**的归档（缺尾容错用例）。
pub fn build_without_trailer(variant: CpioVariant, entries: &[EntrySpec]) -> Vec<u8> {
    let mut v = Vec::new();
    for e in entries {
        v.extend_from_slice(&record(
            variant, &e.name, &e.data, e.mode, e.mtime, e.ino, e.nlink,
        ));
    }
    v
}

/// 同 [`build`]，但整体补零到 512 字节块边界（GNU cpio `-B` / bsdtar 形态）。
pub fn build_blocked(variant: CpioVariant, entries: &[EntrySpec]) -> Vec<u8> {
    let mut v = build(variant, entries);
    let pad = (512 - v.len() % 512) % 512;
    v.resize(v.len() + pad, 0);
    v
}

/// 只写头（不写名字/数据）——「记录内部截断」用例。
pub fn header_only(variant: CpioVariant, name: &str, data_len: usize) -> Vec<u8> {
    header(
        variant,
        name,
        data_len,
        Hdr {
            mode: 0o100644,
            ino: 1,
            nlink: 1,
            ..Default::default()
        },
        0,
    )
}

/// 写头 + 名字，**不写数据区**——「数据区被截断」用例。
pub fn header_and_name(variant: CpioVariant, name: &str, data_len: usize) -> Vec<u8> {
    let mut v = header(
        variant,
        name,
        data_len,
        Hdr {
            mode: 0o100644,
            ino: 1,
            nlink: 1,
            ..Default::default()
        },
        0,
    );
    write_name(&mut v, variant, name);
    v
}

/// 头里除名字/数据长度外的字段（聚成结构，免去 8 参函数）。
#[derive(Debug, Clone, Copy, Default)]
struct Hdr {
    mode: u32,
    mtime: u64,
    ino: u32,
    nlink: u32,
}

/// 单条记录（头 + 名字 + 对齐 + 数据 + 对齐）。
fn record(
    variant: CpioVariant,
    name: &str,
    data: &[u8],
    mode: u32,
    mtime: u64,
    ino: u32,
    nlink: u32,
) -> Vec<u8> {
    // crc 变体：`check` = 数据字节和（GNU cpio 口径；实测 `hello` → 532）
    let check = if variant == CpioVariant::Crc {
        data.iter()
            .map(|&b| u32::from(b))
            .fold(0u32, u32::wrapping_add)
    } else {
        0
    };
    let hdr = Hdr {
        mode,
        mtime,
        ino,
        nlink,
    };
    let mut v = header(variant, name, data.len(), hdr, check);
    write_name(&mut v, variant, name);
    v.extend_from_slice(data);
    let pad = data_pad(variant, data.len());
    v.resize(v.len() + pad, 0);
    v
}

/// 头块（含 newc/crc 的 `check` 域：数据字节和）。
fn header(variant: CpioVariant, name: &str, data_len: usize, hdr: Hdr, check: u32) -> Vec<u8> {
    let Hdr {
        mode,
        mtime,
        ino,
        nlink,
    } = hdr;
    let namesize = name.len() as u64 + 1; // 含尾 NUL
    match variant {
        CpioVariant::Newc | CpioVariant::Crc => {
            let magic: &[u8] = if variant == CpioVariant::Crc {
                b"070702"
            } else {
                b"070701"
            };
            let mut v = magic.to_vec();
            // ino, mode, uid, gid, nlink, mtime, filesize, devmajor, devminor,
            // rdevmajor, rdevminor, namesize, check
            for f in [
                ino as u64,
                mode as u64,
                0,
                0,
                nlink as u64,
                mtime,
                data_len as u64,
                0,
                0,
                0,
                0,
                namesize,
                u64::from(check),
            ] {
                v.extend_from_slice(format!("{f:08x}").as_bytes());
            }
            v
        }
        CpioVariant::Odc => {
            let mut v = b"070707".to_vec();
            let oct = |v: &mut Vec<u8>, val: u64, width: usize| {
                v.extend_from_slice(format!("{val:0width$o}").as_bytes());
            };
            oct(&mut v, 0o777777, 6); // dev
            oct(&mut v, ino as u64, 6);
            oct(&mut v, mode as u64, 6);
            oct(&mut v, 0, 6); // uid
            oct(&mut v, 0, 6); // gid
            oct(&mut v, nlink as u64, 6);
            oct(&mut v, 0, 6); // rdev
            oct(&mut v, mtime, 11);
            oct(&mut v, namesize, 6);
            oct(&mut v, data_len as u64, 11);
            v
        }
        CpioVariant::BinLe | CpioVariant::BinBe => {
            let le = variant.little();
            let mut v = Vec::with_capacity(26);
            let mut push = |w: u16| {
                if le {
                    v.extend_from_slice(&w.to_le_bytes());
                } else {
                    v.extend_from_slice(&w.to_be_bytes());
                }
            };
            push(0o070707);
            push(0); // dev（bin 的 dev 只有一个 16 位字：0o777777 放不下，写 0）
            push(ino as u16);
            push(mode as u16);
            push(0); // uid
            push(0); // gid
            push(nlink as u16);
            push(0); // rdev
            push((mtime >> 16) as u16);
            push((mtime & 0xffff) as u16);
            push(namesize as u16);
            push((data_len as u64 >> 16) as u16);
            push((data_len as u64 & 0xffff) as u16);
            v
        }
    }
}

/// 名字（含尾 NUL）+ newc/crc 的名字区对齐填充。
fn write_name(v: &mut Vec<u8>, variant: CpioVariant, name: &str) {
    v.extend_from_slice(name.as_bytes());
    v.push(0);
    let pad = name_pad(variant, name.len() + 1);
    v.resize(v.len() + pad, 0);
}

/// 名字区填充：**偏移自头起点**对齐（`110 + namesize` / `26 + namesize`）。
///
/// GNU cpio 实测口径（2.14，`newc`：`nsize=5` → 名区补 1；`bin`：`26+5=31` → 补 1）。
fn name_pad(variant: CpioVariant, namesize: usize) -> usize {
    let a = variant.align();
    if a <= 1 {
        return 0;
    }
    let used = (variant.header_bytes() + namesize) as u64;
    let rem = (used % a as u64) as usize;
    if rem == 0 { 0 } else { a - rem }
}

/// 数据区填充：**偏移自数据起点**对齐（`filesize` 单独对齐）。
///
/// 与名字区不同——GNU cpio 2.14 实测：`newc` 的 `link`（名区尾 228、`fsize=5`）
/// 补 3 字节后下一条头落在 236，即 `228 + align4(5) = 236`；若按「自头起点对齐」
/// 会得到 233，与实测不符。故**名字区与数据区各按自己的起点对齐**。
fn data_pad(variant: CpioVariant, len: usize) -> usize {
    let a = variant.align();
    if a <= 1 {
        return 0;
    }
    let rem = len % a;
    if rem == 0 { 0 } else { a - rem }
}

#[cfg(test)]
#[path = "tests/cpio.rs"]
mod tests;
