use std::io::Read;

use portable_pty::{Child, ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

pub struct SpawnOptions<'a> {
    pub cols: u16,
    pub rows: u16,
    pub cwd: &'a str,
    pub env: &'a [(String, String)],
}

pub struct Spawned {
    pub pid: u32,
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn std::io::Write + Send>,
    pub master: Box<dyn MasterPty + Send>,
    pub killer: Box<dyn ChildKiller + Send + Sync>,
    pub child: Box<dyn Child + Send + Sync>,
}

pub fn open(command: &str, args: &[String], options: &SpawnOptions<'_>) -> Result<Spawned, String> {
    let pair = native_pty_system()
        .openpty(PtySize { rows: options.rows, cols: options.cols, pixel_width: 0, pixel_height: 0 })
        .map_err(|error| error.to_string())?;
    let mut builder = CommandBuilder::new(command);
    builder.args(args);
    builder.cwd(options.cwd);
    builder.env_clear();
    for (name, value) in options.env {
        builder.env(name, value);
    }
    let child = pair.slave.spawn_command(builder).map_err(|error| error.to_string())?;
    drop(pair.slave);
    let pid = child.process_id().unwrap_or(0);
    let reader = pair.master.try_clone_reader().map_err(|error| error.to_string())?;
    let writer = pair.master.take_writer().map_err(|error| error.to_string())?;
    let killer = child.clone_killer();
    Ok(Spawned { pid, reader, writer, master: pair.master, killer, child })
}

pub fn writer(mut writer: Box<dyn std::io::Write + Send>) -> std::sync::mpsc::Sender<Vec<u8>> {
    let (sender, receiver) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        while let Ok(bytes) = receiver.recv() {
            let _ = writer.write_all(&bytes);
            let _ = writer.flush();
        }
    });
    sender
}

pub fn start(
    mut reader: Box<dyn Read + Send>,
    mut child: Box<dyn Child + Send + Sync>,
    on_data: impl Fn(String) + Send + 'static,
    on_exit: impl FnOnce(Option<u32>) + Send + 'static,
) {
    let reading = std::thread::spawn(move || {
        let mut decoder = Decoder::default();
        let mut bytes = [0u8; 8192];
        loop {
            match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => {
                    let text = decoder.push(&bytes[..count]);
                    if !text.is_empty() {
                        on_data(text);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let text = decoder.finish();
        if !text.is_empty() {
            on_data(text);
        }
    });
    std::thread::spawn(move || {
        let status = child.wait();
        let _ = reading.join();
        let code = status.ok().map(|status| status.exit_code());
        on_exit(code);
    });
}

#[derive(Default)]
struct Decoder {
    pending: Vec<u8>,
}

impl Decoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut text = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(valid) => {
                    text.push_str(valid);
                    self.pending.clear();
                    return text;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    text.push_str(std::str::from_utf8(&self.pending[..valid]).expect("the prefix is valid"));
                    match error.error_len() {
                        Some(length) => {
                            text.push('\u{FFFD}');
                            self.pending.drain(..valid + length);
                        }
                        None => {
                            self.pending.drain(..valid);
                            return text;
                        }
                    }
                }
            }
        }
    }

    fn finish(&mut self) -> String {
        let text = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        text
    }
}
