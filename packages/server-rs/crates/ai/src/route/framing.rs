use std::collections::{BTreeSet, VecDeque};
use std::pin::Pin;
use std::sync::Arc;

use futures_util::{Stream, StreamExt, stream};

use crate::route::executor::ByteStream;
use crate::schema::{AiError, AiErrorReason};

const MAX_EVENT_SIZE: usize = 10 * 1024 * 1024;
const BOM: char = '\u{feff}';

pub type FrameStream = Pin<Box<dyn Stream<Item = Result<String, AiError>> + Send>>;

#[derive(Clone)]
pub struct Definition {
    pub id: &'static str,
    frame: Arc<dyn Fn(ByteStream) -> FrameStream + Send + Sync>,
}

impl Definition {
    pub fn new(id: &'static str, frame: impl Fn(ByteStream) -> FrameStream + Send + Sync + 'static) -> Self {
        Self { id, frame: Arc::new(frame) }
    }

    pub fn frame(&self, bytes: ByteStream) -> FrameStream {
        (self.frame)(bytes)
    }
}

pub fn sse() -> Definition {
    Definition::new("sse", |bytes| sse_frames(bytes, None, false))
}

pub fn sse_with_done() -> Definition {
    Definition::new("sse", |bytes| sse_frames(bytes, None, true))
}

pub fn sse_events(events: impl IntoIterator<Item = String>) -> Definition {
    let events: BTreeSet<String> = events.into_iter().collect();
    Definition::new("sse", move |bytes| sse_frames(bytes, Some(events.clone()), false))
}

pub fn lines() -> Definition {
    Definition::new("lines", |bytes| line_frames(bytes))
}

pub fn document() -> Definition {
    Definition::new("document", |bytes| document_frames(bytes))
}

pub fn sse_frames(bytes: ByteStream, events: Option<BTreeSet<String>>, include_done: bool) -> FrameStream {
    drive(bytes, SseDecoder { utf8: Utf8Decoder::default(), parser: SseParser::default(), events, include_done })
}

pub fn line_frames(bytes: ByteStream) -> FrameStream {
    drive(bytes, LineDecoder::default())
}

pub fn document_frames(bytes: ByteStream) -> FrameStream {
    drive(bytes, DocumentDecoder::default())
}

trait Decoder: Send + 'static {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, AiError>;
    fn finish(&mut self) -> Result<Vec<String>, AiError>;
}

fn drive<S: Decoder>(bytes: ByteStream, decoder: S) -> FrameStream {
    Box::pin(stream::unfold(
        (bytes, decoder, VecDeque::<Result<String, AiError>>::new(), false),
        |(mut bytes, mut decoder, mut queue, mut done)| async move {
            loop {
                if let Some(item) = queue.pop_front() {
                    return Some((item, (bytes, decoder, queue, done)));
                }
                if done {
                    return None;
                }
                match bytes.next().await {
                    Some(Ok(chunk)) => match decoder.feed(&chunk) {
                        Ok(frames) => queue.extend(frames.into_iter().map(Ok)),
                        Err(error) => {
                            queue.push_back(Err(error));
                            done = true;
                        }
                    },
                    Some(Err(error)) => {
                        queue.push_back(Err(error));
                        done = true;
                    }
                    None => {
                        match decoder.finish() {
                            Ok(frames) => queue.extend(frames.into_iter().map(Ok)),
                            Err(error) => queue.push_back(Err(error)),
                        }
                        done = true;
                    }
                }
            }
        },
    ))
}

#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    fn decode(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        drain_utf8(&mut self.pending, false)
    }

    fn finish(&mut self) -> String {
        drain_utf8(&mut self.pending, true)
    }
}

