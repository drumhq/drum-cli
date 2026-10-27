#![allow(dead_code)] // Shared by independent integration-test executables.

use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    net::TcpListener,
    os::{fd::FromRawFd, unix::fs::PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub type Environment = BTreeMap<String, String>;
pub const CLI: &str = env!("CARGO_BIN_EXE_drum");

pub fn environment(base: &Path) -> Environment {
    let mut env: Environment = std::env::vars()
        .filter(|(key, _)| {
            !key.starts_with("GIT_") && !key.starts_with("DRUM_") && key != "XDG_CONFIG_HOME"
        })
        .collect();
    for (key, value) in [
        ("HOME", base.display().to_string()),
        ("DRUM_CONFIG_DIR", base.join("config").display().to_string()),
        ("DRUM_NO_UPDATE_CHECK", "1".into()),
        ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
        ("GIT_TERMINAL_PROMPT", "0".into()),
    ] {
        env.insert(key.into(), value);
    }
    env
}

pub fn command(
    program: impl AsRef<std::ffi::OsStr>,
    args: &[&str],
    cwd: &Path,
    env: &Environment,
) -> Command {
    let mut command = Command::new(program);
    command.args(args).current_dir(cwd).env_clear().envs(env);
    command
}

/// Reap children on assertion failures as well as normal completion.
pub struct Process(pub Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Process {
    pub fn stop(&mut self) {
        assert!(self.shutdown(), "service failed to stop cleanly");
    }
    pub fn shutdown(&mut self) -> bool {
        if let Ok(Some(status)) = self.0.try_wait() {
            return status.success();
        }
        // SAFETY: the PID belongs to this live child.
        if unsafe { libc::kill(self.0.id() as i32, libc::SIGTERM) } != 0 {
            return false;
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status.success();
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

pub struct Output {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
    pub elapsed: Duration,
}
impl Output {
    pub fn text(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
    pub fn success(self) -> String {
        assert!(
            self.status.success(),
            "command failed: {}",
            redact(&self.text())
        );
        self.stdout.trim().into()
    }
    pub fn failure(self) -> String {
        assert!(!self.status.success(), "command unexpectedly succeeded");
        self.stdout.trim().into()
    }
}
pub fn redact(text: &str) -> String {
    let mut result = String::new();
    let mut rest = text;
    while let Some(index) = rest.find("#token=") {
        result.push_str(&rest[..index]);
        result.push_str("#token=[redacted]");
        rest = &rest[index + 7..];
        rest = &rest[rest
            .find(|c: char| !c.is_ascii_hexdigit())
            .unwrap_or(rest.len())..];
    }
    result.push_str(rest);
    result
}
fn read_file(file: &mut File) -> String {
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}
pub fn run(mut command: Command, input: &str) -> Output {
    // Files avoid pipe-buffer deadlocks for large Git/service diagnostics.
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut stdin = tempfile::tempfile().unwrap();
    stdin.write_all(input.as_bytes()).unwrap();
    stdin.rewind().unwrap();
    let start = Instant::now();
    let mut process = Process(
        command
            .stdin(stdin)
            .stdout(stdout.try_clone().unwrap())
            .stderr(stderr.try_clone().unwrap())
            .spawn()
            .expect("start test command"),
    );
    let status = loop {
        if let Some(status) = process.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            start.elapsed() < Duration::from_secs(40),
            "command timed out"
        );
        thread::sleep(Duration::from_millis(10));
    };
    Output {
        status,
        stdout: read_file(&mut stdout),
        stderr: read_file(&mut stderr),
        elapsed: start.elapsed(),
    }
}
pub fn git(args: &[&str], cwd: &Path, env: &Environment) -> String {
    run(command("git", args, cwd, env), "").success()
}
pub fn cli(args: &[&str], cwd: &Path, env: &Environment) -> Output {
    run(command(CLI, args, cwd, env), "")
}
pub fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
pub fn executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
pub fn json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
pub fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}
pub fn files(directory: &Path, extension: &str) -> Vec<PathBuf> {
    fs::read_dir(directory)
        .into_iter()
        .flatten()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == extension))
        .collect()
}

/// Run with a real terminal on stderr, optionally stdin/stdout too.
pub fn terminal(mut command: Command, answer: Option<&str>, combined: bool) -> Output {
    let (mut master, slave) = unsafe {
        // SAFETY: openpty initializes both owned descriptors; no optional termios/name pointers.
        let (mut master, mut slave) = (-1, -1);
        assert_eq!(
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut()
            ),
            0
        );
        (File::from_raw_fd(master), File::from_raw_fd(slave))
    };
    use std::os::fd::AsRawFd;
    // SAFETY: master is a valid owned descriptor.
    unsafe {
        assert_ne!(
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK),
            -1
        );
    }
    let mut stdout = tempfile::tempfile().unwrap();
    command.stderr(slave.try_clone().unwrap());
    if answer.is_some() {
        command.stdin(slave.try_clone().unwrap());
    } else {
        command.stdin(Stdio::null());
    }
    if combined {
        command.stdout(slave.try_clone().unwrap());
    } else {
        command.stdout(stdout.try_clone().unwrap());
    }
    let start = Instant::now();
    let mut process = Process(command.spawn().unwrap());
    let mut bytes = Vec::new();
    let mut sent = false;
    let status = loop {
        let mut buffer = [0; 8192];
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.raw_os_error() == Some(libc::EIO) =>
                {
                    break;
                }
                Err(error) => panic!("reading terminal: {error}"),
            }
        }
        if !sent
            && let Some(answer) = answer
            && String::from_utf8_lossy(&bytes).contains("Continue? [y/N] ")
        {
            master.write_all(answer.as_bytes()).unwrap();
            sent = true;
        }
        if let Some(status) = process.0.try_wait().unwrap() {
            // Drain after exit, retaining the slave until all output is read (macOS).
            loop {
                match master.read(&mut buffer) {
                    Ok(n) if n > 0 => bytes.extend_from_slice(&buffer[..n]),
                    _ => break,
                }
            }
            break status;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "terminal command timed out"
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert!(answer.is_none() || sent, "expected trust prompt");
    Output {
        status,
        stdout: read_file(&mut stdout),
        stderr: String::from_utf8_lossy(&bytes).into_owned(),
        elapsed: start.elapsed(),
    }
}

pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: String,
}
pub struct Response {
    pub status: u16,
    pub body: String,
    pub delay: Duration,
}
impl Response {
    pub fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            body: value.to_string(),
            delay: Duration::ZERO,
        }
    }
}
pub struct Server {
    pub origin: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Server {
    pub fn start(handler: impl Fn(Request, &str) -> Response + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let server_origin = origin.clone();
        let handler = Arc::new(handler);
        let thread = thread::spawn(move || {
            let mut workers = Vec::new();
            while !stopping.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let handler = handler.clone();
                        let origin = server_origin.clone();
                        workers.push(thread::spawn(move || {
                            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
                            let mut bytes = Vec::new(); let mut byte = [0];
                            while bytes.len() < 16384 && !bytes.ends_with(b"\r\n\r\n") {
                                if stream.read(&mut byte).unwrap_or(0) == 0 { return; } bytes.push(byte[0]);
                            }
                            let headers = String::from_utf8_lossy(&bytes).into_owned();
                            let mut parts = headers.split_whitespace();
                            let request = Request { method: parts.next().unwrap().into(), path: parts.next().unwrap().into(), headers };
                            let response = handler(request, &origin);
                            thread::sleep(response.delay);
                            let extra = if response.status == 302 { "Location: /must-not-follow\r\n" } else { "" };
                            let _ = write!(stream, "HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n{}\r\n{}", response.status, response.body.len(), extra, response.body);
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => panic!("fake API accept: {error}"),
                }
            }
            for worker in workers {
                worker.join().unwrap();
            }
        });
        Self {
            origin,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let result = self.thread.take().unwrap().join();
        if !thread::panicking() {
            result.unwrap();
        }
    }
}
