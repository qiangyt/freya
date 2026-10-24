use std::{
    io::{
        Read,
        Write,
    },
    process::{
        Command,
        Stdio,
    },
};

use portable_pty::CommandBuilder;

use super::{
    TerminalBackend,
    TerminalOutput,
};
use crate::handle::TerminalError;

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
/// Reaping and the Drop contract follow the `PtyBackend` shape: the child moves
/// into a reaper thread that joins the two readers and then `wait()`s (no
/// zombie). One honest platform difference vs the PTY: a pipe has no
/// master-handle close that would signal the child (Unix pty close raises
/// SIGHUP, ConPTY dies with the pseudo console). Stopping this backend relies
/// on dropping the stdin writer -- the child's stdin reads EOF, which is what
/// conventional CLI programs exit on. A child that ignores stdin EOF outlives
/// the backend; its readers keep draining until the output channel closes, and
/// the reaper still reaps it afterwards.
pub struct PipeBackend {
    command: CommandBuilder,
    writer: Option<Box<dyn Write + Send>>,
}

impl PipeBackend {
    pub fn new(command: CommandBuilder) -> Self {
        Self {
            command,
            writer: None,
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
        // All pieces ready before self state lands (same order discipline as
        // pty.rs): any failure above leaves writer None and no orphan process.
        self.writer = Some(Box::new(stdin));
        // stdout and stderr merge into the same output channel (the TerminalOutput
        // Clone shares one queue; two pipes interleave at chunk granularity, which
        // is the best a byte stream without PTY framing can do).
        let stdout_reader = std::thread::spawn({
            let output = output.clone();
            move || pump(stdout, &output, "stdout")
        });
        let stderr_reader = std::thread::spawn(move || pump(stderr, &output, "stderr"));
        // Reaper: both flows stopped first, so `wait` cannot race a reader that
        // still holds buffered bytes (same reader-tail reap shape as pty.rs).
        std::thread::spawn(move || {
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            let _ = child.wait(); // reap, no zombie/orphan
        });
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TerminalError> {
        let writer = self.writer.as_mut().ok_or(TerminalError::Closed)?;
        writer.write_all(data)?;
        writer.flush()?;
        Ok(())
    }

    fn resize(&mut self, _rows: u16, _cols: u16) {
        // No-op by design: pipes carry no window size (see the struct docs).
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
}
