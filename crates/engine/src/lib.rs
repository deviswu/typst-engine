//! Typst 的实时增量编译引擎。UI 无关。
//!
//! - [`vfs`] —— 「路径 → 字节」的虚拟文件系统，支持内存覆盖磁盘
//! - [`world`] —— `typst::World` 实现，增量维护语法树、字体与包

mod path_util;

pub mod export;
pub mod vfs;
pub mod world;
