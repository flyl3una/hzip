//! `IEnumExplorerCommand` 实现（子命令枚举）。
//!
//! 只需支持 `Next` 的批量读取与 `Reset`/`Clone`；shell 在构建 flyout 时按
//! `Next(1, ...)` 逐个取，故游标语义必须准确——多给或少给都会让菜单项
//! 与实际动作对不上（比菜单不出现更难排查）。

use std::cell::RefCell;
use std::rc::Rc;

use windows::core::{implement, Result, HRESULT};
use windows::Win32::Foundation::{E_NOTIMPL, S_FALSE, S_OK};
use windows::Win32::UI::Shell::{
    IEnumExplorerCommand, IEnumExplorerCommand_Impl, IExplorerCommand,
};

/// 命令枚举器（`Rc` + `RefCell` 即可：COM 对象不跨线程——
/// 注册表路径用 `ThreadingModel=Apartment`，稀疏包路径用 `STA` 代理）。
#[implement(IEnumExplorerCommand)]
pub struct HzipEnum {
    /// 全部命令（固定顺序）。
    items: Vec<IExplorerCommand>,
    /// 当前游标。
    cursor: Rc<RefCell<usize>>,
}

impl HzipEnum {
    /// 构造（游标归零）。
    pub fn new(items: Vec<IExplorerCommand>) -> Self {
        Self {
            items,
            cursor: Rc::new(RefCell::new(0)),
        }
    }
}

impl IEnumExplorerCommand_Impl for HzipEnum_Impl {
    /// 取 `celt` 个命令写入 `puicommand`；实际取到的个数写 `pceltfetched`。
    ///
    /// 返回值：取满 → `S_OK`；不足 → `S_FALSE`（COM 枚举约定）。
    fn Next(
        &self,
        celt: u32,
        puicommand: *mut Option<IExplorerCommand>,
        pceltfetched: *mut u32,
    ) -> HRESULT {
        crate::guard::guard_hr("HzipEnum::Next", || {
            let mut cursor = self.cursor.borrow_mut();
            if puicommand.is_null() {
                return windows::Win32::Foundation::E_POINTER;
            }
            let mut fetched: u32 = 0;
            while fetched < celt && *cursor < self.items.len() {
                let item = self.items[*cursor].clone();
                // SAFETY: 调用方保证 puicommand 指向 celt 个可写槽位
                unsafe { puicommand.add(fetched as usize).write(Some(item)) };
                *cursor += 1;
                fetched += 1;
            }
            if !pceltfetched.is_null() {
                // SAFETY: 出参非空校验通过
                unsafe { pceltfetched.write(fetched) };
            }
            if fetched == celt {
                S_OK
            } else {
                S_FALSE
            }
        })
    }

    fn Skip(&self, celt: u32) -> Result<()> {
        let mut cursor = self.cursor.borrow_mut();
        *cursor = (*cursor + celt as usize).min(self.items.len());
        Ok(())
    }

    fn Reset(&self) -> Result<()> {
        *self.cursor.borrow_mut() = 0;
        Ok(())
    }

    fn Clone(&self) -> Result<IEnumExplorerCommand> {
        // 直接构造 `HzipEnum`（`_Impl` 是 macro 生成的包装类型，无公开字段）
        let cloned = HzipEnum {
            items: self.items.clone(),
            cursor: Rc::new(RefCell::new(*self.cursor.borrow())),
        };
        Ok(IEnumExplorerCommand::from(cloned))
    }
}

/// 未使用（保留以说明为何 `E_NOTIMPL` 在此模块可见）。
#[allow(dead_code)]
const _NOT_IMPL: HRESULT = E_NOTIMPL;