fn drain_utf8(pending: &mut Vec<u8>, last: bool) -> String {
    let mut output = String::new();
    loop {
        match std::str::from_utf8(pending) {
            Ok(valid) => {
                output.push_str(valid);
                pending.clear();
                break;
            }
            Err(error) => {
                let valid_up_to = error.valid_up_to();
                if valid_up_to > 0 {
                    output.push_str(std::str::from_utf8(&pending[..valid_up_to]).unwrap_or_default());
                }
                match error.error_len() {
                    Some(length) => {
                        output.push('\u{fffd}');
                        pending.drain(..valid_up_to + length);
                    }
                    None => {
                        pending.drain(..valid_up_to);
                        break;
                    }
                }
            }
        }
    }
    if last && !pending.is_empty() {
        pending.clear();
        output.push('\u{fffd}');
    }
    output
}

struct SseParser {
    is_first_chunk: bool,
    pending: String,
    pending_length: usize,
    discard_trailing_newline: bool,
    event_name: Option<String>,
    data: String,
}

struct SseEvent {
    name: String,
    data: String,
}

impl SseParser {
    fn feed(&mut self, chunk: &str) -> Result<Vec<SseEvent>, String> {
        if chunk.is_empty() {
            return Ok(Vec::new());
        }
        let chunk = if self.is_first_chunk && chunk.starts_with(BOM) { &chunk[BOM.len_utf8()..] } else { chunk };
        self.is_first_chunk = false;
        let mut events = Vec::new();
        let bytes = chunk.as_bytes();
        let length = bytes.len();
        let mut position = 0;
        while position < length {
            if self.discard_trailing_newline {
                self.discard_trailing_newline = false;
                if bytes[position] == b'\n' {
                    position += 1;
                    continue;
                }
            }
            let mut line_end = position;
            while line_end < length && bytes[line_end] != b'\r' && bytes[line_end] != b'\n' {
                line_end += 1;
            }
            if line_end == length {
                self.pending.push_str(&chunk[position..]);
                self.pending_length += length - position;
                break;
            }
            self.discard_trailing_newline = bytes[line_end] == b'\r';
            let line = if self.pending.is_empty() {
                chunk[position..line_end].to_string()
            } else {
                self.pending.push_str(&chunk[position..line_end]);
                self.pending_length = 0;
                std::mem::take(&mut self.pending)
            };
            self.parse_line(&line, &mut events);
            position = line_end + 1;
        }
        if self.pending_length + self.data.len() > MAX_EVENT_SIZE {
            self.reset();
            return Err(format!("Pending SSE event exceeded the maximum size of {MAX_EVENT_SIZE}"));
        }
        Ok(events)
    }

    fn parse_line(&mut self, line: &str, events: &mut Vec<SseEvent>) {
        if line.is_empty() {
            let name = match self.event_name.take() {
                Some(name) if !name.is_empty() => name,
                _ => "message".to_string(),
            };
            let data = std::mem::take(&mut self.data);
            if !data.is_empty() {
                let trimmed = data.strip_suffix('\n').unwrap_or(&data).to_string();
                events.push(SseEvent { name, data: trimmed });
            }
            return;
        }
        let (field, value) = match line.find(':') {
            Some(colon) => {
                let value = &line[colon + 1..];
                (&line[..colon], value.strip_prefix(' ').unwrap_or(value))
            }
            None => (line, ""),
        };
        if field == "data" {
            self.data.push_str(value);
            self.data.push('\n');
        } else if field == "event" {
            self.event_name = Some(value.to_string());
        }
    }

    fn reset(&mut self) {
        self.is_first_chunk = true;
        self.pending.clear();
        self.pending_length = 0;
        self.discard_trailing_newline = false;
        self.event_name = None;
        self.data.clear();
    }
}

impl Default for SseParser {
    fn default() -> Self {
        Self {
            is_first_chunk: true,
            pending: String::new(),
            pending_length: 0,
            discard_trailing_newline: false,
            event_name: None,
            data: String::new(),
        }
    }
}

struct SseDecoder {
    utf8: Utf8Decoder,
    parser: SseParser,
    events: Option<BTreeSet<String>>,
    include_done: bool,
}

