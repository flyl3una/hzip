//! WinZip AES：extra field 0x9901 解析与 PBKDF2 KV 拆包（02 §8.1 `aes` 行；T45）。
//!
//! # 数据布局（02 §3.1 / WinZip AES 规范 AE-1/AE-2）
//!
//! | strength | key_len | salt_len | dkLen | V(HMAC key) | verifier 误收 |
//! |---|---|---|---|---|---|
//! | 1 → AES-128 | 16 | 8 | 26 | 8 | 1/65536 |
//! | 2 → AES-192 | 24 | 12 | 38 | 12 | 1/65536 |
//! | 3 → AES-256 | 32 | 16 | 50 | 16 | 1/65536 |
//!
//! 条目数据区：`salt ‖ PasswordVerifier(2) ‖ AES-CTR 密文 ‖ HMAC-SHA1-80(10)`。
//! KV 拆包：`PBKDF2-HMAC-SHA1(pw, salt, 1000, dkLen)` → K（AES 密钥 `key_len`）‖
//! V（HMAC 密钥 `key_len/2`）‖ PasswordVerifier（2）。
//!
//! # 双验证次序（红线 3，02 §7.3「实现层做 HMAC/校验头预验」）
//!
//! 读路径经 zip crate `aes-crypto`（feature 并集常开）的 [`zip::read`] 链路完成：
//! 第一关卡 = `AesReader::validate` 的 2 字节 PasswordVerifier 预验（错 →
//! `ZipError::InvalidPassword` → 本 crate 映射 `Password 3101`，流尚未交付）；
//! 第二关卡 = `AesReaderValid` EOF 处 HMAC-SHA1-80 比对（InvalidData
//! "Invalid authentication code…" → 本 crate [`crate::reader`] 映射 3101）。
//! AE-2 条目中央目录 CRC 恒 0 且 zip 侧跳过 CRC 校验 → HMAC 即完整性终验。
//! 截断（尾 10 字节不足）→ UnexpectedEof → `Format 2002`（Truncated）。
//! 错误密码必被两关卡之一拦截 → 零乱码流交付。
//!
//! # 写路径路线决策（§4.5 路线 A；登记依据 zip 4.6.1，9.0.4 复核同）
//!
//! 写方向采用 zip crate `aes-crypto` 的 `SimpleFileOptions::with_aes_encryption`
//! （加密链在 `start_file` 时建 `AesWriter`，salt 随机、PBKDF2 派生、HMAC 尾自动）。
//! 三档 key 长度经 `zip::AesMode::{Aes128,Aes192,Aes256}` 可控（TC-X-004 KV 兼容）。
//! **AE-x 选择偏差（如实登记）**：zip `write.rs finish_file` 按
//! `stats.bytes_written < 20` 自动 AE-2 / AE-1 切换（9.0.4 = `update_aes_extra_field`
//! 的 `write.rs:2197`，规则与 4.6.1 逐字相同）（WinZip 官方建议：小文件 AE-2
//! 防 CRC 泄露，大文件 AE-1 增 CRC 校验）——**无法强制全量 AE-2**。本模块 rustdoc
//! 结论：AE-2 写默认在小文件（<20 B）成立（中央目录 CRC=0，单测断言）；大文件为
//! AE-1（CRC 保留真实值，7-Zip/zip 读侧均兼容）。任务卡「写默认 AE-2」全量语义
//! 挂跟踪表（如后续需要全量 AE-2，可退 §4.5 自研路线——本模块已备 parse/KV 原语）。
//!
//! 本模块（feature `aes` 门控，02 §2.2）收敛职责：extra field 语义解析、KV 拆包
//! 原语、caps 升级辅助；加解密流与写加密链由 zip crate 承载（互操作以 zip 自身
//! 黄金向量 + 7-Zip 样本测试裁决）。
//!
//! # 密码承载（02 §1.2）
//!
//! 密码一律 `PasswordRef` 借用或 `Zeroizing` 包裹；含 NUL 明确
//! `Err Password 3101`（拒绝、不静默截断、不 panic，任务卡 §6.2）。

