//! 密码三通道优先级链（02 §10.2 图；`PASSWORD_MAX_RETRIES=3`，附录 B.2）。
//!
//! # 优先级链（高 → 低）
//!
//! 1. `--password-env VAR`：读 `env(VAR)`；变量缺失 → 提交前 HZ-5003 即时
//!    退出（§3.2 C 节点），**不进任务循环**（[`PasswordResolver::preflight`]）；
//! 2. `HZIP_PASSWORD`：取值应答；错误密码反馈后**不再重试**（02 §10.2 E 节点
//!    原文「失败后提示不再重试」）；
//! 3. stdin 是 TTY：rpassword 隐藏输入；错误密码（`WrongPassword` reason 重问）
//!    重试计数至 [`PASSWORD_MAX_RETRIES`]=3 → 返回 `None`（主循环取消任务）；
//! 4. stdin 非 TTY：读 stdin 一行（trim 尾换行）；读取失败/EOF → 以**空密码**
//!    应答（普通条目无需密码即正确继续；真加密条目收敛 HZ-3101 退出 2——
//!    02 §10.2 H「失败即 3101 不挂起」的请求-应答等价表达；
//!    TC-CLI-008/012 看门狗约束：无 TTY 时永不进入交互等待）。
//!
//! 密码经 `Zeroizing<String>` 持有（02 §1.2）；日志不打印明文。

use std::io::{BufRead, IsTerminal as _};
use std::sync::atomic::Ordering;

use hzip_core::jobs::{JobRequest, JobResponse};
use hzip_error::HzipError;
use zeroize::Zeroizing;

/// 密码重试上限（02 附录 B.2 `PASSWORD_MAX_RETRIES=3`；定义位置 hzip-cli）。
pub const PASSWORD_MAX_RETRIES: u8 = 3;

/// stdin 非 TTY 提示（一次；跨重试去重）。
#[derive(Debug, Default)]
pub struct StdinIsNotTty {
    warned: std::sync::atomic::AtomicBool,
}

impl StdinIsNotTty {
    fn warn_once(&self, msg: &str) {
        if !self.warned.swap(true, Ordering::Relaxed) {
            eprintln!("{msg}");
        }
    }
}

/// 空密码拒绝（设密语义：压缩加密不接受空串；HZ-5003）。
fn checked(pw: Zeroizing<String>) -> Result<Zeroizing<String>, HzipError> {
    if pw.is_empty() {
        return Err(HzipError::Unsupported {
            what: "hz-5003 密码为空（压缩加密不接受空密码）".into(),
        });
    }
    Ok(pw)
}

/// 三通道解析器（每任务一个；`resolve` 由主循环应答线程调用）。
#[derive(Debug)]
pub struct PasswordResolver {
    /// `--password-env` 指定的环境变量名
    password_env: Option<String>,
    /// `HZIP_PASSWORD` 通道命中标记（该通道不重试：一旦用过不再读新值）
    env_used: bool,
    /// 非 TTY 降级提示去重
    no_tty_hint: StdinIsNotTty,
}

impl PasswordResolver {
    /// 新建（`password_env` = CLI 的 `--password-env`）。
    pub fn new(password_env: Option<String>) -> Self {
        Self {
            password_env,
            env_used: false,
            no_tty_hint: StdinIsNotTty::default(),
        }
    }

    /// 提交前预检：`--password-env` 指定但变量缺失 → `Err`（HZ-5003 语义，
    /// 02 §10.2 C 节点即时返回，不进任务循环）。
    pub fn preflight(&self) -> Result<(), HzipError> {
        if let Some(var) = &self.password_env
            && std::env::var_os(var).is_none()
        {
            return Err(HzipError::Unsupported {
                what: format!("hz-5003 password-env 缺失：环境变量 {var} 未设置").into(),
            });
        }
        Ok(())
    }

