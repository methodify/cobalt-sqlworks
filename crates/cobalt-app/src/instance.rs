//! One Cobalt per user: a second launch (a `.sql` double-clicked in Explorer while Cobalt is
//! open, a shortcut with `-S server`) hands its command line to the running instance over a
//! local socket and exits; the running one opens the files and brings its window forward.
//! The dev build (the `agent` feature) stays out of it unless `COBALT_INSTANCE` is set, so a
//! driven dev instance never hijacks a release build's launches.

use interprocess::local_socket::{prelude::*, GenericFilePath, GenericNamespaced, ListenerOptions, Stream};
use std::io::{BufRead, BufReader, Write};
use std::sync::{mpsc, Arc, Mutex};

/// What a launch found out.
pub enum Claim {
    /// This process is the instance: launches handed over by later processes arrive here.
    Primary(Receiver),
    /// A running instance took the command line; this process should exit.
    Forwarded,
}

/// The primary's end: the handed-over command lines, and the egui context to wake for them.
pub struct Receiver {
    pub rx: mpsc::Receiver<Vec<String>>,
    wake: Arc<Mutex<Option<egui::Context>>>,
}

impl Receiver {
    /// A receiver nothing ever sends to (single-instance handling switched off).
    pub fn none() -> Self {
        let (_tx, rx) = mpsc::channel();
        Self { rx, wake: Arc::new(Mutex::new(None)) }
    }
    /// The listener thread repaints this context when a launch arrives.
    pub fn bind(&self, ctx: egui::Context) {
        *self.wake.lock().unwrap() = Some(ctx);
    }
}

fn socket_name() -> String {
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "user".into());
    let user: String = user.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    format!("cobalt-sqlworks-{user}.instance")
}

fn name() -> std::io::Result<interprocess::local_socket::Name<'static>> {
    let n = socket_name();
    if GenericNamespaced::is_supported() {
        n.to_ns_name::<GenericNamespaced>()
    } else {
        std::env::temp_dir().join(n).to_fs_name::<GenericFilePath>()
    }
}

/// Hand `args` to a running instance, or become the instance.
pub fn claim(args: Vec<String>) -> Claim {
    let name = match name() {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "single-instance socket name; running standalone");
            return Claim::Primary(Receiver::none());
        }
    };
    // a running instance answers: hand the command line over
    if let Ok(mut s) = Stream::connect(name.clone()) {
        let line = serde_json::to_string(&args).unwrap_or_else(|_| "[]".into());
        if s.write_all(line.as_bytes()).and_then(|_| s.write_all(b"\n")).and_then(|_| s.flush()).is_ok() {
            let mut ack = String::new();
            let _ = BufReader::new(s).read_line(&mut ack);
            tracing::info!(args = ?args, "handed the launch to the running Cobalt");
            return Claim::Forwarded;
        }
    }
    // a stale socket file (a crash on a platform with file-type names) must not keep us out
    if !GenericNamespaced::is_supported() {
        let _ = std::fs::remove_file(std::env::temp_dir().join(socket_name()));
    }
    let listener = match ListenerOptions::new().name(name).create_sync() {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(error = %e, "single-instance listener not created; running standalone");
            return Claim::Primary(Receiver::none());
        }
    };
    let (tx, rx) = mpsc::channel::<Vec<String>>();
    let wake: Arc<Mutex<Option<egui::Context>>> = Arc::new(Mutex::new(None));
    let wake2 = Arc::clone(&wake);
    std::thread::Builder::new()
        .name("cobalt-instance".into())
        .spawn(move || {
            for conn in listener.incoming() {
                let Ok(conn) = conn else { continue };
                let mut reader = BufReader::new(conn);
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let args: Vec<String> = serde_json::from_str(line.trim()).unwrap_or_default();
                let _ = reader.get_mut().write_all(b"ok\n");
                if tx.send(args).is_err() {
                    break;
                }
                if let Some(ctx) = wake2.lock().unwrap().as_ref() {
                    ctx.request_repaint();
                }
            }
        })
        .ok();
    Claim::Primary(Receiver { rx, wake })
}
