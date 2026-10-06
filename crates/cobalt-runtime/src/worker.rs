//! Client for local-spark-mcp's worker process. Cobalt listens on a loopback port, spawns
//! `python -m local_spark_mcp.worker --port N`, and exchanges length-prefixed JSON frames:
//! requests `{"id", "method", "params"}`, responses `{"id", "ok", "result"}` or
//! `{"id", "ok": false, "error", "traceback", "fatal"}`.
//!
//! The worker is synchronous: one request at a time. Its stdout/stderr (Spark and py4j chatter,
//! Ivy progress, tracebacks) is pumped to a `log` callback while calls wait.

use crate::{Result, RuntimeError};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub type LogFn = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    /// The environment's interpreter.
    pub python: PathBuf,
    pub env: Vec<(String, String)>,
    /// `SparkEngine` keyword arguments for `init`.
    pub init: Value,
    pub startup_timeout: Duration,
}

pub struct Worker {
    child: Child,
    stream: TcpStream,
    next_id: u64,
    log_rx: Receiver<String>,
    log: LogFn,
    tail: VecDeque<String>,
    /// The `init` result (versions, profile, databases…).
    pub info: Value,
    pub started: Instant,
}

pub fn send_frame(stream: &mut TcpStream, v: &Value) -> Result<()> {
    let data = serde_json::to_vec(v)?;
    let len = (data.len() as u32).to_be_bytes();
    stream.write_all(&len)?;
    stream.write_all(&data)?;
    stream.flush()?;
    Ok(())
}

