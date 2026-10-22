use std::io::{
    Read,
    Write,
};

use portable_pty::{
    CommandBuilder,
    MasterPty,
    PtySize,
    native_pty_system,
};

use super::{
    TerminalBackend,
    TerminalOutput,
};
use crate::handle::TerminalError;

/// 本地 PTY 后端（原 pty.rs spawn_pty 的 openpty/spawn/read 泵下沉于此）。
pub struct PtyBackend {
    command: CommandBuilder,
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
}

impl PtyBackend {
    pub fn new(command: CommandBuilder) -> Self {
        Self {
            command,
            master: None,
            writer: None,
        }
    }
}

impl TerminalBackend for PtyBackend {
    fn start(&mut self, output: TerminalOutput) -> Result<(), TerminalError> {
        let pair = native_pty_system()
            .openpty(PtySize::default())
            .map_err(|e| TerminalError::StartError(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| TerminalError::StartError(e.to_string()))?;
        let mut child = pair
            .slave
            .spawn_command(self.command.clone())
            .map_err(|e| TerminalError::StartError(e.to_string()))?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| TerminalError::StartError(e.to_string()))?;
        // 全部就绪后才落自身状态：任一步失败都直接返回，writer/master 保持 None，
        // 不会留下半初始化的后端（失败路径上没有孤儿句柄，master 随 pair 一起 drop）。
        self.writer = Some(writer);
        self.master = Some(pair.master);
        std::thread::spawn(move || {
            // 单 chunk 上限 64 KiB，与 OUTPUT_CHANNEL_CAPACITY 共同决定待解析字节的
            // 硬上限（256 + 1 个发送者保证槽位 = 257 chunk ≈ 16.4 MiB）；队列满时
            // output.write 会阻塞，背压经 pty 读缓冲传给子进程（子进程写满 → 阻塞子进程）。
            let mut buf = [0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if output.write(buf[..n].to_vec()).is_err() {
                            break; // 终端已销毁（读取端 drop），读线程没有存在意义
                        }
                    }
                    Err(e) => {
                        // 读端 EOF（子进程退出/master 被 drop）与真实 I/O 错误都走到这。
                        // 原来这里静默 break，失败原因不可见；记录一次再退。
                        eprintln!("freya-terminal: pty reader stopped: {e}");
                        break;
                    }
                }
            }
            let _ = child.wait(); // 收割，防僵尸/孤儿
        });
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TerminalError> {
        let writer = self.writer.as_mut().ok_or(TerminalError::Closed)?;
        writer.write_all(data)?;
        writer.flush()?;
        Ok(())
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        if let Some(master) = &self.master {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{
        Duration,
        Instant,
    };

    use futures_channel::mpsc;

    use super::*;
    use crate::backends::{
        OUTPUT_CHANNEL_CAPACITY,
        TerminalBackend,
        TerminalOutput,
    };

    #[cfg(windows)]
    fn echo_command(marker: &str) -> portable_pty::CommandBuilder {
        let mut cmd = portable_pty::CommandBuilder::new("cmd.exe");
        cmd.args(["/c", "echo", marker]);
        cmd
    }

    #[cfg(not(windows))]
    fn echo_command(marker: &str) -> portable_pty::CommandBuilder {
        let mut cmd = portable_pty::CommandBuilder::new("sh");
        cmd.args(["-c", &format!("printf %s {marker}")]);
        cmd
    }

    #[test]
    fn pty_backend_streams_child_output() {
        let marker = "putala-pty-backend-ok";
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(OUTPUT_CHANNEL_CAPACITY);
        let output = TerminalOutput::new_for_test(tx);
        let mut backend = PtyBackend::new(echo_command(marker));
        backend.start(output).expect("start pty backend");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut collected = Vec::new();
        // Windows ConPTY 在建立 pty 时会先发一个 DSR 光标位置查询（`ESC[6n`），并且在收
        // 到光标位置报告之前不转发子进程的任何输出。真实链路里这条回写由 alacritty 的
        // `Term::device_status` 经 `EventProxy::PtyWrite` 完成；本测试单独驱动
        // `PtyBackend`（没有终端引擎），所以必须自己扮演这个角色，否则 Windows 上
        // 永远等不到子进程输出。DSR 可能被拆包，因此在累积缓冲上做滑动窗口匹配。
        #[cfg(windows)]
        let mut dsr_answered = false;
        while Instant::now() < deadline {
            // `try_recv` 是 futures-channel 0.3.34 起 `try_next` 的非 deprecated 替身，
            // 语义一致（非阻塞取一条，没有就返回 Err）。
            if let Ok(bytes) = rx.try_recv() {
                collected.extend_from_slice(&bytes);
                #[cfg(windows)]
                if !dsr_answered && collected.windows(4).any(|w| w == b"\x1b[6n") {
                    dsr_answered = true;
                    let _ = backend.write(b"\x1b[1;1R");
                }
                if String::from_utf8_lossy(&collected).contains(marker) {
                    return; // PASS
                }
            } else {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        panic!(
            "did not receive marker within 10s; got {:?}",
            String::from_utf8_lossy(&collected)
        );
    }

    #[test]
    fn pty_backend_resize_does_not_panic() {
        let (tx, _rx) = mpsc::channel::<Vec<u8>>(OUTPUT_CHANNEL_CAPACITY);
        let mut backend = PtyBackend::new(echo_command("resize-noop"));
        backend.start(TerminalOutput::new_for_test(tx)).unwrap();
        backend.resize(40, 120); // 只要求不 panic、尺寸类型正确
        // `echo` 随时可能已经退出，写入失败（Broken pipe / Closed）是正常竞态，不当断言。
        let _ = backend.write(b"exit\r");
    }

    #[test]
    fn terminal_output_write_blocks_when_full() {
        // 覆盖 TerminalOutput 的背压契约（评审 Important 1 的回归测试）：有界通道满时
        // 后续 chunk 必须阻塞在读线程里，而不是把队列撑大——队列不排空，pty 读缓冲就
        // 不排空，子进程最终被自己的写调用阻塞（OOM 路径被堵死）。
        // 只测通道语义、不起子进程，因此与平台无关、无竞态。
        //
        // futures-channel 的有界通道容量是 `buffer + num_senders`（每个发送者自带一个
        // 保证槽位）。这里 buffer=1 且只有 1 个持久发送者：前两笔必然入队，从第三笔起
        // 才真正阻塞在 poll_ready 上。
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(1);
        let output = TerminalOutput::new_for_test(tx);
        output.write(vec![0u8; 8]).expect("first chunk fits");
        output
            .write(vec![1u8; 8])
            .expect("second chunk fits (sender's own slot)");
        // 整块 output 移入线程：发送者必须常驻，park 状态才能跨次 write 生效。
        let writer = std::thread::spawn(move || output.write(vec![2u8; 8]));
        // 队列已满：写入方必须还卡在 write 内。留 100ms 让写入线程起跑，若语义回退成
        // unbounded（或发送者被换成一次性 clone），它早就该结束了。
        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !writer.is_finished(),
            "bounded output channel must block when full instead of growing"
        );
        // 消费出一个空位，被阻塞的写入才能完成。
        assert_eq!(rx.try_recv().unwrap(), vec![0u8; 8]);
        writer.join().expect("writer thread").expect("third chunk");
        assert_eq!(rx.try_recv().unwrap(), vec![1u8; 8]);
        assert_eq!(rx.try_recv().unwrap(), vec![2u8; 8]);
    }
}