impl SseDecoder {
    fn process(&mut self, text: &str) -> Result<Vec<String>, AiError> {
        let events = match self.parser.feed(text) {
            Ok(events) => events,
            Err(message) => {
                return Err(AiError {
                    reason: AiErrorReason::InvalidProviderOutput {
                        message,
                        body: Some(text.to_string()),
                        http: None,
                        classification: None,
                        route: Some("sse".to_string()),
                    },
                });
            }
        };
        Ok(events.into_iter().filter_map(|event| self.accept(event)).collect())
    }

    fn accept(&self, event: SseEvent) -> Option<String> {
        if let Some(events) = &self.events
            && !events.contains(&event.name)
        {
            return None;
        }
        if event.data.is_empty() || event.data == "null" || event.data == ": keepalive" {
            return None;
        }
        if event.data == "[DONE]" && !self.include_done && !(self.events.is_some() && event.name != "message") {
            return None;
        }
        Some(event.data)
    }
}

impl Decoder for SseDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, AiError> {
        let text = self.utf8.decode(bytes);
        if text.is_empty() {
            return Ok(Vec::new());
        }
        self.process(&text)
    }

    fn finish(&mut self) -> Result<Vec<String>, AiError> {
        let text = self.utf8.finish();
        if text.is_empty() {
            return Ok(Vec::new());
        }
        self.process(&text)
    }
}

#[derive(Default)]
struct LineDecoder {
    utf8: Utf8Decoder,
    pending: String,
    mid_crlf: bool,
}

impl LineDecoder {
    fn split(&mut self, text: &str) -> Vec<String> {
        let mut lines = Vec::new();
        let bytes = text.as_bytes();
        let length = bytes.len();
        let mut from = 0;
        let mut position = 0;
        if self.mid_crlf && bytes.first() == Some(&b'\n') {
            from = 1;
            position = 1;
        }
        self.mid_crlf = false;
        while position < length {
            match bytes[position] {
                b'\n' => {
                    push_line(&mut self.pending, &mut lines, &text[from..position]);
                    from = position + 1;
                }
                b'\r' => {
                    push_line(&mut self.pending, &mut lines, &text[from..position]);
                    if position + 1 == length {
                        self.mid_crlf = true;
                        from = length;
                    } else if bytes[position + 1] == b'\n' {
                        position += 1;
                        from = position + 1;
                    } else {
                        from = position + 1;
                    }
                }
                _ => {}
            }
            position += 1;
        }
        self.pending.push_str(&text[from..]);
        lines
    }
}

fn push_line(pending: &mut String, lines: &mut Vec<String>, segment: &str) {
    if pending.is_empty() {
        lines.push(segment.to_string());
    } else {
        let mut line = std::mem::take(pending);
        line.push_str(segment);
        lines.push(line);
    }
}

impl Decoder for LineDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, AiError> {
        let text = self.utf8.decode(bytes);
        Ok(self.split(&text).into_iter().filter(|line| !line.trim().is_empty()).collect())
    }

    fn finish(&mut self) -> Result<Vec<String>, AiError> {
        let text = self.utf8.finish();
        let mut lines = self.split(&text);
        if !self.pending.is_empty() {
            lines.push(std::mem::take(&mut self.pending));
        }
        Ok(lines.into_iter().filter(|line| !line.trim().is_empty()).collect())
    }
}

#[derive(Default)]
struct DocumentDecoder {
    utf8: Utf8Decoder,
    text: String,
}

impl Decoder for DocumentDecoder {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>, AiError> {
        self.text.push_str(&self.utf8.decode(bytes));
        Ok(Vec::new())
    }

    fn finish(&mut self) -> Result<Vec<String>, AiError> {
        self.text.push_str(&self.utf8.finish());
        Ok(vec![std::mem::take(&mut self.text)])
    }
}
