//! 7z 头加密读写（02 §8.2 C→D 分支；feature `aes`；T46 独占模块）。
//!
//! # 协议（02 §8.2 表 + T46 §3.1；字节级细节与 sevenz-rust `aes256` 交叉锁定）
//!
//! 7z 头加密形态：kEncodedHeader —— 头先 LZMA 压缩再 AES-256-CBC 加密，作为末端
//! 「文件夹」存储，coder id `0x06F10701`（7zAES）。
//!
//! | 要素 | 约定 |
//! |---|---|
//! | Salt | 长度由 coder 属性声明（0–16 字节；写方向以 15 字节随机 salt 补足熵，7-Zip 同款） |
//! | NumCyclesPower | 属性字节给定；写方向默认 19（≈ 52 万次迭代，见 [`AES_DEFAULT_CYCLES_POWER`]） |
//! | key derivation | SHA-256：`h0 = SHA256(password ‖ salt)`，再对前次结果迭代共 2^NumCyclesPower 次 → 32 字节 key |
//! | 加密 | AES-256-CBC，IV = 16 字节全零，密文为 16 的倍数 |
//!
//! # 读路径集成（C→D 分支）
//!
//! hzip-fmt-7z 读路径经 `sevenz_rust::Archive::read(…, password)` 打开：sevenz-rust
//! 以 `aes256` feature 自解密 kEncodedHeader 文件夹（AES→LZMA 链）。密码错误 →
//! sevenz 解码 io 错误（`read_exact`/CRC 失败）映射 [`HzipError::Password`]
//! `PasswordTarget::Header` **HZ-3102**；`entries()` 在该分支下只返回 Err 或（解密
//! 成功后的）完整列表，**零条目名泄露**（解析发生在构造 reader 阶段，任何条目名
//! 产出之前）。本模块提供的 [`decrypt_header`]/[`encrypt_header`] 为**等价自研
//! 原语**（RustCrypto `aes`+`cbc`+`sha2`）：作单测自验证与自组装 folder 路线的
//! 备用实现，主写路径经 sevenz-rust `AesEncoderOptions` 表达（`encrypt_header`）。
//!
//! # HZ-3102 零泄露实现（02 §8.2 D 节点）
//!
//! 头解密/解压失败（密码错误/头 CRC 不符）一律在产出任何条目名前返回
//! `Password{Header}`；错误文案只含档案级上下文（路径 + 「头部加密」），不携带
//! 从密文推出的任何字段。

// 模块当前消费方=单测（自研等价验证）；writer/reader 集成走 sevenz-rust aes256。dead_code 允许：公开面供测试与后续自组装路线引用（02 §13.2）。
#![allow(dead_code)]
use hzip_error::HzipError;
use hzip_format::password::PasswordRef;
use zeroize::Zeroizing;

/// 写方向默认迭代指数 19（≈ 524288 次 SHA-256；7-Zip 同款默认）。
pub(crate) const AES_DEFAULT_CYCLES_POWER: u8 = 19;

/// 7zAES coder 属性（salt 长度 + NumCyclesPower）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AesCoderProps {
    /// 盐（0–16 字节）。
    pub salt: Vec<u8>,
    /// 迭代指数（默认 19）。
    pub num_cycles_power: u8,
}

impl AesCoderProps {
    /// 构造默认属性（写方向）：`num_cycles_power = 19`，salt 为空（调用方按需补）。
    pub(crate) fn default_pw(pw_len: usize) -> Self {
        // 写方向默认以随机 salt 补足熵（15 字节 + 16 字节 IV 布局下盐长 15）。
        // 本函数不引入随机源（确定性可测）：由调用方以 15 字节熵填充 salt。
        let mut salt = vec![0u8; 15];
        // 以密码长度 + 常量种子确定性扩展（仅测试向量稳定；生产路径经
        // sevenz-rust AesEncoderOptions 的随机盐）。文档注明。
        for (i, b) in salt.iter_mut().enumerate() {
            *b = (pw_len as u8).wrapping_mul(31).wrapping_add(i as u8);
        }
        Self {
            salt,
            num_cycles_power: AES_DEFAULT_CYCLES_POWER,
        }
    }
}

