use std::{
    io::{
        Read,
        Write,
    },
    process::{
        Command,
        Stdio,
    },
    sync::{
        Arc,
        Mutex as StdMutex,
        mpsc::TrySendError,
    },
    time::{
        Duration,
        Instant,
    },
};

use portable_pty::CommandBuilder;

use super::{
    TerminalBackend,
    TerminalOutput,
};
use crate::handle::TerminalError;

/// The write-side enqueue budget (Task 43, I4): how long [`PipeBackend::write`]
/// waits for a free slot in the writer queue before giving up this packet.
/// The caller is the freya UI thread (the key handler), so the wait parks the
/// whole UI for at most this long -- the same 200 ms ruling the SSH backend's
/// write path made on 2026-10-23, now applied to the pipe transport too: the
/// queue plus the OS pipe hold several hundred KiB, which normal typing can
/// never fill; a full queue means the child stopped reading stdin.
const WRITE_WAIT_BUDGET: Duration = Duration::from_millis(200);

/// The retry cadence inside [`WRITE_WAIT_BUDGET`] (`try_send` is a cheap
/// capacity check; the sleep is what bounds the UI thread's park).
const WRITE_WAIT_POLL: Duration = Duration::from_millis(10);

/// The writer queue's capacity, in chunks (one `write` call = one chunk).
/// Bounded: together with the OS pipe buffer it is the hard cap on how much
/// unparsed input may sit between the UI and a child that is not reading.
const WRITE_QUEUE_CHUNKS: usize = 64;

/// The reaper's poll cadence while waiting for the child to exit (a `wait()`
/// would hold the child slot's lock for the child's whole lifetime and block
/// Drop's kill on the UI thread; `try_wait` polling keeps every lock hold
/// short).
const REAP_POLL: Duration = Duration::from_millis(50);

/// Raw-pipe backend: spawns `command` with stdin/stdout/stderr connected through
/// plain OS pipes, no pseudo-terminal in the middle (the degraded local-terminal
/// transport behind the app-side `PUTALA_CONPTY` gate).
///
/// Differences from [`super::pty::PtyBackend`] that follow from having no PTY:
/// - the child sees no terminal, so it does not emit terminal semantics (no
///   colors negotiated through TERM quirks, no cursor addressing, no echo of
///   typed input) -- its raw output bytes still reach the terminal viewport;
/// - `resize` is a no-op: pipes carry no window-size concept, so there is
///   nothing to deliver `rows`/`cols` to (the child keeps whatever size it
///   assumes by default);
/// - stderr has no ExtendedData-style side channel (that is a PTY/SSH notion);
///   it is merged into the same byte stream as stdout, with both flows stopped
///   before the child is reaped (see the reader threads in [`TerminalBackend::start`]).
///
/// Reaping and the Drop contract follow the `PtyBackend` shape (backends/mod.rs:
/// Drop must stop everything `start` launched): the child lives in a shared
/// slot the reaper thread `try_wait`-polls (no zombie), and Drop KILLS it
/// (Task 43, I4) -- a pipe has no master-handle close that would signal the
/// child (Unix pty close raises SIGHUP, ConPTY dies with the pseudo console),
/// and "drop the stdin writer and hope the child exits on EOF" leaves a
/// stdin-ignoring child running forever. `write` enqueues into a bounded
/// queue drained by a dedicated writer thread, so a child that stopped
/// reading stdin cannot park the UI thread on the full OS pipe (the same
/// 200 ms ruling the SSH backend's write path made).
pub struct PipeBackend {
    command: CommandBuilder,
    /// The writer queue's sending end (None before `start` / after teardown).
    writer: Option<std::sync::mpsc::SyncSender<Vec<u8>>>,
    /// The child in the slot shared with the reaper thread (Drop kills
    /// through it; the reaper reaps through it).
    child: Option<Arc<StdMutex<std::process::Child>>>,
}

impl PipeBackend {
    pub fn new(command: CommandBuilder) -> Self {
        Self {
            command,
            writer: None,
            child: None,
        }
    }
}

