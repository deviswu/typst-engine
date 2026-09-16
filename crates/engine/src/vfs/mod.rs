//! 虚拟文件系统：路径 → 字节，支持内存覆盖磁盘。

mod access;
mod fs;
mod memory;
mod overlay;
mod system;

pub use access::*;
pub use fs::*;
pub use memory::*;
pub use overlay::*;
pub use system::*;
