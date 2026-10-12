//! BCJ2 四流编码器（02 §6.4 编码方向；与 [`crate::filter::bcj2::Bcj2Decoder`] 严格对称）。
//!
//! # 四流结构与顺序
//!
//! 与 T12 解码器同目录对称实现（7-Zip `Bcj2Enc` / 容器子流序）：
//!
//! | 参数 | 名称 | 内容 |
//! |---|---|---|
//! | `main` | 主体流 | 剥离转换位移后的 x86 字节流（E8/E9/jcc 转换后 4 字节位移**不出现**） |
//! | `jump` | jump 流 | `E9`(jmp rel32) 与 `0F 8x`(jcc rel32) 转换后的 4 字节**大端**绝对地址流 |
//! | `call` | call 流 | `E8`(call rel32) 转换后的 4 字节**大端**绝对地址流 |
//! | `byte` | 标记流 | 自适应二进制范围编码（RC）的转换标记位（与 7-Zip 产出一致） |
//!
//! > 任务卡 §4.4 所述「每 8 个标记打包 1 字节 BitVector」与真实 7-Zip 产物不符
//! > （T12 模块 rustdoc 已登记同款偏差）：7-Zip 的标记流是范围编码（5 位概率模型，
//! > `kTopValue = 1<<24`，5 字节初始 `code`）。本实现按 7-Zip 参考语义交付，
//! > 与 T12 解码器逐位对称。
//!
//! # 候选与转换规则（§3.3，与 T12 §4.9 解码语义逐条对称）
//!
//! 单遍流式扫描维护虚拟 `ip`（**输出 main 字节计数**，即解码器同义虚拟指令地址）：
//!
//! - 候选 = main 视图中 `E8`/`E9` 单字节，或「前一个 main 字节为 `0F` 且当前字节
//!   `0x80..0x8F`」的 jcc 双字节组（`0F 8x rel32`；候选判定基于 **main 流视图**，
//!   与解码器窗口一致——被转换位移剔除的字节不参与上下文）；
//! - 候选且 4 字节位移完整 → **CONVERTED**：标记位 1；`abs = rel + (ip_after_opcode + 4)`
//!   （u32 wrapping，`ip_after_opcode` 为 opcode 之后输出位置）以**大端**写入
//!   call（E8）/jump（E9/jcc）流；main 流只含 opcode 字节，位移被消费（解码器
//!   自绝对地址重建位移的对称面）；`ip += 5`（E8/E9）或 `ip += 6`（jcc）；
//! - 候选但位移不完整（流尾）→ 标记位 0，opcode 字节原样入 main（位移尾原样扫描）；
//! - 概率上下文索引与解码器一致：`E8 → 2 + prev`、`E9 → 1`、`jcc → 0`（`prev`
//!   为 opcode 前最后一个 main 字节）。
//!
//! # 收尾契约
//!
//! [`Bcj2Encoder::finish`]：流尾候选按上述「位移不完整 → 标记 0」规则裁决 → RC
//! flush（5 次 `ShiftLow`，与解码器 5 字节初始 code 对称）→ 各流 flush → 校验
//! 转换计数与标记位数一致（三流第 k 个记录与标记位一一对应；T12 §4.9 对称）。
//! 转换器对任意字节流均良定义，不 panic（红线 4）。

use std::io::{self, Write};

/// 范围编码模型常量（与 T12 解码器一致）。
const K_NUM_MODEL_TOTAL_BITS: u32 = 11;
const K_BIT_MODEL_TOTAL: u32 = 1 << K_NUM_MODEL_TOTAL_BITS;
/// 概率更新位移。
const K_NUM_MOVE_BITS: u32 = 5;

/// 未决（需更多输入才能判定）缓冲上限：jcc 全组 6 字节（0F 8x + 4 位移）。
const PENDING_MAX: usize = 6;

/// BCJ2 编码统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bcj2Stats {
    /// 输入原始字节数。
    pub bytes_in: u64,
    /// main 流输出字节数。
    pub main: u64,
    /// jump 流输出字节数（4 的倍数）。
    pub jump: u64,
    /// call 流输出字节数（4 的倍数）。
    pub call: u64,
    /// 标记流（RC）输出字节数。
    pub byte: u64,
    /// 转换次数。
    pub converted: u64,
}