    /// 压缩侧一次性取密码（设计稿 11 §3.6；与 `resolve` 的条目应答语义区分）：
    /// ① `--password-env` 指定变量 ② `HZIP_PASSWORD` ③ TTY 隐藏输入**两次**
    /// （不一致重试至 3 次；空密码拒绝）。非 TTY 无可用通道 → HZ-5003 不挂起。
    pub fn resolve_setup_password(&self) -> Result<Zeroizing<String>, HzipError> {
        if let Some(var) = &self.password_env
            && let Some(v) = std::env::var_os(var)
        {
            return checked(Zeroizing::new(v.to_string_lossy().into_owned()));
        }
        if let Some(v) = std::env::var_os("HZIP_PASSWORD") {
            return checked(Zeroizing::new(v.to_string_lossy().into_owned()));
        }
        if std::io::stdin().is_terminal() {
            for _ in 0..PASSWORD_MAX_RETRIES {
                let a = match rpassword::prompt_password("设置压缩密码: ") {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                        return Err(HzipError::Unsupported {
                            what: "hz-5003 已取消密码输入".into(),
                        });
                    }
                    Err(e) => return Err(HzipError::from(e)),
                };
                let b = match rpassword::prompt_password("再次输入密码（防误触）: ") {
                    Ok(s) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                        return Err(HzipError::Unsupported {
                            what: "hz-5003 已取消密码输入".into(),
                        });
                    }
                    Err(e) => return Err(HzipError::from(e)),
                };
                if a != b {
                    eprintln!("两次输入不一致，请重试（最多 3 次）");
                    continue;
                }
                return checked(Zeroizing::new(a));
            }
            return Err(HzipError::Unsupported {
                what: "hz-5003 两次密码输入均不一致（超过重试上限）".into(),
            });
        }
        Err(HzipError::Unsupported {
            what: "hz-5003 压缩加密需要密码：非交互终端请给 --password-env 或 HZIP_PASSWORD".into(),
        })
    }

    /// 主循环应答 `JobRequest::Password`；返回应答或 `None`（表示应取消任务——
    /// TTY 通道重试超限，02 §4.4）。
    pub fn resolve(&mut self, req: &JobRequest) -> Result<Option<JobResponse>, HzipError> {
        // ① --password-env（preflight 已保证存在；重试也沿用同值——不重复读）
        if let Some(var) = &self.password_env
            && let Some(v) = std::env::var_os(var)
        {
            let pw = v.to_string_lossy().into_owned();
            return Ok(Some(JobResponse::Password(Zeroizing::new(pw))));
        }
        // ② HZIP_PASSWORD：取值应答；使用后不再重试（失败后不再读 env——02 §10.2 E）
        if !self.env_used
            && let Some(v) = std::env::var_os("HZIP_PASSWORD")
        {
            self.env_used = true;
            let pw = v.to_string_lossy().into_owned();
            return Ok(Some(JobResponse::Password(Zeroizing::new(pw))));
        }
        // ③④ stdin 通道（TTY 提示 / 非 TTY 一行）
        self.resolve_stdin(req)
    }

    /// stdin 通道：TTY → rpassword（失败重试至 3）；非 TTY → read_line 一行
    /// （EOF → 空密码应答，见模块文档 ④）。
    fn resolve_stdin(&mut self, req: &JobRequest) -> Result<Option<JobResponse>, HzipError> {
        let reason_wrong = matches!(
            req,
            JobRequest::Password {
                reason: hzip_core::jobs::PasswordReason::WrongPassword,
                ..
            }
        );
        if std::io::stdin().is_terminal() {
            // 提示文案：重试时标注（TTY 通道计数收敛 PASSWORD_MAX_RETRIES 由
            // 本函数调用方 retries 计数——core 侧超限会以 Password 终态 Failed
            // 落账；此处每次重问都重新提示（终端交互面）。
            let hint = if reason_wrong {
                "密码错误，请重试（最多 3 次）: "
            } else {
                "输入密码: "
            };
            match rpassword::prompt_password(hint) {
                Ok(pw) => Ok(Some(JobResponse::Password(Zeroizing::new(pw)))),
                Err(e) => {
                    // 终端读取失败（用户中断等）→ 不挂起：按取消处理
                    if e.kind() == std::io::ErrorKind::Interrupted {
                        return Ok(None);
                    }
                    Err(HzipError::from(e))
                }
            }
        } else {
            // ④ 非 TTY：stdin 一行（trim 尾换行）；EOF → 空密码应答
            // （对普通条目探测性询问静默通过；真加密条目收敛 3101 不挂起）
            if reason_wrong {
                self.no_tty_hint
                    .warn_once("警告: 密码错误且 stdin 非交互终端；将按空密码继续（HZ-3101）");
            }
            let mut line = String::new();
            let n = std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map_err(|e| HzipError::Io {
                    source: e,
                    path: None,
                })?;
            if n == 0 {
                // EOF：空密码应答——fmt 层对普通条目（探测性询问）以无密码
                // 继续；真加密条目校验失败收敛 3101（退出 2，不挂起）
                return Ok(Some(JobResponse::Password(Zeroizing::new(String::new()))));
            }
            // trim 尾换行（\n / \r\n），保留其余
            while line.ends_with('\n') || line.ends_with('\r') {
                line.pop();
            }
            Ok(Some(JobResponse::Password(Zeroizing::new(line))))
        }
    }
}

#[cfg(test)]
#[path = "tests/password.rs"]
mod tests;