/// Translate the portable-pty `CommandBuilder` into a `std::process::Command`
/// with the same program/args/cwd and the caller-added env entries applied on
/// top of the inherited environment (the same effective environment the PTY
/// backends hand the child: base env plus `CommandBuilder::env` extras).
fn to_std_command(command: &CommandBuilder) -> Result<Command, TerminalError> {
    let argv = command.get_argv();
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| TerminalError::StartError("empty command argv".to_owned()))?;
    let mut cmd = Command::new(program);
    cmd.args(args);
    for (key, value) in command.iter_extra_env_as_str() {
        cmd.env(key, value);
    }
    if let Some(cwd) = command.get_cwd() {
        cmd.current_dir(cwd);
    }
    // A console child of a windowed (GUI) process would otherwise allocate a
    // visible console window the moment it starts. CREATE_NO_WINDOW gives the
    // child a hidden console instead; std's CommandExt carries the flag, so
    // this module needs no windows-sys surface of its own.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // windows_sys::Win32::System::Threading::CREATE_NO_WINDOW's value.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    Ok(cmd)
}

/// Drain one pipe into the shared output channel until EOF or a closed receiver.
fn pump<R: Read>(
    mut reader: R,
    output: &TerminalOutput,
    label: &'static str,
) -> Result<(), std::io::Error> {
    // Same 64 KiB chunk cap as the pty reader: with OUTPUT_CHANNEL_CAPACITY it
    // bounds the unparsed backlog at ~16.4 MiB and feeds the write-side
    // backpressure (a full queue blocks this thread, which blocks the child's
    // write when the pipe buffer fills up).
    let mut buf = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                if output.write(buf[..n].to_vec()).is_err() {
                    // Terminal destroyed (receiver dropped): no reason to keep reading.
                    return Ok(());
                }
            }
            Err(e) => {
                // Same visibility rule as the pty reader: log once, then stop.
                eprintln!("freya-terminal: pipe {label} reader stopped: {e}");
                return Err(e);
            }
        }
    }
}