// 读/写运行时经 zip crate aes-crypto（route A，见模块文档「写路径路线决策」）；
// 下列 spec 原语是 02 §8.1/任务卡 §4.2 冻结交付面（KV 拆包/派生/extra 解析），
// 当前 crate 内运行时消费者为 zip crate 内部链路——对 lib 非测试构建不可达即
// dead_code。它们经单测锁定协议正确性（§6.1 aes_key_derivation_kv 等），并供
// 后续互操作测试/自研写路径引用；不删除（任务卡「禁静默砍」）。
// （模块经 lib.rs `#[cfg(feature = "aes")]` 门控——不在本文件重复 cfg。）
#![allow(dead_code)]

use hmac::Hmac;
use hzip_error::{FormatErrorKind, HzipError, PasswordTarget};
use sha1::Sha1;
use zeroize::Zeroizing;

use crate::handler;

/// PBKDF2 迭代次数（WinZip 规范固定 1000）。
const PBKDF2_ITERATIONS: u32 = 1000;
/// 0x9901 extra field 数据区固定长度（7 字节）。
const AE_EXTRA_LEN: usize = 7;
/// extra field 链头（id 2 + len 2）长度。
const EXTRA_HDR: usize = 4;
/// 0x9901 头 id。
const EXTRA_ID_9901: u16 = 0x9901;
/// vendor "AE" 小端 u16（0x41 0x45）。
const VENDOR_AE: u16 = 0x4541;

/// AE 版本（extra field 前 2 字节；02 §3.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AesVersion {
    /// AE-1：中央目录 CRC 保留真实值（读兼容；zip 写 ≥20 B 时自动采用）。
    Aes1,
    /// AE-2：CRC 置 0，HMAC 即完整性终验（zip 写 <20 B 时自动采用）。
    Aes2,
}

/// AES 强度（extra field 第 5 字节；1/2/3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AesStrength {
    Aes128,
    Aes192,
    Aes256,
}

impl AesStrength {
    /// 1/2/3 → 变体；其余 None（调用方 → 3103）。
    pub(crate) fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(Self::Aes128),
            2 => Some(Self::Aes192),
            3 => Some(Self::Aes256),
            _ => None,
        }
    }

    /// 密钥长度 16/24/32。
    pub(crate) const fn key_len(&self) -> usize {
        match self {
            Self::Aes128 => 16,
            Self::Aes192 => 24,
            Self::Aes256 => 32,
        }
    }

    /// salt 长度 = key_len/2 → 8/12/16。
    pub(crate) const fn salt_len(&self) -> usize {
        self.key_len() / 2
    }

    /// 写方向：按 key 长度（128/192/256）请求；其它 None。
    pub(crate) fn from_key_len(n: usize) -> Option<Self> {
        match n {
            16 => Some(Self::Aes128),
            24 => Some(Self::Aes192),
            32 => Some(Self::Aes256),
            _ => None,
        }
    }

    /// zip crate AesMode 互转（写路径用）。
    pub(crate) fn to_zip_mode(self) -> zip::AesMode {
        match self {
            Self::Aes128 => zip::AesMode::Aes128,
            Self::Aes192 => zip::AesMode::Aes192,
            Self::Aes256 => zip::AesMode::Aes256,
        }
    }

    /// 1/2/3 字节。
    pub(crate) fn strength_byte(self) -> u8 {
        match self {
            Self::Aes128 => 1,
            Self::Aes192 => 2,
            Self::Aes256 => 3,
        }
    }
}

/// extra field 0x9901 解析结果（02 §3.1 表）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct AesEntryInfo {
    pub(crate) version: AesVersion,
    pub(crate) strength: AesStrength,
    pub(crate) real_method: u16,
}