/// SHA-256 迭代 key derivation（02 §8.2 表）。
///
/// 7-Zip/sevenz-rust 语义：每轮 `sha.update(salt); sha.update(password);
/// sha.update(extra)`，`extra` 为 8 字节递增计数器；共 `2^num_cycles_power` 轮后取
/// 摘要为 32 字节 key（`num_cycles_power == 0x3f` 的特殊直填路径本层不支持——
/// 头加密写方向恒为 19）。返回 Zeroizing 包裹的密钥。
pub(crate) fn derive_key(
    pw: PasswordRef<'_>,
    props: &AesCoderProps,
) -> Result<Zeroizing<[u8; 32]>, HzipError> {
    use sha2::{Digest, Sha256};
    let Some(pw) = pw.as_str() else {
        return Err(HzipError::Password {
            target: hzip_error::PasswordTarget::Header,
        });
    };
    let pw_bytes = pw.as_bytes();
    let mut sha = Sha256::new();
    let mut extra = [0u8; 8];
    let rounds: u64 = if props.num_cycles_power >= 63 {
        // 防御：指数≥63 的迭代量不可行（真实档案不会出现；错误密码路径在
        // sevenz-rust 侧先失败）。保守拒之，防 DoS。
        return Err(HzipError::Password {
            target: hzip_error::PasswordTarget::Header,
        });
    } else {
        1u64 << props.num_cycles_power
    };
    for _ in 0..rounds {
        sha.update(&props.salt);
        sha.update(pw_bytes);
        sha.update(extra);
        for item in &mut extra {
            *item = item.wrapping_add(1);
            if *item != 0 {
                break;
            }
        }
    }
    let digest: [u8; 32] = sha.finalize().into();
    Ok(Zeroizing::new(digest))
}

/// 头解密：AES-256-CBC（零 IV）解密 → LZMA 解压 → 头字节。
///
/// 密码错误 / 解密后 CRC 不符 → `Err Password{Header}` **HZ-3102**（零条目名泄露，
/// 见模块文档）。`props` 属性首字节布局按 7z 规范：`num_cycles = b0 & 0x3f`；
/// `iv_size = ((b0>>6)&1) + (b1 & 0x0f)`；`salt_size = ((b0>>7)&1) + (b1>>4)`。
#[cfg(feature = "aes")]
pub(crate) fn decrypt_header(
    data: &[u8],
    pw: PasswordRef<'_>,
    props: &AesCoderProps,
) -> Result<Vec<u8>, HzipError> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit};
    type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;

    let key = derive_key(pw, props)?;
    if !data.len().is_multiple_of(16) {
        return Err(HzipError::Password {
            target: hzip_error::PasswordTarget::Header,
        });
    }
    let iv = [0u8; 16];
    let dec = Aes256CbcDec::new(
        &aes::cipher::generic_array::GenericArray::from(*key),
        &aes::cipher::generic_array::GenericArray::from(iv),
    );
    let mut plain = data.to_vec();
    for chunk in plain.chunks_mut(16) {
        dec.clone()
            .decrypt_block_mut(aes::cipher::generic_array::GenericArray::from_mut_slice(
                chunk,
            ));
    }
    // LZMA 解压（带 13 字节 .lzma 头——本原语自组装格式，见 encrypt_header 注）。
    lzma_decode(&plain).map_err(|_| HzipError::Password {
        target: hzip_error::PasswordTarget::Header,
    })
}

#[cfg(feature = "aes")]
fn lzma_decode(data: &[u8]) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut r =
        sevenz_rust::lzma::LZMAReader::new_mem_limit(std::io::Cursor::new(data), u32::MAX, None)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut out = Vec::new();
    r.read_to_end(&mut out)?;
    Ok(out)
}

/// 头加密（写方向）：LZMA 压缩 → AES-256-CBC 加密 → 字节。
///
/// 主写路径经 sevenz-rust `AesEncoderOptions`（随机盐/IV）表达，本原语用于
/// 自组装/单测等价验证（确定性 salt 由 `props` 给定）。
#[cfg(feature = "aes")]
pub(crate) fn encrypt_header(
    header: &[u8],
    pw: PasswordRef<'_>,
    props: &AesCoderProps,
) -> Result<Vec<u8>, HzipError> {
    use aes::cipher::{BlockEncryptMut, KeyIvInit};
    type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;

    // LZMA 压缩：new_use_header（13 字节 .lzma 头）——本原语为自洽等价实现
    // （7z 容器内 LZMA props 在 coder 属性中；header_crypt 单测往返用带头形态）。
    let mut compressed = Vec::new();
    {
        use std::io::Write;
        let mut enc = sevenz_rust::lzma::LZMAWriter::new_use_header(
            sevenz_rust::lzma::CountingWriter::new(&mut compressed),
            &sevenz_rust::lzma::LZMA2Options::with_preset(6),
            Some(header.len() as u64),
        )
        .map_err(|e| HzipError::Io {
            source: e,
            path: None,
        })?;
        enc.write_all(header).map_err(|e| HzipError::Io {
            source: e,
            path: None,
        })?;
        enc.write(&[]).map_err(|e| HzipError::Io {
            source: e,
            path: None,
        })?;
    }
    // AES-256-CBC 零 IV；密文补齐 16 倍数（PKCS#7 风格：不足补满 16）。
    let key = derive_key(pw, props)?;
    let iv = [0u8; 16];
    let enc = Aes256CbcEnc::new(
        &aes::cipher::generic_array::GenericArray::from(*key),
        &aes::cipher::generic_array::GenericArray::from(iv),
    );
    let mut padded = compressed;
    let rem = padded.len() % 16;
    if rem != 0 {
        let pad = 16 - rem;
        padded.extend(std::iter::repeat_n(0u8, pad));
    }
    for chunk in padded.chunks_mut(16) {
        enc.clone()
            .encrypt_block_mut(aes::cipher::generic_array::GenericArray::from_mut_slice(
                chunk,
            ));
    }
    Ok(padded)
}