impl TerminalBackend for PipeBackend {
    fn start(&mut self, output: TerminalOutput) -> Result<(), TerminalError> {
        let mut command = to_std_command(&self.command)?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| TerminalError::StartError(e.to_string()))?;
        // All piped, so the takes cannot fail in practice; if they ever do, kill
        // and reap the child so the failure path leaves no orphan behind, keeping
        // the same "no half-initialized backend" guarantee as the pty backend.
        let (stdin, stdout, stderr) =
            match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
                (Some(stdin), Some(stdout), Some(stderr)) => (stdin, stdout, stderr),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(TerminalError::StartError(
                        "child pipes unavailable after spawn".to_owned(),
                    ));
                }
            };
        // The child moves into the SHARED slot: the reaper thread reaps through
        // it and Drop kills through it (Task 43, I4). The reaper NEVER holds
        // the lock across a blocking wait -- a `wait()` there would hold it for
        // the child's whole lifetime and Drop's kill (the UI thread) would
        // block on it; `try_wait` polling keeps every hold short.
        let child_slot: Arc<StdMutex<std::process::Child>> = Arc::new(StdMutex::new(child));
        // The writer thread (Task 43, I4): the ONLY place `write_all` runs.
        // The UI thread enqueues into the bounded queue below; a child that
        // stopped reading stdin parks THIS thread on the full OS pipe, not the
        // UI. The queue's sender is stored as the backend's `writer` face; the
        // thread exits when the queue's senders are gone or the child's stdin
        // broke (its write errors), whichever comes first.
        let (writer_tx, writer_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(WRITE_QUEUE_CHUNKS);
        std::thread::spawn(move || {
            let mut stdin = stdin;
            for chunk in writer_rx {
                if stdin.write_all(&chunk).is_err() || stdin.flush().is_err() {
                    break; // the child is gone: nothing left to write
                }
            }
            // dropping stdin here closes the child's stdin pipe (the EOF
            // conventional CLI programs exit on -- the polite half of the
            // teardown; Drop's kill is the guaranteed half).
        });
        // stdout and stderr merge into the same output channel (the TerminalOutput
        // Clone shares one queue; two pipes interleave at chunk granularity, which
        // is the best a byte stream without PTY framing can do).
        let stdout_reader = std::thread::spawn({
            let output = output.clone();
            move || pump(stdout, &output, "stdout")
        });
        let stderr_reader = std::thread::spawn(move || pump(stderr, &output, "stderr"));
        // Reaper: both flows stopped first, so the poll-wait cannot race a
        // reader that still holds buffered bytes (same reader-tail reap shape
        // as pty.rs); then try_wait-poll until the child is reaped (no zombie,
        // no orphan) -- also covering the child that exited long ago while the
        // backend stayed alive.
        let reaper_slot = Arc::clone(&child_slot);
        std::thread::spawn(move || {
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            loop {
                let reaped = {
                    let mut slot = reaper_slot
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    matches!(slot.try_wait(), Ok(Some(_)) | Err(_))
                };
                if reaped {
                    return;
                }
                std::thread::sleep(REAP_POLL);
            }
        });
        // All pieces ready before self state lands (same order discipline as
        // pty.rs): any failure above leaves writer/child None and no orphan
        // process.
        self.writer = Some(writer_tx);
        self.child = Some(child_slot);
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TerminalError> {
        // Bounded enqueue (Task 43, I4): the old shape ran `write_all` on the
        // CALLING thread (the freya UI thread), so a child that stopped
        // reading stdin parked the whole app on the full OS pipe. The write
        // now hands the chunk to the writer thread through a bounded queue:
        // a transient backlog (a paste outrunning the child for a few ms) is
        // still delivered whole, and after the budget the packet is given up
        // with a readable error so the failure domain stays this one
        // terminal -- the same ruling the SSH backend's write path made
        // (2026-10-23), which this transport never had.
        let writer = self.writer.as_ref().ok_or(TerminalError::Closed)?;
        let deadline = Instant::now() + WRITE_WAIT_BUDGET;
        let mut chunk = data.to_vec();
        loop {
            match writer.try_send(chunk) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Full(returned)) => {
                    chunk = returned;
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(TerminalError::WriteError(format!(
                            "本地终端无响应：子进程 {}ms 内未读取输入（可能已停止响应），本次输入已放弃",
                            WRITE_WAIT_BUDGET.as_millis()
                        )));
                    }
                    std::thread::sleep(WRITE_WAIT_POLL.min(deadline - now));
                }
                // The writer thread is gone (the child's stdin broke): the
                // same Closed the old shape's broken-pipe error meant.
                Err(TrySendError::Disconnected(_)) => return Err(TerminalError::Closed),
            }
        }
    }

    fn resize(&mut self, _rows: u16, _cols: u16) {
        // No-op by design: pipes carry no window size (see the struct docs).
    }
}

