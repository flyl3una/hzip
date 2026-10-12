//! BCJ2 四流解码器（02 §6.4；7-Zip LZMA SDK `Bcj2Dec` 忠实移植，本任务核心交付）。
//!
//! # 四流结构与顺序（与 7-Zip 容器子流序对齐）
//!
//! BCJ2 编码器把 x86 流拆为 4 个流（7-Zip `BCJ2_STREAM_*` 枚举，亦是 7z 容器中
//! coder=3 的 4 个输入子流索引）：
//!
//! | 索引 | 名称 | 内容 |
//! |---|---|---|
//! | 0 | main | 剥离转换位移后的主体字节流（LZMA 解压后） |
//! | 1 | call | `E8` (call rel32) 的 4 字节**大端**绝对地址流 |
//! | 2 | jump | `E9` (jmp rel32) 与 `0F 8x` (jcc rel32) 的 4 字节**大端**绝对地址流 |
//! | 3 | rc | 标记流：**范围编码（RC）** 的转换标记位（非简单 BitVector） |
//!
//! > 任务卡 §4.9 所述「每 8 个标记打包 1 字节 BitVector」与真实 7-Zip 产物不符：
//! > 7-Zip 的标记流是自适应二进制范围编码（5 位概率模型，`kTopValue = 1<<24`，
//! > 初始 5 字节 `code`）。为兑现「能解 7-Zip 产出的 BCJ2 档案」（P2 首要目标），
//! > 本实现按 7-Zip 参考语义交付，偏差记录见任务卡报告。
//!
//! 四流均传入**已 LZMA 解压**的字节流（T25 负责 4×LZMA 解压图装配，02 §8.2）。
//! 候选标记：`E8`/`E9` 单字节与 `0F 8x`（jcc rel32，2 字节）；候选判定按 7-Zip
//! 扫描规则（5 字节窗口、上下文字节索引概率），与编码器确定性对称。
//!
//! # 重组语义
//!
//! 解码器依 RC 标记回插跳转：标记位=转换时，从 call/jump 流读大端绝对地址 `abs`，
//! 回插相对位移 `rel = abs − (ip + 5)`（`ip` 为标记字节的虚拟地址，输出字节计数），
//! 4 字节小端写入输出；标记位=未转换时，后续 4 字节属 main 流原样输出。`ip` 即输出
//! 字节计数（虚拟指令地址）。
//!
//! # 收尾契约（TC-CONF-008）
//!
//! 四流必须同步耗尽：main 耗尽且 `code == 0` 为干净结束；main 剩余或 jump/call
//! 剩余非整 4 字节组 / rc 提前耗尽 → `finish` 报 `Err`（不 panic 穿越）。

use std::io::{self, Read};

use crate::traits::DecoderStats;

const KTOP_VALUE: u32 = 1 << 24;
const K_NUM_BIT_MODEL_TOTAL_BITS: u32 = 11;
const K_BIT_MODEL_TOTAL: u32 = 1 << K_NUM_BIT_MODEL_TOTAL_BITS; // 2048
const K_NUM_MOVE_BITS: u32 = 5;

const STREAM_MAIN: u8 = 0;
const STREAM_CALL: u8 = 1;
const STREAM_JUMP: u8 = 2;
const STREAM_RC: u8 = 3;

const DEC_STATE_ORIG_0: u8 = 4;
const DEC_STATE_ORIG_3: u8 = 7;
const DEC_STATE_ORIG: u8 = 8;
const DEC_STATE_ERROR: u8 = 9;

#[inline]
fn is_32bit_stream(s: u8) -> bool {
    s == STREAM_CALL || s == STREAM_JUMP
}

#[inline]
fn bcj2_prob_index(v: u32) -> usize {
    // 上下文索引（与 7-Zip Bcj2Dec 一致）：8x→0 : e9→1 : xxe8→xx+2
    let c = ((v.wrapping_add(0x17)) >> 6) & 1;
    let prev = (v >> 24) & 0xff;
    let bit5 = (v >> 5) & 1;
    (((0u32.wrapping_sub(c)) & prev) + c + bit5) as usize
}

/// 单条输入流缓冲。
#[derive(Debug, Default)]
struct StreamBuf {
    data: Vec<u8>,
    pos: usize,
    eof: bool,
}

impl StreamBuf {
    fn avail(&self) -> usize {
        self.data.len() - self.pos
    }

    fn refill_from(&mut self, r: &mut dyn Read) -> io::Result<()> {
        if self.pos >= 8192 {
            self.data.drain(..self.pos);
            self.pos = 0;
        }
        let mut tmp = [0u8; 8192];
        let n = loop {
            match r.read(&mut tmp) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        };
        if n == 0 {
            self.eof = true;
        } else {
            self.data.extend_from_slice(&tmp[..n]);
        }
        Ok(())
    }