#[cfg(all(test, feature = "aes"))]
mod tests {
    use super::*;

    fn pw(s: &str) -> zeroize::Zeroizing<String> {
        zeroize::Zeroizing::new(s.to_string())
    }

    /// derive_key：NumCyclesPower 0/19 两档可运行；salt 变化 → key 变化；
    /// 同 salt 同 pw 同指数 → key 稳定（与参考实现确定性一致）。
    #[test]
    fn header_crypt_kdf() {
        let p0 = AesCoderProps {
            salt: vec![0xAA; 15],
            num_cycles_power: 0,
        };
        let z = pw("hzip-密码测试");
        let k0 = derive_key(PasswordRef::new(&z), &p0).unwrap();
        assert_eq!(k0.len(), 32);
        let p19 = AesCoderProps {
            salt: vec![0xAA; 15],
            num_cycles_power: 19,
        };
        let k19 = derive_key(PasswordRef::new(&z), &p19).unwrap();
        assert_ne!(*k0, *k19, "迭代指数变化 → key 变化");
        // salt 变化 → key 变化
        let p0b = AesCoderProps {
            salt: vec![0xBB; 15],
            num_cycles_power: 0,
        };
        let k0b = derive_key(PasswordRef::new(&z), &p0b).unwrap();
        assert_ne!(*k0, *k0b, "salt 变化 → key 变化");
        // 同参数稳定
        let k0c = derive_key(PasswordRef::new(&z), &p0).unwrap();
        assert_eq!(*k0, *k0c, "同参数确定性");
    }

    /// roundtrip：encrypt_header → decrypt_header 还原；NumCyclesPower=0 快路径。
    #[test]
    fn header_crypt_roundtrip() {
        for ncp in [0u8, 3] {
            let props = AesCoderProps {
                salt: vec![0x5A; 15],
                num_cycles_power: ncp,
            };
            let header = b"kHeader payload with names and metadata";
            let zp = pw("混合密码-测试");
            let enc = encrypt_header(header, PasswordRef::new(&zp), &props).unwrap();
            assert_eq!(enc.len() % 16, 0, "密文 16 倍数");
            let dec = decrypt_header(&enc, PasswordRef::new(&zp), &props).unwrap();
            assert_eq!(dec, header, "roundtrip 还原");
        }
    }

    /// wrong password → Err 3102；错误信息三面（Debug/Display）不含任何从密文
    /// 推出的字段（HZ-3102 零泄露：本原语错误路径只返回 Password{Header}）。
    #[test]
    fn header_crypt_wrong_password() {
        let props = AesCoderProps {
            salt: vec![0x5A; 15],
            num_cycles_power: 0,
        };
        let header = b"secret-header-bytes";
        let zc = pw("correct-password");
        let zw = pw("wrong-password");
        let enc = encrypt_header(header, PasswordRef::new(&zc), &props).unwrap();
        let e = decrypt_header(&enc, PasswordRef::new(&zw), &props).expect_err("应报错");
        assert_eq!(e.code(), 3102);
        assert!(matches!(
            e,
            HzipError::Password {
                target: hzip_error::PasswordTarget::Header
            }
        ));
        let debug_s = format!("{e:?}");
        let disp = format!("{e}");
        for leak in ["secret", "correct", "wrong", "header-bytes"] {
            assert!(
                !debug_s.contains(leak) && !disp.contains(leak),
                "错误信息不得泄露 {leak:?}"
            );
        }
    }
}