impl Drop for PipeBackend {
    fn drop(&mut self) {
        // Task 43, I4 -- the Drop contract (backends/mod.rs): stop everything
        // `start` launched.
        // 1. Kill the child. A pipe has no master-handle close that would
        //    signal it (the pty backends' automatic path), and relying on the
        //    stdin EOF alone leaves a stdin-ignoring child running forever.
        //    kill() is a quick syscall; the lock is held only for it, and the
        //    reaper's try_wait polling never holds it long either.
        // 2. Drop the queue's sender: the writer thread finishes its current
        //    (possibly parked) blocking write -- kill above broke the pipe, so
        //    that write errors out at once -- and exits.
        // 3. The reaper (never joined from here: the UI thread must not wait
        //    on it) observes the killed child: readers hit EOF, the join
        //    returns, try_wait reaps. No zombie, no orphan, no UI block.
        if let Some(child) = self.child.take() {
            let _ = child
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .kill();
        }
        self.writer = None;
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
        pty::PtyBackend,
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

    /// True once `collected` contains the marker text.
    fn marker_seen(collected: &[u8], marker: &str) -> bool {
        String::from_utf8_lossy(collected).contains(marker)
    }

    /// The DSR dance from the pty test, shared by the cross-backend regression:
    /// Windows ConPTY withholds child output until the `ESC[6n` cursor query it
    /// sent at open time is answered; without a terminal engine driving
    /// `EventProxy::PtyWrite`, the test must answer it itself (sliding window,
    /// the query may arrive split across chunks).
    fn drive_windows_conpty(backend: &mut PtyBackend, collected: &[u8], dsr_answered: &mut bool) {
        if !*dsr_answered && collected.windows(4).any(|w| w == b"\x1b[6n") {
            *dsr_answered = true;
            let _ = backend.write(b"\x1b[1;1R");
        }
    }

    /// The pipe transport delivers the child's stdout bytes to the output
    /// channel: the same marker the pty test uses comes back through
    /// `TerminalOutput` (the pipe half of the A1 gate).
    #[test]
    fn pipe_backend_streams_child_output() {
        let marker = "putala-pipe-backend-ok";
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(OUTPUT_CHANNEL_CAPACITY);
        let output = TerminalOutput::new_for_test(tx);
        let mut backend = PipeBackend::new(echo_command(marker));
        backend.start(output).expect("start pipe backend");
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut collected = Vec::new();
        // No DSR dance here: pipes carry raw bytes, no ConPTY gate in between.
        while Instant::now() < deadline {
            if let Ok(bytes) = rx.try_recv() {
                collected.extend_from_slice(&bytes);
                if marker_seen(&collected, marker) {
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

    /// The A1 regression net: the same child command's marker must arrive
    /// through BOTH local transports. The original 07-proposal finding was
    /// "the local ConPTY silently drops child output"; this test pins the
    /// two-transport contract so a future transport regression on either side
    /// turns red here instead of in front of a user.
    #[test]
    fn same_marker_comes_back_through_pty_and_pipe_backends() {
        let marker = "putala-both-backends-ok";
        let deadline = Instant::now() + Duration::from_secs(10);

        // -- the ConPTY half (DSR must be answered, see drive_windows_conpty).
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(OUTPUT_CHANNEL_CAPACITY);
        let mut pty = PtyBackend::new(echo_command(marker));
        pty.start(TerminalOutput::new_for_test(tx))
            .expect("start pty backend");
        let mut pty_collected = Vec::new();
        #[cfg(windows)]
        let mut dsr_answered = false;
        while Instant::now() < deadline && !marker_seen(&pty_collected, marker) {
            match rx.try_recv() {
                Ok(bytes) => {
                    pty_collected.extend_from_slice(&bytes);
                    #[cfg(windows)]
                    drive_windows_conpty(&mut pty, &pty_collected, &mut dsr_answered);
                }
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        assert!(
            marker_seen(&pty_collected, marker),
            "PtyBackend (ConPTY) dropped the child marker: {:?}",
            String::from_utf8_lossy(&pty_collected)
        );

        // -- the pipe half.
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(OUTPUT_CHANNEL_CAPACITY);
        let mut pipe = PipeBackend::new(echo_command(marker));
        pipe.start(TerminalOutput::new_for_test(tx))
            .expect("start pipe backend");
        let mut pipe_collected = Vec::new();
        while Instant::now() < deadline && !marker_seen(&pipe_collected, marker) {
            match rx.try_recv() {
                Ok(bytes) => pipe_collected.extend_from_slice(&bytes),
                Err(_) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        assert!(
            marker_seen(&pipe_collected, marker),
            "PipeBackend dropped the child marker: {:?}",
            String::from_utf8_lossy(&pipe_collected)
        );
    }

    #[test]
    fn pipe_backend_resize_does_not_panic() {
        let (tx, _rx) = mpsc::channel::<Vec<u8>>(OUTPUT_CHANNEL_CAPACITY);
        let mut backend = PipeBackend::new(echo_command("resize-noop"));
        backend.start(TerminalOutput::new_for_test(tx)).unwrap();
        // No window size behind pipes: resize is a no-op, only required not to
        // panic (the rows/cols types still go through the trait shape).
        backend.resize(40, 120);
        // `echo` may have exited already; a Broken pipe / Closed error there is
        // the normal race, not a failure.
        let _ = backend.write(b"exit\r");
    }

    // ---- Task 43, I4: the Drop contract + the bounded write ----
    //
    // backends/mod.rs's Drop contract says "Drop 时必须停止 start() 启动的一切
    // 运行物". The pipe backend's honest-platform note ("a child that ignores
    // stdin EOF outlives the backend") violated exactly that: Drop killed
    // nothing, and `write` executed `write_all` on the CALLING thread -- a
    // child that stopped reading stdin parked the UI thread on the full OS
    // pipe forever (the SSH backend's 200 ms fix never covered this path).

    /// A child that never exits and never reads stdin (the I4 fixture):
    /// PowerShell's Start-Sleep on Windows, `sh -c sleep` elsewhere.
    #[cfg(windows)]
    fn never_exit_command() -> portable_pty::CommandBuilder {
        let mut cmd = portable_pty::CommandBuilder::new("powershell.exe");
        cmd.args(["-NoProfile", "-Command", "Start-Sleep -Seconds 100"]);
        cmd
    }

    #[cfg(not(windows))]
    fn never_exit_command() -> portable_pty::CommandBuilder {
        let mut cmd = portable_pty::CommandBuilder::new("sh");
        cmd.args(["-c", "sleep 100"]);
        cmd
    }

    /// I4 (a) RED: dropping the backend must terminate a stdin-ignoring,
    /// never-exiting child. The witness is the OUTPUT channel disconnecting:
    /// the two reader threads hold the only senders, they exit on EOF, and EOF
    /// arrives when the OS closes the child's pipes -- i.e. when the child is
    /// dead. With no Drop kill (the old shape), the child sleeps on and the
    /// readers stay parked in `read`, so the channel stays connected and the
    /// deadline fires.
    #[test]
    fn dropping_the_backend_kills_a_stdin_ignoring_child() {
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(OUTPUT_CHANNEL_CAPACITY);
        let mut backend = PipeBackend::new(never_exit_command());
        backend
            .start(TerminalOutput::new_for_test(tx))
            .expect("start the pipe backend");
        drop(backend);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match rx.try_recv() {
                // Closed = every reader thread exited = the child's pipes hit
                // EOF = the child is gone. PASS.
                Err(futures_channel::mpsc::TryRecvError::Closed) => return,
                _ => {
                    assert!(
                        Instant::now() < deadline,
                        "Drop 后 10s 子进程仍在运行：忽略 stdin EOF 的子进程必须被终止\
                         （backends/mod.rs 的 Drop 契约）"
                    );
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    /// I4 (b) RED: `write` on a full stdin pipe must return within a bounded
    /// time instead of parking the caller (the freya UI thread). The old shape
    /// ran `write_all` on the calling thread: a child that stopped reading
    /// stdin filled the OS pipe and the 17th 4 KiB chunk parked the UI
    /// forever. The watchdog thread turns the park into a bounded failure.
    #[test]
    fn write_on_a_full_stdin_pipe_returns_bounded() {
        let (done_tx, done_rx) = std::sync::mpsc::channel::<usize>();
        // Deliberately unjoined: on the frozen path the worker is parked
        // forever, so this thread must never wait on it (the SSH backend
        // freeze tests' same note).
        let _worker = std::thread::spawn(move || {
            let (tx, _rx) = mpsc::channel::<Vec<u8>>(OUTPUT_CHANNEL_CAPACITY);
            let mut backend = PipeBackend::new(never_exit_command());
            backend
                .start(TerminalOutput::new_for_test(tx))
                .expect("start the pipe backend");
            let chunk = vec![b'x'; 4 * 1024];
            let mut delivered = 0usize;
            for _ in 0..1024 {
                // The child never reads stdin: the OS pipe fills, then (after
                // the fix) the bounded queue. The FIRST error is the witness
                // point -- the loop stops there.
                if backend.write(&chunk).is_err() {
                    break;
                }
                delivered += 1;
            }
            let _ = done_tx.send(delivered);
        });
        match done_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(delivered) => assert!(
                delivered < 1024,
                "子进程不读 stdin，1 MiB 不可能全部送达（第 {delivered} 块后应看到有界错误）"
            ),
            Err(_) => panic!(
                "stdin 管道写满后 write 无限阻塞了调用线程：旧形态在调用线程直接 \
                 write_all，子进程不读 stdin 时 UI 线程被永久挂起"
            ),
        }
    }
}