    fn byte(&mut self) -> Option<u8> {
        if self.avail() == 0 {
            return None;
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Some(b)
    }

    fn be32(&mut self) -> u32 {
        debug_assert!(self.avail() >= 4);
        let v = u32::from_be_bytes([
            self.data[self.pos],
            self.data[self.pos + 1],
            self.data[self.pos + 2],
            self.data[self.pos + 3],
        ]);
        self.pos += 4;
        v
    }
}

/// BCJ2 四流解码器：`Read` 输出重组后的原始 x86 字节。
pub struct Bcj2Decoder {
    inner: [Box<dyn Read + Send>; 4],
    streams: [StreamBuf; 4],
    state: u8,
    ip: u32,
    temp: u32,
    range: u32,
    code: u32,
    probs: [u16; 2 + 256],
    crc: crate::crc::Crc32,
    bytes_out: u64,
    finished: bool,
}

impl Bcj2Decoder {
    /// 构造 BCJ2 解码器。
    ///
    /// 参数顺序即 7-Zip 容器子流序：`main`(0)、`call`(1)、`jump`(2)、`markers`/rc(3)。
    /// 四流均为已 LZMA 解压的字节流（T25 装配）。
    pub fn new(
        main: Box<dyn Read + Send>,
        call: Box<dyn Read + Send>,
        jump: Box<dyn Read + Send>,
        markers: Box<dyn Read + Send>,
    ) -> Self {
        Self {
            inner: [main, call, jump, markers],
            streams: Default::default(),
            state: STREAM_RC,
            ip: 0,
            temp: 0,
            range: 0,
            code: 0,
            probs: [K_BIT_MODEL_TOTAL as u16 >> 1; 2 + 256],
            crc: crate::crc::Crc32::new(),
            bytes_out: 0,
            finished: false,
        }
    }

    /// 读一条流字节；EOF 返回 `None`（自动拉取源数据）。
    fn pull_byte(&mut self, s: u8) -> io::Result<Option<u8>> {
        if self.streams[s as usize].avail() == 0 {
            if self.streams[s as usize].eof {
                return Ok(None);
            }
            self.streams[s as usize].refill_from(&mut *self.inner[s as usize])?;
        }
        Ok(self.streams[s as usize].byte())
    }

    /// 确保 32 位流可用 ≥4 字节（否则截断错误）。
    fn pull_be32(&mut self, s: u8) -> io::Result<u32> {
        while self.streams[s as usize].avail() < 4 && !self.streams[s as usize].eof {
            self.streams[s as usize].refill_from(&mut *self.inner[s as usize])?;
        }
        if self.streams[s as usize].avail() < 4 {
            return Err(self.truncated_err());
        }
        Ok(self.streams[s as usize].be32())
    }

    fn truncated_err(&self) -> io::Error {
        io::Error::new(io::ErrorKind::UnexpectedEof, "BCJ2 四流不同步（截断/损坏）")
    }

