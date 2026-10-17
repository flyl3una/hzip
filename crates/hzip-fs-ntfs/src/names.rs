//! NTFS 名字：UTF-16 ↔ UTF-8（计划 `docs/tasks/T-IMG/T-IMG11-ntfs.md` §2.2）。
//!
//! # 为什么这一层单独成模块
//!
//! NTFS 的 `$FILE_NAME` 名字是 **UTF-16 码元序列**（不是字节流，也没有编码声明），
//! hzip 的 `EntryName` 是 UTF-8 字符串。上游 `read::read_dir_entries` / 路径解析
//! 已经做过一次 `String::from_utf16_lossy`，所以本模块的两个方向函数是**写侧与
//! 诊断侧的单一约定**，也是「不丢条目」这条红线的落点：
//!
//! - 非法代理对（未配对 surrogate）→ `U+FFFD`：条目**照样列出**，由
//!   [`is_lossy_name`] 让调用方把次数记进诊断（`NtfsVolume::lossy_name_count`，
//!   `EntryTree` 目前没有 `warnings` 字段——见 crate 文档的偏离说明）；
//! - 写侧不许由我们造出非法名：[`validate_new_name`] 在任何上游调用**之前**拒掉
//!   超长/含非法字符/以空格或句点结尾的名字（Windows 会静默裁掉后者，我们宁可说
//!   出来）。

use hzip_vol::{VolError, VolResult};

/// NTFS 名字上限（**UTF-16 码元数**）：`$FILE_NAME.file_name_length` 是 `u8` ⇒ 255。
///
/// 与 `FsCaps::name_max` 的口径一致（`hzip_vol` 的注释把 NTFS 记为 255）。
pub const NAME_MAX: usize = 255;

/// Windows 拒绝、而 NTFS 本身允许的字符（按 MS-FSCC：路径分隔符与通配符）。
const ILLEGAL_CHARS: &[char] = &['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// UTF-16 → hzip 条目名（never-fail：非法代理对变 `U+FFFD`，**不丢条目**）。
#[must_use]
pub fn utf16_to_entry_name(raw: &[u16]) -> String {
    String::from_utf16_lossy(raw)
}

/// hzip 条目名 → UTF-16（写侧：交给上游的路径/名字参数）。
#[must_use]
pub fn entry_name_to_utf16(name: &str) -> Vec<u16> {
    name.encode_utf16().collect()
}

/// 名字里是否出现替换字符 `U+FFFD`。
///
/// 这是「上游那次 `from_utf16_lossy` 丢过信息」的**可观测标志**（也可能只是卷里
/// 真有一个叫 `U+FFFD` 的条目——两种情况都值得让用户知道）。
#[must_use]
pub fn is_lossy_name(name: &str) -> bool {
    name.contains(char::REPLACEMENT_CHARACTER)
}

/// 写侧名字校验：不合法就别去打扰上游（上游也会拒，但那时可能已经动过盘）。
///
/// 逐条口径：
/// - 空名 ⇒ 拒（NTFS 没有无名条目）；
/// - `.` / `..` ⇒ 拒（目录索引里是合成项，不是真名字）；
/// - UTF-16 码元数 > [`NAME_MAX`] ⇒ 拒；
/// - 含 `\0`、C0/C1 控制字符 ⇒ 拒（Windows 打不开）；
/// - 含 [`ILLEGAL_CHARS`] ⇒ 拒（Windows 把 `/` 当分隔符、`?` 当通配符）；
/// - 以空格或句点结尾 ⇒ 拒（Windows 解析路径时会裁掉它，之后再也打不开原名）。
pub fn validate_new_name(name: &str) -> VolResult<()> {
    let bad = |key: &'static str| Err(VolError::Unsupported(key));
    if name.is_empty() || name == "." || name == ".." {
        return bad("vol.err.invalid-name");
    }
    if entry_name_to_utf16(name).len() > NAME_MAX {
        return bad("vol.err.name-too-long");
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return bad("vol.err.invalid-name");
    }
    for ch in name.chars() {
        let code = ch as u32;
        if ch == '\0' || code <= 0x1F || code == 0x7F || (0x80..=0x9F).contains(&code) {
            return bad("vol.err.invalid-name");
        }
        if ILLEGAL_CHARS.contains(&ch) {
            return bad("vol.err.invalid-name");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
