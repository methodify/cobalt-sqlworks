//! The session layer: a tokio runtime on its own thread that owns every database connection
//! and talks to the UI thread through two channels.
//!
//! UI → runtime: [`Command`] (unbounded mpsc). Runtime → UI: [`Event`] (crossbeam, drained at
//! the top of every frame) plus `request_repaint()` so the UI wakes up.
//!
//! Each editor tab gets a [`tab_actor`] task that owns one `Box<dyn Connection>` and executes
//! commands strictly in order (a session does one thing at a time). Metadata reads for the
//! object explorer use a separate per-profile actor with `ConnectionRole::Metadata`, so browsing
//! never queues behind a running query.
//!
//! Result rows never travel as events: the actor creates an `Arc<ResultSet>`, hands it to the UI
//! once (`Event::ResultSetStarted`), then appends Arrow batches into it directly.

pub mod actor;
pub mod protocol;

pub use protocol::*;

use cobalt_core::*;
use cobalt_driver::Driver;
use cobalt_results::MemoryBudget;
use crossbeam_channel::{Receiver, Sender};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Silent credential refresh for an actor that has to reopen a connection on its own (the one it
/// had died while the app sat idle). Returns the credentials unchanged when they are still good
/// (SQL login, Windows auth, an unexpired token); an Entra token past its lifetime is refreshed
/// from the stored refresh token. `Err` means the user has to sign in again interactively.
pub type CredRefresher = Arc<
    dyn Fn(ConnectionProfile, ResolvedCredentials) -> futures_util::future::BoxFuture<'static, std::result::Result<ResolvedCredentials, String>>
        + Send
        + Sync,
>;

/// Shared, cheap-to-clone context for actors.
#[derive(Clone)]
pub struct Shared {
    pub events: Sender<Event>,
    pub repaint: Arc<dyn Fn() + Send + Sync>,
    pub driver: Arc<dyn Driver>,
    pub budget: Arc<MemoryBudget>,
    pub spill_dir: PathBuf,
    pub refresh: CredRefresher,
}

impl Shared {
    pub fn emit(&self, ev: Event) {
        let _ = self.events.send(ev);
        (self.repaint)();
    }
}

/// UI-side handle. Cloneable; `send` never blocks.
#[derive(Clone)]
pub struct SessionManager {
    tx: mpsc::UnboundedSender<Command>,
    events: Receiver<Event>,
    handle: tokio::runtime::Handle,
    next_req: Arc<AtomicU64>,
    next_run: Arc<AtomicU64>,
    driver: Arc<dyn Driver>,
}

impl SessionManager {
    /// Spawn the runtime thread. `repaint` is called after every event.
    pub fn start(driver: Arc<dyn Driver>, budget: Arc<MemoryBudget>, spill_dir: PathBuf, repaint: Arc<dyn Fn() + Send + Sync>, refresh: CredRefresher) -> Self {
        let (tx, rx) = mpsc::unbounded_channel::<Command>();
        let (etx, erx) = crossbeam_channel::unbounded::<Event>();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .thread_name("cobalt-session")
            .enable_all()
            .build()
            .expect("tokio runtime");
        let handle = runtime.handle().clone();
        let shared = Shared { events: etx, repaint, driver: driver.clone(), budget, spill_dir, refresh };
        std::thread::Builder::new()
            .name("cobalt-session-main".into())
            .spawn(move || {
                runtime.block_on(dispatcher(rx, shared));
            })
            .expect("session thread");
        Self { tx, events: erx, handle, next_req: Arc::new(AtomicU64::new(1)), next_run: Arc::new(AtomicU64::new(1)), driver }
    }

    pub fn driver(&self) -> Arc<dyn Driver> {
        self.driver.clone()
    }

    pub fn send(&self, cmd: Command) {
        if self.tx.send(cmd).is_err() {
            tracing::error!("session runtime is gone");
        }
    }

    /// Drain all pending events (call once per frame).
    pub fn drain(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    pub fn new_request(&self) -> RequestId {
        RequestId(self.next_req.fetch_add(1, Ordering::Relaxed))
    }

    pub fn new_run(&self) -> RunId {
        RunId(self.next_run.fetch_add(1, Ordering::Relaxed))
    }

    /// Run an arbitrary future on the session runtime (credential flows, exports).
    pub fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.handle.spawn(fut);
    }

    pub fn handle(&self) -> tokio::runtime::Handle {
        self.handle.clone()
    }
}

/// Routes commands to per-tab and per-profile actors.
async fn dispatcher(mut rx: mpsc::UnboundedReceiver<Command>, shared: Shared) {
    let mut tabs: HashMap<TabId, mpsc::UnboundedSender<actor::TabMsg>> = HashMap::new();
    let mut meta: HashMap<ProfileId, mpsc::UnboundedSender<actor::MetaMsg>> = HashMap::new();

    while let Some(cmd) = rx.recv().await {
        match cmd {
            Command::Shutdown => break,
            Command::Connect { tab, profile, creds, database } => {
                // A fresh actor per connect; any previous one for this tab is told to close.
                if let Some(old) = tabs.remove(&tab) {
                    let _ = old.send(actor::TabMsg::Close);
                }
                let (atx, arx) = mpsc::unbounded_channel();
                tabs.insert(tab, atx);
                tokio::spawn(actor::tab_actor(tab, profile, creds, database, arx, shared.clone()));
            }
            Command::Disconnect { tab } => {
                if let Some(a) = tabs.remove(&tab) {
                    let _ = a.send(actor::TabMsg::Close);
                } else {
                    shared.emit(Event::Disconnected { tab });
                }
            }
            Command::Run { tab, run, script, opts, start_line, sink } => route_tab(&tabs, &shared, tab, actor::TabMsg::Run { run, script, opts, start_line, sink }),
            Command::Cancel { tab } => route_tab(&tabs, &shared, tab, actor::TabMsg::Cancel),
            Command::Import { tab, table, create_sql, columns, rx, cancel } => route_tab(&tabs, &shared, tab, actor::TabMsg::Import { table, create_sql, columns, rx, cancel }),
            Command::FetchMore { tab, rows } => route_tab(&tabs, &shared, tab, actor::TabMsg::FetchMore { rows }),
            Command::ChangeDatabase { tab, database } => route_tab(&tabs, &shared, tab, actor::TabMsg::ChangeDatabase { database }),
            Command::Ping { tab } => route_tab(&tabs, &shared, tab, actor::TabMsg::Ping),
            Command::SimulateLost { tab } => route_tab(&tabs, &shared, tab, actor::TabMsg::SimulateLost),
            Command::Metadata { req, profile, creds, kind } => {
                let entry = meta.entry(profile.id);
                let tx = entry.or_insert_with(|| {
                    let (mtx, mrx) = mpsc::unbounded_channel();
                    tokio::spawn(actor::meta_actor(profile.clone(), creds.clone(), mrx, shared.clone()));
                    mtx
                });
                if tx.send(actor::MetaMsg::Request { req, kind: kind.clone() }).is_err() {
                    // actor died (connection lost) — recreate once
                    meta.remove(&profile.id);
                    let (mtx, mrx) = mpsc::unbounded_channel();
                    tokio::spawn(actor::meta_actor(profile.clone(), creds, mrx, shared.clone()));
                    let _ = mtx.send(actor::MetaMsg::Request { req, kind });
                    meta.insert(profile.id, mtx);
                }
            }
            Command::CloseMetadata { profile } => {
                if let Some(m) = meta.remove(&profile) {
                    let _ = m.send(actor::MetaMsg::Close);
                }
            }
        }
    }
}

fn route_tab(tabs: &HashMap<TabId, mpsc::UnboundedSender<actor::TabMsg>>, shared: &Shared, tab: TabId, msg: actor::TabMsg) {
    match tabs.get(&tab) {
        Some(a) => {
            if a.send(msg).is_err() {
                shared.emit(Event::Disconnected { tab });
            }
        }
        None => shared.emit(Event::NotConnected { tab }),
    }
}