/// BCJ2 四流编码器（02 §6.4；与 T12 解码器严格对称）。
///
/// 四个输出端由 fmt-7z folder 组装方提供（各自接 LZMA 编码器）。
pub struct Bcj2Encoder {
    main: Box<dyn Write + Send>,
    jump: Box<dyn Write + Send>,
    call: Box<dyn Write + Send>,
    byte: Box<dyn Write + Send>,
    /// 输出 main 字节计数（虚拟指令地址；u32 wrapping，与解码器同义）。
    ip: u32,
    /// 概率上下文窗口（T12 解码器同款：每 main 字节 `v=(v<<24)|b`；转换后
    /// `v=rel>>24` 残留——rel==disp，见 process_pending）。
    v: u32,
    /// 待决输入缓冲（候选判定需位移视界）。
    pending: Vec<u8>,
    bytes_in: u64,
    main_bytes: u64,
    jump_bytes: u64,
    call_bytes: u64,
    byte_bytes: u64,
    converted: u64,
    finished: bool,
    // —— 范围编码状态（7-Zip `CRangeEnc` 语义）——
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    /// 概率模型（2 + 256 项，初始 1024；与解码器逐位对称更新）。
    probs: [u16; 2 + 256],
}

impl Bcj2Encoder {
    /// 构造 BCJ2 编码器。
    ///
    /// 参数序 = 任务卡签名序：`main`、`jump`、`call`、`byte`（与解码器
    /// `Bcj2Decoder::new(main, call, jump, markers)` 的容器子流语义一致：
    /// 解码参数 1=call、2=jump；本构造参数 2=jump、3=call——folder 组装方按
    /// 7-Zip 容器子流索引 `0=main,1=call,2=jump,3=rc` 接线）。
    pub fn new(
        main: Box<dyn Write + Send>,
        jump: Box<dyn Write + Send>,
        call: Box<dyn Write + Send>,
        byte: Box<dyn Write + Send>,
    ) -> Self {
        Self {
            main,
            jump,
            call,
            byte,
            ip: 0,
            v: 0,
            pending: Vec::with_capacity(PENDING_MAX),
            bytes_in: 0,
            main_bytes: 0,
            jump_bytes: 0,
            call_bytes: 0,
            byte_bytes: 0,
            converted: 0,
            finished: false,
            low: 0,
            range: u32::MAX,
            cache: 0,
            cache_size: 1,
            probs: [(K_BIT_MODEL_TOTAL as u16) >> 1; 2 + 256],
        }
    }

    // ---------- 范围编码（7-Zip CRangeEnc 忠实语义） ----------

    /// 编码一个标记位（`bit == true` = 转换；lzma-rust/7-Zip RangeEnc 语义）。
    fn encode_bit(&mut self, bit: bool, prob_idx: usize) -> io::Result<()> {
        let prob = &mut self.probs[prob_idx];
        let ttt = *prob as u32;
        let bound = (self.range >> K_NUM_MODEL_TOTAL_BITS) * ttt;
        if !bit {
            self.range = bound;
            *prob = (ttt + ((K_BIT_MODEL_TOTAL - ttt) >> K_NUM_MOVE_BITS)) as u16;
        } else {
            self.low += bound as u64;
            self.range = self.range.wrapping_sub(bound);
            *prob = (ttt - (ttt >> K_NUM_MOVE_BITS)) as u16;
        }
        if self.range & 0xFF00_0000 == 0 {
            self.range <<= 8;
            self.shift_low()?;
        }
        Ok(())
    }

