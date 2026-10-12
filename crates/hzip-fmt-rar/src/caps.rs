//! RAR 能力矩阵与签名（02 §8.3 / §7.1；T26 §3.3/§3.4）。
//!
//! caps 声明对照（02 §7.2 / T26 §3.4）：
//!
//! | 字段 | 值 | 说明 |
//! |---|---|---|
//! | `read` | true | rar4/rar5 只读 |
//! | `write`/`append`/`edit` | false | D2：rar 压缩永不做；写/追加/编辑 → `Unsupported` 5001 |
//! | `encryption` | `Aes(Bits256)` | 读能力声明：rar5 数据加密 AES-256（rar4 专有算法兼容亦经 unrar，见 crate 文档） |
//! | `split` | true | 读侧卷集（`reader` + `volume` 层） |
//! | `solid` | true | 只读固实块（unrar 处理） |
//! | `comment` | false | rar 注释不呈现 |

use hzip_format::Signature;
use hzip_format::caps::{AesKey, EncryptionCaps, FormatCaps};

/// rar 格式 ID（02 §7.2）。
pub const FORMAT_ID: &str = "rar";

/// rar4 魔数 `Rar!\x1A\x07\x00`（7 B，02 §8.3/T26 §3.3 差异表）。
pub(crate) const MAGIC_RAR4: [u8; 7] = *b"Rar!\x1a\x07\x00";
/// rar5 魔数 `Rar!\x1A\x07\x01\x00`（8 B）。
pub(crate) const MAGIC_RAR5: [u8; 8] = *b"Rar!\x1a\x07\x01\x00";

/// 签名：rar5 长魔数在前（8 B，先于 rar4 匹配——与 `hzip_format::sniffer`
/// 魔数表序一致，长魔数优先；同一 id 两个 handler 不可共存，故 Signature 只报
/// 一个，匹配由 sniffer 的魔数表完成）。
pub static SIGNATURE: Signature = Signature {
    magic: &MAGIC_RAR5,
    offset: 0,
    display: "rar",
    extensions: &["rar", "part1.rar"],
};

/// 读档案头魔数判定是否 rar5。
///
/// rar4 / rar5 魔数共享前 6 B `Rar!\x1A\x07`，第 7 B 区分版本：rar4 = `0x00`
/// （共 7 B）、rar5 = `0x01`（共 8 B，末字节 `0x00`）。纯字节级读前 8 B；非
/// rar / 不足 7 B → `false`。命名风格不唯魔数版本（rar5 可用旧命名、rar4 可见
/// 新命名，T26 §3.2）——本判定只作诊断记录。
pub(crate) fn header_is_rar5(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut head = [0u8; 8];
    let n = match f.read(&mut head) {
        Ok(n) => n,
        Err(_) => return false,
    };
    if n < 7 {
        return false; // rar4 最短魔数也要 7 B
    }
    if head[..7] == MAGIC_RAR4 {
        return false; // rar4
    }
    n >= 8 && head[..8] == MAGIC_RAR5
}

/// 能力矩阵（T26 §3.4 逐字对照，见模块文档）。
pub(crate) fn caps() -> FormatCaps {
    FormatCaps {
        read: true,
        write: false,
        append: false,
        edit: false,
        encryption: EncryptionCaps::Aes(AesKey::Bits256),
        split: true,
        solid: true,
        comment: false,
        // rar 只读（write=false）：写侧无可选压缩方式
        methods: &[],
        // 只读格式：写入永不加密
        write_encryption: &[],
    }
}

#[cfg(test)]
#[path = "tests/caps.rs"]
mod tests;