/// 解析 extra field 0x9901。
///
/// 接受两种形态：7 字节数据区本身，或完整 extra field 链（`id(2)‖len(2)‖data…`
/// 顺序扫描 0x9901）。vendor 非 "AE" / version 未知 / strength 非 1..3 / 数据区
/// 长度 ≠7 → `Format 3103`（UnknownCrypto，02 附录 A HZ-3103）。
pub(crate) fn parse_extra_field(data: &[u8]) -> Result<AesEntryInfo, HzipError> {
    let body = locate_9901_body(data).ok_or_else(fmt_3103)?;
    if body.len() != AE_EXTRA_LEN {
        return Err(fmt_3103());
    }
    let version_raw = u16::from_le_bytes([body[0], body[1]]);
    let vendor = u16::from_le_bytes([body[2], body[3]]);
    let strength = AesStrength::from_byte(body[4]).ok_or_else(fmt_3103)?;
    let real_method = u16::from_le_bytes([body[5], body[6]]);
    let version = match version_raw {
        0x0001 => AesVersion::Aes1,
        0x0002 => AesVersion::Aes2,
        _ => return Err(fmt_3103()),
    };
    if vendor != VENDOR_AE {
        return Err(fmt_3103());
    }
    Ok(AesEntryInfo {
        version,
        strength,
        real_method,
    })
}

/// 定位 0x9901 数据区（链扫描）；7 字节直接形态原样返回。
fn locate_9901_body(data: &[u8]) -> Option<&[u8]> {
    if data.len() == AE_EXTRA_LEN {
        return Some(data);
    }
    let mut off = 0usize;
    while off + EXTRA_HDR <= data.len() {
        let id = u16::from_le_bytes([data[off], data[off + 1]]);
        let len = u16::from_le_bytes([data[off + 2], data[off + 3]]) as usize;
        let end = off.checked_add(EXTRA_HDR + len)?;
        if end > data.len() {
            return None;
        }
        if id == EXTRA_ID_9901 {
            return Some(&data[off + EXTRA_HDR..end]);
        }
        off = end;
    }
    None
}

/// 3103 构造。
fn fmt_3103() -> HzipError {
    HzipError::Format {
        format: handler::fmt(),
        kind: FormatErrorKind::UnknownCrypto,
    }
}

/// 拒绝含 NUL 的密码（`Err Password 3101`；不静默截断、不 panic）。
pub(crate) fn reject_nul(pw: &str) -> Result<(), HzipError> {
    if pw.as_bytes().contains(&0) {
        return Err(HzipError::Password {
            target: PasswordTarget::Entry,
        });
    }
    Ok(())
}

/// 派生密钥三段（K ‖ V ‖ Verifier；02 §3.1 KV 拆包）。
pub(crate) struct AesKeys {
    /// 加密密钥有效段（长度 key_len）。
    enc: Zeroizing<Vec<u8>>,
    /// HMAC 密钥有效段（长度 key_len/2）。
    mac: Zeroizing<Vec<u8>>,
    /// PasswordVerifier 2 字节。
    pub(crate) verifier: [u8; 2],
}

impl AesKeys {
    pub(crate) fn enc_key(&self) -> &[u8] {
        &self.enc
    }
    pub(crate) fn mac_key(&self) -> &[u8] {
        &self.mac
    }
}

/// PBKDF2-HMAC-SHA1 派生 + KV 拆包（确定性）。
pub(crate) fn derive_keys(pw: &str, salt: &[u8], s: AesStrength) -> AesKeys {
    let key_len = s.key_len();
    let dk_len = key_len + key_len / 2 + 2;
    let mut dk = Zeroizing::new(vec![0u8; dk_len]);
    // 输出长度 ≤ 50 远低于 HMAC-SHA1 的 (2^32-1)*20 上限——必成功；仅编程不变量
    // 破坏时 panic（不可达），不构成「任意输入 panic」路径（输入经内部预哈希）。
    let res = pbkdf2::pbkdf2::<Hmac<Sha1>>(pw.as_bytes(), salt, PBKDF2_ITERATIONS, &mut dk);
    assert!(
        res.is_ok(),
        "PBKDF2 输出长度超出上限（dk_len ≤ 50，不可达）"
    );
    let enc = Zeroizing::new(dk[..key_len].to_vec());
    let mac = Zeroizing::new(dk[key_len..key_len + key_len / 2].to_vec());
    let mut verifier = [0u8; 2];
    verifier.copy_from_slice(&dk[dk_len - 2..]);
    AesKeys { enc, mac, verifier }
}

#[cfg(test)]
#[path = "tests/aes.rs"]
mod tests;
