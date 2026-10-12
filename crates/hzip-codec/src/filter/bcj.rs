//! BCJ（x86 单流）分支过滤器解码器（02 §6.4；7-Zip LZMA SDK Bra86 语义）。
//!
//! 解码方向：读取已过滤（编码）流，将 E8/E9 之后的 4 字节位移由**绝对地址**
//! 还原为相对位移（`rel = abs − (ip + 5)`，`ip` 为指令起始虚拟地址），字节级还原
//! 原始 x86 代码。候选判定遵循 7-Zip 规则：5 字节指令组内（上一次转换后 4 字节内）
//! 的 E8/E9 不视为候选（确定性对称）；BCJ 单流不处理 jcc（`0F 8x`），仅 E8/E9。
//!
//! 编码方向（相对→绝对，与解码互为逆）供测试自验证使用，正式编码器归 T46。

use std::io::{self, Read};

use crate::traits::{Decoder, DecoderStats};

/// 4 字节窗口内首个候选字节偏移（`(b & 0xfe) == 0xe8`，即 E8/E9），无则 `None`。
#[inline]
fn window_marker(data: &[u8], q: usize) -> Option<usize> {
    debug_assert!(q + 3 < data.len());
    (0..4).find(|&i| (data[q + i] & 0xfe) == 0xe8)
}

/// `b == 0xff`（负向位移 MS 字节需二次转换处理）。
#[inline]
fn needs_conv_ms(b: u8) -> bool {
    ((b.wrapping_add(1)) & 0xfe) == 0
}

/// Bra86 x86 分支转换（7-Zip `Z7_BRANCH_CONV_ST(X86)` 忠实移植；`encoding=true` 编码、
/// `false` 解码）。就地转换 `data`；`ip` 为缓冲区起始文件偏移（值参，调用方需自行
/// 按返回的处理字节数前移）；`state` 为跨块掩码（调用方持久化）。返回已处理字节数，
/// 末尾 ≤4 字节原样保留（可能属于未闭合的转换窗口，留待下一块）。
///
/// `#[allow(clippy::expect_used)]`：内部两处 `try_into().expect()` 的窗口边界由
/// `lim = size - 4` 静态保证（p < lim 即 p+4 ≤ size），expect 不可达——转换器为
/// 热路径纯函数，不引入 Result 传播。
#[allow(clippy::expect_used)]
pub(crate) fn x86_convert(data: &mut [u8], ip: u32, state: &mut u32, encoding: bool) -> usize {
    let size = data.len();
    if size < 5 {
        return 0;
    }
    let lim = size - 4;
    let pc = ip.wrapping_add(size as u32); // BR_PC_INIT: pc += size
    let mut mask: u32 = *state;
    let mut p: usize = 0;

    macro_rules! pc_get {
        () => {
            pc.wrapping_sub((lim - p) as u32)
        };
    }

    // a3 转换：读 p 处 4 字节位移并就地改写。返回 false 表示位移过大（不转换，mask|=4 → start）。
    macro_rules! convert_a3 {
        () => {{
            let mut v = u32::from_le_bytes(data[p..p + 4].try_into().expect("window in bounds"));
            v = v.wrapping_add(1 << 24);
            if v & 0xfe00_0000 != 0 {
                false
            } else {
                let c = pc_get!();
                v = if encoding {
                    v.wrapping_add(c)
                } else {
                    v.wrapping_sub(c)
                };
                v &= (1 << 25) - 1;
                v = v.wrapping_sub(1 << 24);
                data[p..p + 4].copy_from_slice(&v.to_le_bytes());
                true
            }
        }};
    }

    'outer: loop {
        // ---------------- start ----------------
        if p >= lim {
            break;
        }
        let m = window_marker(data, p);
        p += 4;
        match m {
            None => {
                // C：IS(0)..IS(2) 均未命中后 mask=0，再 IS(3) 未命中 → main_loop
                mask = 0;
                // → main_loop
            }
            Some(3) => {
                // C：IS(2) 未命中置 mask=0，IS(3) 命中 → a3
                mask = 0;
                // → a3
                if p > lim {
                    p -= 1;
                    break;
                }
                if convert_a3!() {
                    p += 4;
                } else {
                    mask |= 4;
                    continue 'outer;
                }
                // → main_loop
            }
            Some(k) => {
                // C start 块逐字节检查间的 mask 移位：命中前每跳过 1 字节 mask >>= 1
                for _ in 0..k {
                    mask >>= 1;
                }
                p -= 3 - k; // m{k}: 标签贯通 p-- (3-k) 次（k=0→3 次, k=1→2, k=2→1）
                if mask == 0 {
                    // → a3
                    if p > lim {
                        p -= 1;
                        break;
                    }
                    if convert_a3!() {
                        p += 4;
                    } else {
                        mask |= 4;
                        continue 'outer;
                    }
                    // → main_loop
                } else if p > lim {
                    p -= 1;
                    break;
                } else if mask > 4 || mask == 3 {
                    // C：mask >>= 1 后 continue（外层 for 的 mask |= 4 生效）
                    mask = (mask >> 1) | 4;
                    continue 'outer;
                } else {
                    mask >>= 1;
                    if needs_conv_ms(data[p + mask as usize]) {
                        mask |= 4;
                        continue 'outer;
                    }
                    // m-path 转换（含 mask<<3 二次处理）
                    let mut v =
                        u32::from_le_bytes(data[p..p + 4].try_into().expect("window in bounds"));
                    v = v.wrapping_add(1 << 24);
                    if v & 0xfe00_0000 != 0 {
                        mask |= 4;
                        continue 'outer;
                    }
                    let c = pc_get!();
                    v = if encoding {
                        v.wrapping_add(c)
                    } else {
                        v.wrapping_sub(c)
                    };
                    let m3 = mask << 3;
                    if needs_conv_ms((v >> m3) as u8) {
                        v ^= (0x100u32 << m3).wrapping_sub(1);
                        let c2 = pc_get!();
                        v = if encoding {
                            v.wrapping_add(c2)
                        } else {
                            v.wrapping_sub(c2)
                        };
                    }
                    mask = 0;
                    v &= (1 << 25) - 1;
                    v = v.wrapping_sub(1 << 24);
                    data[p..p + 4].copy_from_slice(&v.to_le_bytes());
                    p += 4;
                    // → main_loop
                }
            }
        }

        // ---------------- main_loop ----------------
        loop {
            if p >= lim {
                break 'outer;
            }
            let m = window_marker(data, p);
            p += 4;
            match m {
                None => {
                    if p >= lim {
                        break 'outer;
                    }
                    continue;
                }
                Some(k) => {
                    p -= 3 - k; // a{k}: 标签贯通 p-- (3-k) 次
                    // → a3
                    if p > lim {
                        p -= 1;
                        break 'outer;
                    }
                    if convert_a3!() {
                        p += 4;
                    } else {
                        mask |= 4;
                        break; // → outer start
                    }
                    continue; // → main_loop 继续扫描
                }
            }
        }
    }

    *state = mask;
    p
}

