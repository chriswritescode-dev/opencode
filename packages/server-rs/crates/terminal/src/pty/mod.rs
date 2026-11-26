pub mod protocol;
mod spawn;

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, Weak};

use opencode_bus::{Bus, Definition, PublishOptions};
use opencode_config::shell::{self, Priority, ShellSelect};
use opencode_contract::codec::encode_json;
use opencode_contract::generated::LocationRef;
pub use opencode_contract::generated::{Pty as Info, PtyStatus};
use opencode_util::lock;
use portable_pty::PtySize;
use serde_json::{Value, json};

use crate::pty::spawn::SpawnOptions;

pub const BUFFER_LIMIT: usize = 1024 * 1024 * 2;
const EXITED_LIMIT: usize = 25;

use opencode_contract::generated::PtyID;
pub use opencode_contract::generated::requests::{
    PtyCreateBody as CreateInput, PtyUpdateBody as UpdateInput, PtyUpdateBodySize as UpdateSize,
};

pub struct Options {
    pub location: LocationRef,
    pub env: Vec<(String, String)>,
}

#[derive(Clone, Debug, thiserror::Error)]
pub enum PtyError {
    #[error("Pty.NotFoundError")]
    NotFound { pty_id: String },
    #[error("Pty.ExitedError")]
    Exited { pty_id: String },
    #[error("pty spawn failed: {message}")]
    Spawn { message: String },
}

pub struct AttachInput {
    pub cursor: Option<i64>,
    pub on_data: Arc<dyn Fn(String) + Send + Sync>,
    pub on_end: Arc<dyn Fn(Option<u32>) + Send + Sync>,
}

pub struct Attachment {
    pub replay: String,
    pub cursor: usize,
    pub write: Box<dyn Fn(String) + Send + Sync>,
    pub activate: Box<dyn Fn() + Send + Sync>,
    pub detach: Box<dyn Fn() + Send + Sync>,
}

impl Attachment {
    pub fn write(&self, data: String) {
        (self.write)(data);
    }

    pub fn activate(&self) {
        (self.activate)();
    }

    pub fn detach(&self) {
        (self.detach)();
    }
}

struct Handle {
    writer: Sender<Vec<u8>>,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    killer: Mutex<Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>>,
}

struct Subscriber {
    on_data: Arc<dyn Fn(String) + Send + Sync>,
    on_end: Arc<dyn Fn(Option<u32>) + Send + Sync>,
    active: bool,
    detached: bool,
    pending: Vec<String>,
    end: Option<Option<u32>>,
}

struct Active {
    info: Info,
    handle: Arc<Handle>,
    buffer: String,
    buffer_units: usize,
    cursor: usize,
    subscribers: BTreeMap<u64, Arc<Mutex<Subscriber>>>,
    next_token: u64,
}

struct Inner {
    bus: Arc<Bus>,
    shell: Arc<ShellSelect>,
    location: LocationRef,
    env: Vec<(String, String)>,
    sessions: Mutex<BTreeMap<String, Arc<Mutex<Active>>>>,
    exit_order: Mutex<Vec<String>>,
}

pub struct Pty {
    inner: Arc<Inner>,
}

impl Pty {
    pub fn new(bus: Arc<Bus>, shell: Arc<ShellSelect>, options: Options) -> Arc<Self> {
        Arc::new(Pty {
            inner: Arc::new(Inner {
                bus,
                shell,
                location: options.location,
                env: options.env,
                sessions: Mutex::new(BTreeMap::new()),
                exit_order: Mutex::new(Vec::new()),
            }),
        })
    }

    pub fn list(&self) -> Vec<Info> {
        lock(&self.inner.sessions).values().map(|session| lock(session).info.clone()).collect()
    }

    pub fn get(&self, id: &str) -> Result<Info, PtyError> {
        self.require(id).map(|session| lock(&session).info.clone())
    }