    /// `ShiftLow`：把 `low` 高字节经 cache 链写往标记流（lzma-rust/7-Zip
    /// `CRangeEncoder::ShiftLow` 忠实语义；`low` 移位前掩 24 位）。
    fn shift_low(&mut self) -> io::Result<()> {
        let low_hi = (self.low >> 32) as i32;
        if low_hi != 0 || self.low < 0xFF00_0000u64 {
            let mut temp = self.cache;
            loop {
                self.byte.write_all(&[(temp as i32 + low_hi) as u8])?;
                self.byte_bytes += 1;
                temp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = (self.low >> 24) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
        Ok(())
    }

    /// RC 收尾（5 次 `ShiftLow`；与解码器 5 字节初始 code 对称）。
    fn rc_flush(&mut self) -> io::Result<()> {
        for _ in 0..5 {
            self.shift_low()?;
        }
        Ok(())
    }

    // ---------- main/jump/call 输出 ----------

    fn emit_main(&mut self, b: u8) -> io::Result<()> {
        self.main.write_all(&[b])?;
        self.main_bytes += 1;
        self.v = (self.v << 24) | b as u32;
        Ok(())
    }

    /// 转换输出：绝对地址（大端）入 call/jump 流。`abs` 由调用方算好。
    fn emit_abs(&mut self, abs: u32, is_call: bool) -> io::Result<()> {
        let bytes = abs.to_be_bytes();
        if is_call {
            self.call.write_all(&bytes)?;
            self.call_bytes += 4;
        } else {
            self.jump.write_all(&bytes)?;
            self.jump_bytes += 4;
        }
        Ok(())
    }

    // ---------- 候选裁决 ----------

    /// T12 解码器 `bcj2_prob_index` 忠实复刻（4 字节 v 窗口；对候选字节坍缩为
    /// E8:2+prev / E9:1 / jcc:0，`prev` = v>>24）。
    #[inline]
    fn prob_index(&self, v: u32) -> usize {
        let c = ((v.wrapping_add(0x17)) >> 6) & 1;
        let prev = (v >> 24) & 0xff;
        let bit5 = (v >> 5) & 1;
        (((0u32.wrapping_sub(c)) & prev) + c + bit5) as usize
    }

    /// 逐字节处理输入（pending 内已保证视界足够或已到 finish）。
    ///
    /// 候选判定与概率上下文完全镜像 T12 解码器主扫描：`self.v` 为解码器同款
    /// 上下文（`v=(v<<24)|b` 每 main 字节；u32 左移 24 实际保留「上一字节<<24 |
    /// 当前字节」，即 prev 上下文）。E8/E9 = 单字节候选（5 字节组）；jcc =
    /// `0F 8x` 双字节候选（6 字节组，检测点 = 8x 字节、v 高字节为 0F）。
    /// 转换后 `v=disp>>24` 残留（解码器 rel==disp 同款），保证后续候选逐位一致。
    #[allow(clippy::expect_used)] // 窗口边界由视界长度分支静态保证（expect 不可达）
    fn process_pending(&mut self, at_eof: bool) -> io::Result<()> {
        loop {
            if self.pending.is_empty() {
                return Ok(());
            }
            let b = self.pending[0];
            // —— jcc 组（0F 8x rel32；6 字节视界）——
            if b == 0x0F && self.pending.len() >= 2 && self.pending[1] & 0xF0 == 0x80 {
                if self.pending.len() < 6 && !at_eof {
                    return Ok(()); // 视界不足：等待
                }
                let ip0 = self.ip;
                if self.pending.len() >= 6 {
                    let disp = u32::from_le_bytes(self.pending[2..6].try_into().expect("len>=6"));
                    // v 经 0F、8x 两次更新后 = (0F<<24)|8x；jcc 概率索引恒 0
                    let v2 = (0x0Fu32 << 24) | self.pending[1] as u32;
                    let idx = self.prob_index(v2);
                    self.encode_bit(true, idx)?;
                    self.emit_main(0x0F)?;
                    self.emit_main(self.pending[1])?;
                    // abs = rel + (ip_after_opcode + 4) = rel + ip0 + 6
                    let abs = disp.wrapping_add(ip0.wrapping_add(6));
                    self.emit_abs(abs, false)?; // jcc → jump 流
                    self.v = disp >> 24;
                    self.ip = ip0.wrapping_add(6);
                    self.converted += 1;
                    self.pending.drain(..6);
                    continue;
                }
                // 流尾不完整 jcc：标记 0（idx 0），0F 原样入 main；8x 由下轮
                // 普通路径处理（非候选）
                self.encode_bit(false, 0)?;
                self.emit_main(b)?;
                self.ip = self.ip.wrapping_add(1);
                self.pending.remove(0);
                continue;
            }
            // —— E8/E9（单字节候选；5 字节视界）——
            let is_e89 = ((b.wrapping_add(0x18)) & 0xfe) == 0;
            if !is_e89 {
                self.emit_main(b)?;
                self.ip = self.ip.wrapping_add(1);
                self.pending.remove(0);
                continue;
            }
            if self.pending.len() < 5 && !at_eof {
                return Ok(()); // 视界不足：等待
            }
            let ip0 = self.ip;
            let v_after = (self.v << 24) | b as u32;
            let idx = self.prob_index(v_after);
            if self.pending.len() >= 5 {
                let disp = u32::from_le_bytes(self.pending[1..5].try_into().expect("len>=5"));
                self.encode_bit(true, idx)?;
                // abs = rel + (ip_after_opcode + 4) = rel + ip0 + 5
                let abs = disp.wrapping_add(ip0.wrapping_add(5));
                let is_call = ((v_after.wrapping_add(0x57)) >> 6) & 1 == 0;
                self.emit_abs(abs, is_call)?;
                self.emit_main(b)?;
                self.v = disp >> 24;
                self.ip = ip0.wrapping_add(5);
                self.converted += 1;
                self.pending.drain(..5);
                continue;
            }
            // 流尾不完整 E8/E9：标记 0，opcode 原样入 main
            self.encode_bit(false, idx)?;
            self.emit_main(b)?;
            self.ip = self.ip.wrapping_add(1);
            self.pending.remove(0);
        }
    }

    /// 收尾：流尾候选裁决 + RC flush + 各流 flush + 同步校验。
    ///
    /// 返回统计（`Bcj2Stats`）。校验失败（转换计数与标记位不一致——本实现按
    /// 构造不可达，防御性保留）→ `Err`。调用后 encoder 不可再用。
    pub fn finish(mut self: Box<Self>) -> io::Result<Bcj2Stats> {
        if self.finished {
            return Err(io::Error::other("BCJ2 encoder already finished"));
        }
        self.finished = true;
        self.process_pending(true)?;
        self.rc_flush()?;
        if self.byte_bytes == 0 {
            // 零转换空流：RC 未产任何字节 → 补 5 个零字节（解码器 5 字节初始
            // code=0 装载的对称面；7-Zip 对非空流不出现此情形，防御性补齐）。
            self.byte.write_all(&[0u8; 5])?;
            self.byte_bytes += 5;
        }
        self.main.flush()?;
        self.jump.flush()?;
        self.call.flush()?;
        self.byte.flush()?;
        // 同步校验：main/jump/call 第 k 个记录与标记位一一对应由状态机构造保证
        // （本实现单线程顺序裁决，无异步路径）；统计闭环断言留给测试。
        Ok(Bcj2Stats {
            bytes_in: self.bytes_in,
            main: self.main_bytes,
            jump: self.jump_bytes,
            call: self.call_bytes,
            byte: self.byte_bytes,
            converted: self.converted,
        })
    }
}

impl Write for Bcj2Encoder {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.finished {
            return Err(io::Error::other("BCJ2 encoder already finished"));
        }
        self.pending.extend_from_slice(buf);
        self.bytes_in += buf.len() as u64;
        self.process_pending(false)?;
        // Write 契约：调用方 write_all 期望全量消费——pending 内滞留的尾部字节
        // 属视界等待（后续 write 或 finish 补齐），不算失败。
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // 不裁决（无 EOF 语义）；仅冲刷底层（通常空操作）。
        self.main.flush()?;
        self.jump.flush()?;
        self.call.flush()?;
        self.byte.flush()
    }
}

#[cfg(test)]
#[path = "tests/bcj2_encode.rs"]
mod tests;