/// BCJ（x86 单流）解码器：`Read` 输出还原后的原始 x86 字节。
///
/// 无独立帧格式，故无截断语义（截断仅影响边界处的 E8/E9 位移窗口，不产生错误）；
/// `finish` 返回已输出字节统计。
pub struct BcjDecoder {
    inner: Box<dyn Read + Send>,
    pending: Vec<u8>,
    out: Vec<u8>,
    out_pos: usize,
    ip: u32,
    state: u32,
    crc: crate::crc::Crc32,
    bytes_out: u64,
    eof: bool,
}

impl BcjDecoder {
    /// 构造 BCJ 解码器，读取已过滤流 `inner`。
    pub fn new(inner: Box<dyn Read + Send>) -> Self {
        Self {
            inner,
            pending: Vec::new(),
            out: Vec::new(),
            out_pos: 0,
            ip: 0,
            state: 0,
            crc: crate::crc::Crc32::new(),
            bytes_out: 0,
            eof: false,
        }
    }
}

impl Read for BcjDecoder {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        // 耗尽已转换输出
        while self.out_pos >= self.out.len() {
            self.out.clear();
            self.out_pos = 0;
            // 确保 pending 有 ≥5 字节或 EOF
            while self.pending.len() < 5 && !self.eof {
                let mut tmp = [0u8; 8192];
                let n = self.inner.read(&mut tmp)?;
                if n == 0 {
                    self.eof = true;
                } else {
                    self.pending.extend_from_slice(&tmp[..n]);
                }
            }
            if self.pending.len() < 5 {
                // EOF 且不足一个窗口：无可转换内容，原样输出尾部
                if self.pending.is_empty() {
                    return Ok(0);
                }
                self.out = std::mem::take(&mut self.pending);
                break;
            }
            let processed = x86_convert(&mut self.pending, self.ip, &mut self.state, false);
            self.ip = self.ip.wrapping_add(processed as u32);
            let tail = self.pending.split_off(processed);
            self.out = std::mem::replace(&mut self.pending, tail);
            if self.out.is_empty() && self.eof {
                // 尾部 <5 字节且已 EOF → 原样输出
                self.out = std::mem::take(&mut self.pending);
                break;
            }
            if self.out.is_empty() && !self.eof {
                // processed==0 且非 EOF：pending 可能 <5？不会（上面保证 ≥5）。
                // 理论不可达；防御性继续读。
                continue;
            }
            break;
        }
        let n = buf.len().min(self.out.len() - self.out_pos);
        buf[..n].copy_from_slice(&self.out[self.out_pos..self.out_pos + n]);
        self.out_pos += n;
        self.crc.update(&buf[..n]);
        self.bytes_out += n as u64;
        Ok(n)
    }
}

impl Decoder for BcjDecoder {
    fn finish(self: Box<Self>) -> io::Result<DecoderStats> {
        let this = *self;
        Ok(DecoderStats {
            bytes_out: this.bytes_out,
            crc32: this.crc.finalize(),
        })
    }
}