    pub fn create(&self, input: CreateInput) -> Result<Info, PtyError> {
        let id = format!("pty_{}", opencode_storage::id::ascending());
        let command = input
            .command
            .filter(|command| !command.is_empty())
            .unwrap_or_else(|| self.inner.shell.resolve(Priority::Config));
        let mut args = input.args.unwrap_or_default();
        if shell::login(&command) {
            args.push("-l".to_owned());
        }
        let cwd = input.cwd.filter(|cwd| !cwd.is_empty()).unwrap_or_else(|| self.inner.location.directory.0.clone());
        let env = self.environment(input.env);
        let title = input
            .title
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| format!("Terminal {}", &id[id.len() - 4..]));
        let options = SpawnOptions { cols: 80, rows: 24, cwd: &cwd, env: &env };
        let spawned = spawn::open(&command, &args, &options).map_err(|message| PtyError::Spawn { message })?;
        let info = Info {
            id: PtyID(id.clone()),
            title,
            command,
            args,
            cwd,
            status: PtyStatus::Running,
            pid: f64::from(spawned.pid),
            exit_code: None,
        };
        let handle = Arc::new(Handle {
            writer: spawn::writer(spawned.writer),
            master: Mutex::new(spawned.master),
            killer: Mutex::new(Some(spawned.killer)),
        });
        let session = Arc::new(Mutex::new(Active {
            info: info.clone(),
            handle,
            buffer: String::new(),
            buffer_units: 0,
            cursor: 0,
            subscribers: BTreeMap::new(),
            next_token: 0,
        }));
        lock(&self.inner.sessions).insert(id.clone(), session);
        let weak = Arc::downgrade(&self.inner);
        let data_weak = weak.clone();
        let data_id = id.clone();
        spawn::start(
            spawned.reader,
            spawned.child,
            move |chunk| handle_data(&data_weak, &data_id, chunk),
            move |code| handle_exit(&weak, &id, code),
        );
        self.inner.publish("pty.created", json!({ "info": info_value(&info) }));
        Ok(info)
    }

    pub fn update(&self, id: &str, input: UpdateInput) -> Result<Info, PtyError> {
        let session = self.require(id)?;
        if let Some(title) = input.title.filter(|title| !title.is_empty()) {
            lock(&session).info.title = title;
        }
        if let Some(size) = input.size {
            let handle = {
                let active = lock(&session);
                (active.info.status == PtyStatus::Running).then(|| active.handle.clone())
            };
            if let Some(handle) = handle {
                let size =
                    PtySize { rows: dimension(size.rows), cols: dimension(size.cols), pixel_width: 0, pixel_height: 0 };
                let _ = lock(&handle.master).resize(size);
            }
        }
        let info = lock(&session).info.clone();
        self.inner.publish("pty.updated", json!({ "info": info_value(&info) }));
        Ok(info)
    }

    pub fn remove(&self, id: &str) -> Result<(), PtyError> {
        self.require(id)?;
        self.inner.remove_session(id);
        Ok(())
    }

    pub fn write(&self, id: &str, data: &str) -> Result<(), PtyError> {
        let session = self.require(id)?;
        let handle = {
            let active = lock(&session);
            (active.info.status == PtyStatus::Running).then(|| active.handle.clone())
        };
        let Some(handle) = handle else {
            return Ok(());
        };
        let _ = handle.writer.send(data.as_bytes().to_vec());
        Ok(())
    }

    pub fn attach(&self, id: &str, input: AttachInput) -> Result<Attachment, PtyError> {
        let session = self.require(id)?;
        let (replay, cursor, token, subscriber) = {
            let mut active = lock(&session);
            if active.info.status != PtyStatus::Running {
                return Err(PtyError::Exited { pty_id: id.to_owned() });
            }
            let token = active.next_token;
            active.next_token += 1;
            let subscriber = Arc::new(Mutex::new(Subscriber {
                on_data: input.on_data,
                on_end: input.on_end,
                active: false,
                detached: false,
                pending: Vec::new(),
                end: None,
            }));
            active.subscribers.insert(token, subscriber.clone());
            let end = active.cursor;
            let from = match input.cursor {
                Some(-1) => end,
                Some(cursor) => usize::try_from(cursor.max(0)).unwrap_or(usize::MAX).min(end),
                None => 0,
            };
            let buffer_start = end - active.buffer_units;
            let offset = from.saturating_sub(buffer_start);
            let replay = if offset >= active.buffer_units {
                String::new()
            } else {
                slice_from_units(&active.buffer, offset).to_owned()
            };
            (replay, end, token, subscriber)
        };
        let write_session = session.clone();
        let activate_subscriber = subscriber.clone();
        let detach_session = session.clone();
        let detach_subscriber = subscriber;
        Ok(Attachment {
            replay,
            cursor,
            write: Box::new(move |data: String| {
                let handle = {
                    let active = lock(&write_session);
                    (active.info.status == PtyStatus::Running).then(|| active.handle.clone())
                };
                if let Some(handle) = handle {
                    let _ = handle.writer.send(data.into_bytes());
                }
            }),
            activate: Box::new(move || {
                let mut subscriber = lock(&activate_subscriber);
                if subscriber.active || subscriber.detached {
                    return;
                }
                subscriber.active = true;
                let pending = std::mem::take(&mut subscriber.pending);
                let end = subscriber.end.take();
                let on_data = subscriber.on_data.clone();
                let on_end = subscriber.on_end.clone();
                for chunk in pending {
                    on_data(chunk);
                }
                if let Some(code) = end {
                    on_end(code);
                }
            }),
            detach: Box::new(move || {
                lock(&detach_session).subscribers.remove(&token);
                let mut subscriber = lock(&detach_subscriber);
                subscriber.detached = true;
                subscriber.pending.clear();
                subscriber.end = None;
            }),
        })
    }

    pub fn shutdown(&self) {
        self.inner.shutdown_inner();
    }

    fn require(&self, id: &str) -> Result<Arc<Mutex<Active>>, PtyError> {
        self.inner.session(id).ok_or_else(|| PtyError::NotFound { pty_id: id.to_owned() })
    }

    fn environment(&self, overrides: Option<opencode_contract::codec::JsonMap<String>>) -> Vec<(String, String)> {
        let mut env = self.inner.env.clone();
        if let Some(overrides) = overrides {
            for (name, value) in overrides.0 {
                set(&mut env, &name, value);
            }
        }
        set(&mut env, "TERM", "xterm-256color".to_owned());
        set(&mut env, "OPENCODE_TERMINAL", "1".to_owned());
        if cfg!(windows) {
            set(&mut env, "LC_ALL", "C.UTF-8".to_owned());
            set(&mut env, "LC_CTYPE", "C.UTF-8".to_owned());
            set(&mut env, "LANG", "C.UTF-8".to_owned());
        }
        env
    }
}

