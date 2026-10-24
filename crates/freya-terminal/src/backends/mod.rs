//! Terminal I/O 后端抽象（0.4.3 backport，蓝本 = 上游 0.5 crates/freya-terminal/src/backends/）。
//! 只 backport trait/通道/PTY 实现与 handle 接线模式；不搬上游的 rio-vt 引擎。

pub mod pipe;
pub mod pty;

use std::{
    future::poll_fn,
    sync::Mutex,
    task::Poll,
};

use futures_channel::{
    mpsc,
    mpsc::Sender,
};
use futures_lite::future::block_on;

use crate::handle::TerminalError;

/// 终端字节流后端（本地 PTY、SSH 通道等）。
///
/// Drop 契约——「Drop 时必须停止 start() 启动的一切运行物」——的平台实况：
///
/// - **Windows**：portable-pty 的 conpty 实现里 master 与 slave 共享同一个内部句柄
///   （`Arc` 引用计数），丢掉 master 会调 `ClosePseudoConsole`，读端随即 EOF，子进程
///   随伪控制台关闭而终止。所以像 `PtyBackend` 这样直接丢 `master`/`writer` 就能停下后端。
/// - **Unix**：`try_clone_reader` 返回的是 dup 出来的 fd，只丢 master 不会让读端 EOF。
///   契约仍成立，但依赖 `PtyBackend` 的收尾顺序：读线程退出（EOF 或发送失败）后
///   `child.wait()` 收割子进程；后端先于读线程被 drop 时，子进程靠 pty 句柄关闭
///   引发的 SIGHUP 收尾。实现新后端时不要假设「丢 master 在所有平台都会自动停进程」，
///   按平台显式停。
pub trait TerminalBackend {
    /// 启动后端并把输出字节写入 output。
    fn start(&mut self, output: TerminalOutput) -> Result<(), TerminalError>;
    /// 写入一段输入字节（键盘/粘贴/OSC 回写）。
    fn write(&mut self, data: &[u8]) -> Result<(), TerminalError>;
    /// 行列变化；实现尽力而为，失败不抛。
    fn resize(&mut self, rows: u16, cols: u16);
}

/// 输出通道容量，按 chunk 计。
///
/// futures-channel 的有界通道实际容量是 `buffer + num_senders`（每个发送者占一个
/// 保证槽位）；读线程全程只有 1 个持久发送者，所以待解析字节的硬上限是
/// `OUTPUT_CHANNEL_CAPACITY + 1` 个 chunk。按 64 KiB 的单 chunk 上限
/// （见 `pty.rs` 的读缓冲）折算 ≈ 16.4 MiB。
pub(crate) const OUTPUT_CHANNEL_CAPACITY: usize = 256;

/// 后端 → 终端解析器的输出通道。
///
/// 有界 + 阻塞，两者缺一不可：
///
/// - **有界**：容量 `OUTPUT_CHANNEL_CAPACITY`（256）个 chunk。解析侧（handle 的泵任务）
///   消费慢于后端产出时，待解析字节最多堆到 16 MiB 级，不会无界增长。
/// - **阻塞**：队列满时 [`TerminalOutput::write`] 阻塞当前线程直到有空位。链条是：
///   读线程停在 write → pty 读缓冲不被排空 → 子进程的写调用阻塞。这恢复了 0.4.3
///   原有的「子进程写满 → 阻塞子进程」背压（原来读与解析在同一个任务里串行，
///   天然不可能积压）。
///
/// 之所以不像上游 0.5 那样用 unbounded：读线程与解析泵已经解耦成两个线程，
/// unbounded 队列在 `yes`、大构建日志、`type big.txt` 这类「产出快于解析」的场景下
/// 会无限增长，直到 OOM。也**不能**用「满了就合并/丢弃 chunk」替代阻塞：终端是
/// 字节流，丢一个 chunk 就会花屏，阻塞是唯一可接受的背压语义。
///
/// 读取端（泵任务）一旦结束，写端的发送返回 `Err`，后端据此停掉自己启动的进程/线程。
pub struct TerminalOutput {
    /// 用 `Mutex` 包一层，是为了在 `&self` 的 `write` 里拿到 `&mut Sender`
    /// （`poll_ready` 要 `&mut`），同时不把 `Send + Sync` 收窄（0.4.3 的
    /// `UnboundedSender` 本来就是 `Send + Sync`，Task 6 的 SSH 后端可能跨线程用）。
    ///
    /// **发送者必须常驻、不能每次调用换 clone**：futures-channel 的背压是靠「发送者
    /// 把自己 park 住、下次 `poll_ready` 返回 `Pending`」实现的，park 状态挂在具体
    /// 的 `Sender` 值上。每次 `write` 都 clone 一个新发送者，park 状态随 clone 一起
    /// 丢弃，`poll_ready` 永远返回 `Ready`，队列就退化成无界了。
    pub(crate) sender: Mutex<Sender<Vec<u8>>>,
}

/// `Clone` 得到的是共享同一条队列的新发送者句柄。注意：副本有自己独立的 park 状态，
/// 拿副本当背压发送者用是不可靠的（见 `sender` 字段注释）；这里保留 `Clone` 只是为
/// 了不改变既有 API 形状。
impl Clone for TerminalOutput {
    fn clone(&self) -> Self {
        Self {
            sender: Mutex::new(self.sender.lock().unwrap().clone()),
        }
    }
}

impl TerminalOutput {
    /// 建立一对「后端 → 解析器」的有界通道，容量 `OUTPUT_CHANNEL_CAPACITY` 个 chunk。
    pub(crate) fn channel() -> (Self, mpsc::Receiver<Vec<u8>>) {
        let (sender, receiver) = mpsc::channel(OUTPUT_CHANNEL_CAPACITY);
        (
            Self {
                sender: Mutex::new(sender),
            },
            receiver,
        )
    }

    /// 把一段输出字节交给解析器。队列满时阻塞（背压语义，不丢不并）；
    /// 读取端已销毁时返回 [`TerminalError::Closed`]，调用方应停掉自己的读取循环。
    pub fn write(&self, bytes: Vec<u8>) -> Result<(), TerminalError> {
        // `futures_channel::mpsc::Sender` 只提供异步的 poll_ready/start_send，没有阻塞式
        // 发送；用 futures-lite（本 crate 已在用）的 block_on 把「等空位」落到线程睡眠上。
        // 持锁跨越 block_on 是安全的：写方只有读线程一个，锁在此期间不会被别人抢。
        let mut sender = self.sender.lock().unwrap();
        let mut bytes = Some(bytes);
        block_on(poll_fn(|cx| match sender.poll_ready(cx) {
            Poll::Ready(Ok(())) => Poll::Ready(
                sender
                    .start_send(bytes.take().expect("polled after completion"))
                    .map_err(|_| TerminalError::Closed),
            ),
            Poll::Ready(Err(_)) => Poll::Ready(Err(TerminalError::Closed)),
            Poll::Pending => Poll::Pending,
        }))
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(sender: Sender<Vec<u8>>) -> Self {
        Self {
            sender: Mutex::new(sender),
        }
    }
}