    /// 单步解码：写入 `out`，返回写入字节数；`Ok(0)` 仅表示干净结束或需更多输入。
    fn step(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut v = self.temp;
        let mut out_pos = 0usize;
        let mut state = self.state;

        // ---- RC 初始 code 装载（首 5 字节）----
        if self.range <= 5 {
            self.state = DEC_STATE_ERROR;
            while self.range != 5 {
                if self.range == 1 && self.code != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "BCJ2 标记流起始字节错误",
                    ));
                }
                match self.pull_byte(STREAM_RC)? {
                    Some(b) => {
                        self.code = (self.code << 8) | b as u32;
                        self.range += 1;
                    }
                    None => {
                        self.state = STREAM_RC;
                        self.temp = v;
                        return Ok(out_pos);
                    }
                }
            }
            if self.code == 0xffff_ffff {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "BCJ2 标记流初始 code 非法",
                ));
            }
            self.range = 0xffff_ffff;
            state = self.state;
        }

        // ---- 32 位流续读（转换中途暂停后恢复）----
        if is_32bit_stream(state) {
            let val = self.pull_be32(state)?;
            let ip = self.ip.wrapping_add(4);
            v = val.wrapping_sub(ip);
            self.ip = ip;
            state = DEC_STATE_ORIG_0;
        }
        if (DEC_STATE_ORIG_0..=DEC_STATE_ORIG_3).contains(&state) {
            // 输出剩余回插位移字节（小端）
            loop {
                if out_pos >= out.len() {
                    self.state = state;
                    self.temp = v;
                    return Ok(out_pos);
                }
                out[out_pos] = (v & 0xff) as u8;
                out_pos += 1;
                v >>= 8;
                if state == DEC_STATE_ORIG_3 {
                    break;
                }
                state += 1;
            }
        }

        // ---- 主循环 ----
        'decode: loop {
            if self.range < KTOP_VALUE {
                match self.pull_byte(STREAM_RC)? {
                    Some(b) => {
                        self.range <<= 8;
                        self.code = (self.code << 8) | b as u32;
                    }
                    None => {
                        self.state = STREAM_RC;
                        self.temp = v;
                        return Ok(out_pos);
                    }
                }
            }
            // 扫描 main → 拷贝到 out，遇标记或边界停
            loop {
                if out_pos >= out.len() {
                    self.state = DEC_STATE_ORIG;
                    self.temp = v;
                    return Ok(out_pos);
                }
                let b = match self.pull_byte(STREAM_MAIN)? {
                    Some(b) => b,
                    None => {
                        // main 流耗尽
                        self.state = STREAM_MAIN;
                        self.temp = v;
                        return Ok(out_pos);
                    }
                };
                out[out_pos] = b;
                out_pos += 1;
                self.ip = self.ip.wrapping_add(1);
                v = (v << 24) | b as u32;
                // (b + 0x18) & 0xfe == 0 ⟺ b ∈ {0xE8, 0xE9}
                let is_e89 = ((b.wrapping_add(0x18)) & 0xfe) == 0;
                let is_jcc = {
                    let x = v.wrapping_sub((0x0f << 24) + 0x80);
                    (x & (((1u32 << (4 + 24)) - 1) << 4)) == 0
                };
                if is_e89 || is_jcc {
                    break;
                }
            }
            // 标记位（range ≥ KTOP_VALUE 已由循环顶重归一保证）
            debug_assert!(self.range >= KTOP_VALUE);
            let idx = bcj2_prob_index(v);
            let ttt = self.probs[idx] as u32;
            let bound = (self.range >> K_NUM_BIT_MODEL_TOTAL_BITS) * ttt;
            let converted;
            if self.code < bound {
                self.range = bound;
                self.probs[idx] = (ttt + ((K_BIT_MODEL_TOTAL - ttt) >> K_NUM_MOVE_BITS)) as u16;
                converted = false;
            } else {
                self.range -= bound;
                self.code -= bound;
                self.probs[idx] = (ttt - (ttt >> K_NUM_MOVE_BITS)) as u16;
                converted = true;
            }
            if !converted {
                continue 'decode;
            }
            // 转换：从 call/jump 流读大端绝对地址，回插相对位移
            let cj = if ((v.wrapping_add(0x57) >> 6) & 1) == 0 {
                STREAM_CALL
            } else {
                STREAM_JUMP
            };
            let val = self.pull_be32(cj)?;
            let ip = self.ip.wrapping_add(4);
            let rel = val.wrapping_sub(ip);
            self.ip = ip;
            {
                let mut tmp = rel;
                let mut n = 0;
                while n < 4 && out_pos < out.len() {
                    out[out_pos] = (tmp & 0xff) as u8;
                    tmp >>= 8;
                    out_pos += 1;
                    n += 1;
                }
                if n < 4 {
                    // 输出缓冲不足：暂停，剩余字节续读
                    self.state = DEC_STATE_ORIG_0 + n;
                    self.temp = tmp;
                    return Ok(out_pos);
                }
                v = rel >> 24;
            }
        }
    }

    /// 干净结束判定（main 耗尽且范围解码 code 归零）。
    fn cleanly_finished(&self) -> bool {
        self.state == STREAM_MAIN && self.code == 0
    }

    /// 收尾：读至 EOF 并校验四流同步耗尽；截断/损坏返回 `Err`（TC-CONF-008）。
    pub fn finish(mut self) -> io::Result<DecoderStats> {
        let mut buf = [0u8; 8192];
        loop {
            let n = self.read(&mut buf)?;
            if n == 0 {
                break;
            }
        }
        if !self.cleanly_finished() {
            return Err(self.truncated_err());
        }
        // jump/call 不得剩余整 4 字节组；main 必须耗尽（cleanly_finished 已含）
        for s in [STREAM_JUMP, STREAM_CALL] {
            if self.streams[s as usize].avail() >= 4 {
                return Err(self.truncated_err());
            }
        }
        Ok(DecoderStats {
            bytes_out: self.bytes_out,
            crc32: self.crc.finalize(),
        })
    }
}

impl Read for Bcj2Decoder {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.finished {
            return Ok(0);
        }
        let n = self.step(buf)?;
        if n > 0 {
            self.crc.update(&buf[..n]);
            self.bytes_out += n as u64;
            return Ok(n);
        }
        // n == 0：干净结束或输入不足
        if self.cleanly_finished() {
            self.finished = true;
            return Ok(0);
        }
        // 输入不足且对应流已耗尽 → 截断
        if self.state < STREAM_RC + 1 {
            let s = self.state as usize;
            if self.streams[s].eof {
                return Err(self.truncated_err());
            }
        }
        Err(self.truncated_err())
    }
}