impl Inner {
    fn session(&self, id: &str) -> Option<Arc<Mutex<Active>>> {
        lock(&self.sessions).get(id).cloned()
    }

    fn publish(&self, r#type: &'static str, data: Value) {
        let options = PublishOptions { location: Some(self.location.clone()), ..Default::default() };
        if let Err(error) = self.bus.publish(&Definition::ephemeral(r#type), data, options) {
            log::warn!("failed to publish {type}: {error}");
        }
    }

    fn remove_session(&self, id: &str) {
        let session = lock(&self.sessions).remove(id);
        lock(&self.exit_order).retain(|candidate| candidate != id);
        let Some(session) = session else {
            return;
        };
        teardown(&session);
        self.publish("pty.deleted", json!({ "id": id }));
    }

    fn evict(&self) {
        loop {
            let oldest = {
                let order = lock(&self.exit_order);
                if order.len() <= EXITED_LIMIT {
                    return;
                }
                order[0].clone()
            };
            self.remove_session(&oldest);
        }
    }

    fn shutdown_inner(&self) {
        let sessions: Vec<_> = std::mem::take(&mut *lock(&self.sessions)).into_values().collect();
        lock(&self.exit_order).clear();
        for session in sessions {
            teardown(&session);
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

fn teardown(session: &Arc<Mutex<Active>>) {
    let handle = {
        let active = lock(session);
        (active.info.status == PtyStatus::Running).then(|| active.handle.clone())
    };
    if let Some(handle) = handle
        && let Some(mut killer) = lock(&handle.killer).take()
    {
        let _ = killer.kill();
    }
    notify_end(session, None);
}

fn handle_data(weak: &Weak<Inner>, id: &str, chunk: String) {
    let Some(inner) = weak.upgrade() else {
        return;
    };
    let Some(session) = inner.session(id) else {
        return;
    };
    let mut deliveries = Vec::new();
    {
        let mut active = lock(&session);
        let units = chunk.encode_utf16().count();
        active.cursor += units;
        for subscriber in active.subscribers.values() {
            let mut subscriber = lock(subscriber);
            if !subscriber.active {
                subscriber.pending.push(chunk.clone());
                continue;
            }
            deliveries.push((subscriber.on_data.clone(), chunk.clone()));
        }
        active.buffer.push_str(&chunk);
        active.buffer_units += units;
        if active.buffer_units > BUFFER_LIMIT {
            let overflow = active.buffer_units - BUFFER_LIMIT;
            let dropped = drop_prefix_units(&mut active.buffer, overflow);
            active.buffer_units -= dropped;
        }
    }
    for (callback, chunk) in deliveries {
        callback(chunk);
    }
}

fn handle_exit(weak: &Weak<Inner>, id: &str, code: Option<u32>) {
    let Some(inner) = weak.upgrade() else {
        return;
    };
    let Some(session) = inner.session(id) else {
        return;
    };
    {
        let mut active = lock(&session);
        if active.info.status == PtyStatus::Exited {
            return;
        }
        active.info.status = PtyStatus::Exited;
        active.info.exit_code = code.map(f64::from);
    }
    notify_end(&session, code);
    if inner.session(id).is_none() {
        return;
    }
    lock(&inner.exit_order).push(id.to_owned());
    inner.publish("pty.exited", json!({ "id": id, "exitCode": f64::from(code.unwrap_or(0)) }));
    inner.evict();
}

fn notify_end(session: &Arc<Mutex<Active>>, code: Option<u32>) {
    let mut callbacks = Vec::new();
    {
        let mut active = lock(session);
        for subscriber in active.subscribers.values() {
            let mut subscriber = lock(subscriber);
            if !subscriber.active {
                subscriber.end = Some(code);
                continue;
            }
            callbacks.push(subscriber.on_end.clone());
        }
        active.subscribers.clear();
    }
    for callback in callbacks {
        callback(code);
    }
}

fn info_value(info: &Info) -> Value {
    serde_json::from_str(&encode_json(info)).expect("the generated Pty type encodes as JSON")
}

fn slice_from_units(text: &str, units: usize) -> &str {
    if units == 0 {
        return text;
    }
    let mut consumed = 0;
    for (index, character) in text.char_indices() {
        if consumed >= units {
            return &text[index..];
        }
        consumed += character.len_utf16();
    }
    ""
}

fn drop_prefix_units(text: &mut String, units: usize) -> usize {
    let mut consumed = 0;
    let mut cut = 0;
    for (index, character) in text.char_indices() {
        if consumed >= units {
            cut = index;
            break;
        }
        consumed += character.len_utf16();
        cut = index + character.len_utf8();
    }
    if cut > 0 {
        text.drain(..cut);
    }
    consumed
}

fn dimension(value: f64) -> u16 {
    value.clamp(1.0, f64::from(u16::MAX)) as u16
}

fn set(env: &mut Vec<(String, String)>, name: &str, value: String) {
    env.retain(|(key, _)| key != name);
    env.push((name.to_owned(), value));
}