pub fn recv_frame(stream: &mut TcpStream) -> Result<Option<Value>> {
    let mut len = [0u8; 4];
    match stream.read_exact(&mut len) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let n = u32::from_be_bytes(len) as usize;
    let mut body = vec![0u8; n];
    stream.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

impl Worker {
    /// Spawn and complete the `init` handshake. `cancel` aborts the wait (the process is killed).
    pub fn start(cfg: &WorkerConfig, log: LogFn, cancel: &AtomicBool) -> Result<Worker> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let mut cmd = Command::new(&cfg.python);
        cmd.args(["-m", "local_spark_mcp.worker", "--port", &port.to_string()]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let mut child = cmd.spawn().map_err(|e| RuntimeError::Worker(format!("could not start {}: {e}", cfg.python.display())))?;
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        for pipe in [child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>), child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)].into_iter().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                use std::io::BufRead;
                for line in std::io::BufReader::new(pipe).lines().map_while(std::result::Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);
        let mut w = Worker { child, stream: placeholder_stream(), next_id: 0, log_rx: rx, log, tail: VecDeque::new(), info: Value::Null, started: Instant::now() };
        let deadline = Instant::now() + cfg.startup_timeout;
        let stream = loop {
            w.pump();
            if cancel.load(Ordering::Relaxed) {
                let _ = w.child.kill();
                return Err(RuntimeError::Cancelled);
            }
            match listener.accept() {
                Ok((s, _)) => break s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => {
                    let _ = w.child.kill();
                    return Err(e.into());
                }
            }
            if let Ok(Some(status)) = w.child.try_wait() {
                w.pump();
                return Err(RuntimeError::Worker(format!("worker exited with {status} before connecting:\n{}", w.tail_text())));
            }
            if Instant::now() > deadline {
                let _ = w.child.kill();
                return Err(RuntimeError::Worker(format!("worker did not connect within {:?}", cfg.startup_timeout)));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        stream.set_nodelay(true)?;
        w.stream = stream;
        let info = w.call_cancellable("init", cfg.init.clone(), cfg.startup_timeout, cancel)?;
        w.pump();
        w.info = info;
        Ok(w)
    }

    /// Forward buffered worker output to the log.
    pub fn pump(&mut self) {
        while let Ok(line) = self.log_rx.try_recv() {
            self.tail.push_back(line.clone());
            if self.tail.len() > 60 {
                self.tail.pop_front();
            }
            (self.log)(line);
        }
    }

    /// The last lines the worker printed (for error reports).
    pub fn tail_text(&self) -> String {
        self.tail.iter().cloned().collect::<Vec<_>>().join("\n")
    }

    /// A request that waits for its reply while pumping logs; `cancel` aborts the wait, after
    /// which the connection is out of step and the worker must be killed.
    pub fn call_cancellable(&mut self, method: &str, params: Value, timeout: Duration, cancel: &AtomicBool) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        send_frame(&mut self.stream, &json!({"id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + timeout;
        self.stream.set_read_timeout(Some(Duration::from_millis(200)))?;
        let resp = loop {
            self.pump();
            if cancel.load(Ordering::Relaxed) {
                return Err(RuntimeError::Cancelled);
            }
            match recv_frame(&mut self.stream) {
                Ok(Some(v)) => break v,
                Ok(None) => return Err(RuntimeError::Worker(format!("worker closed the connection during {method}:\n{}", self.tail_text()))),
                Err(RuntimeError::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {
                    if let Ok(Some(status)) = self.child.try_wait() {
                        self.pump();
                        return Err(RuntimeError::Worker(format!("worker exited with {status} during {method}:\n{}", self.tail_text())));
                    }
                    if Instant::now() > deadline {
                        return Err(RuntimeError::Worker(format!("{method} timed out after {timeout:?}")));
                    }
                }
                Err(e) => return Err(e),
            }
        };
        self.stream.set_read_timeout(None)?;
        self.pump();
        if resp.get("ok").and_then(Value::as_bool) != Some(true) {
            let err = resp.get("error").and_then(Value::as_str).unwrap_or("unknown worker error");
            let tb = resp.get("traceback").and_then(Value::as_str).unwrap_or("");
            let fatal = resp.get("fatal").and_then(Value::as_bool).unwrap_or(false);
            let text = if tb.is_empty() { err.to_string() } else { format!("{err}\n{tb}") };
            return Err(if fatal { RuntimeError::WorkerFatal(text) } else { RuntimeError::Worker(text) });
        }
        if resp.get("fatal").and_then(Value::as_bool) == Some(true) {
            let r = resp.get("result").cloned().unwrap_or(Value::Null);
            return Err(RuntimeError::WorkerFatal(r.get("error").and_then(Value::as_str).unwrap_or("the Spark driver is no longer reachable").to_string()));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    /// A plain request with a timeout (no cancel).
    pub fn call(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let never = AtomicBool::new(false);
        self.call_cancellable(method, params, timeout, &never)
    }

    pub fn run_sql(&mut self, sql: &str, limit: Option<u64>) -> Result<Value> {
        self.call("run_sql", json!({"sql": sql, "limit": limit}), Duration::from_secs(60 * 60))
    }
    pub fn run_code(&mut self, code: &str) -> Result<Value> {
        self.call("run_code", json!({"code": code}), Duration::from_secs(60 * 60 * 24))
    }
    pub fn get_info(&mut self) -> Result<Value> {
        self.call("info", json!({}), Duration::from_secs(60))
    }
    pub fn ping(&mut self) -> Result<bool> {
        Ok(self.call("ping", json!({}), Duration::from_secs(10))?.get("pong").and_then(Value::as_bool).unwrap_or(false))
    }
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Ask the worker to stop, then make sure the process is gone.
    pub fn shutdown(mut self) {
        self.next_id += 1;
        let _ = send_frame(&mut self.stream, &json!({"id": self.next_id, "method": "shutdown", "params": {}}));
        let _ = self.stream.set_read_timeout(Some(Duration::from_secs(10)));
        let _ = recv_frame(&mut self.stream);
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Kill the process outright (after a cancelled call, the protocol is out of step).
    pub fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A connected-to-nothing stream used only until `start` accepts the real one.
fn placeholder_stream() -> TcpStream {
    let l = TcpListener::bind(("127.0.0.1", 0)).expect("loopback");
    let addr = l.local_addr().expect("addr");
    let c = TcpStream::connect(addr).expect("connect");
    let _ = l.accept();
    c
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_roundtrip() {
        let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let req = recv_frame(&mut s).unwrap().unwrap();
            assert_eq!(req["method"], "ping");
            send_frame(&mut s, &json!({"id": req["id"], "ok": true, "result": {"pong": true}})).unwrap();
        });
        let mut c = TcpStream::connect(("127.0.0.1", port)).unwrap();
        send_frame(&mut c, &json!({"id": 1, "method": "ping", "params": {}})).unwrap();
        let resp = recv_frame(&mut c).unwrap().unwrap();
        assert_eq!(resp["result"]["pong"], true);
        assert!(recv_frame(&mut c).unwrap().is_none());
        t.join().unwrap();
    }
}
