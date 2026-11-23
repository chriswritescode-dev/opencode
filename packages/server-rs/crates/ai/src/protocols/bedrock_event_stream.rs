use std::collections::VecDeque;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use futures_util::{StreamExt, stream};
use serde_json::{Map, Value, json};

use crate::json::encode_body;
use crate::protocols::shared;
use crate::protocols::utils::crc32::crc32;
use crate::route::framing::Definition;
use crate::schema::{AiError, AiErrorReason, InvalidProviderOutputClassification};

pub const ID: &str = "aws-event-stream";

const PRELUDE_END: usize = 12;
const CHECKSUM_LENGTH: usize = 4;
const MINIMUM_MESSAGE_LENGTH: usize = PRELUDE_END + CHECKSUM_LENGTH;

pub fn framing(route: impl Into<String>) -> Definition {
    let route = route.into();
    Definition::new(ID, move |bytes| {
        Box::pin(stream::unfold(
            (bytes, EventStreamDecoder::new(route.clone()), VecDeque::<Result<String, AiError>>::new(), false),
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
                            if let Err(error) = decoder.finish() {
                                queue.push_back(Err(error));
                            }
                            done = true;
                        }
                    }
                }
            },
        ))
    })
}

struct EventStreamDecoder {
    buffer: Vec<u8>,
    offset: usize,
    route: String,
}

impl EventStreamDecoder {
    fn new(route: String) -> Self {
        Self { buffer: Vec::new(), offset: 0, route }
    }

    fn feed(&mut self, chunk: &[u8]) -> Result<Vec<String>, AiError> {
        self.append(chunk);
        let mut frames = Vec::new();
        while self.remaining() >= 4 {
            let start = self.offset;
            let total_length = be_u32(&self.buffer[start..]) as usize;
            let end = start + total_length;
            if end > self.buffer.len() {
                break;
            }
            let decoded = match decode_frame(&self.buffer[start..end]) {
                Ok(decoded) => decoded,
                Err(detail) => return Err(decode_error(&detail, &self.buffer[start..end], &self.route)),
            };
            self.offset = end;
            if let Some(frame) = frame_event(decoded, &self.route)? {
                frames.push(frame);
            }
        }
        Ok(frames)
    }

    fn finish(&self) -> Result<(), AiError> {
        if self.remaining() == 0 {
            return Ok(());
        }
        Err(incomplete_error(&self.buffer[self.offset..], &self.route))
    }

    fn remaining(&self) -> usize {
        self.buffer.len() - self.offset
    }

    fn append(&mut self, chunk: &[u8]) {
        if self.offset > 0 {
            self.buffer.drain(..self.offset);
            self.offset = 0;
        }
        self.buffer.extend_from_slice(chunk);
    }
}

struct DecodedFrame {
    headers: Map<String, Value>,
    payload: Vec<u8>,
}

fn decode_frame(frame: &[u8]) -> Result<DecodedFrame, String> {
    if frame.len() < MINIMUM_MESSAGE_LENGTH {
        return Err("Provided message too short to accommodate event stream message overhead".to_string());
    }
    let headers_length = be_u32(&frame[4..]) as usize;
    let expected_prelude_checksum = be_u32(&frame[8..]);
    let expected_message_checksum = be_u32(&frame[frame.len() - CHECKSUM_LENGTH..]);
    let prelude_checksum = crc32(&frame[..8]);
    if prelude_checksum != expected_prelude_checksum {
        return Err(format!(
            "The prelude checksum specified in the message ({expected_prelude_checksum}) does not match the calculated CRC32 checksum ({prelude_checksum})"
        ));
    }
    let message_checksum = crc32(&frame[..frame.len() - CHECKSUM_LENGTH]);
    if message_checksum != expected_message_checksum {
        return Err(format!(
            "The message checksum ({message_checksum}) did not match the expected value of {expected_message_checksum}"
        ));
    }
    let available = frame.len() - MINIMUM_MESSAGE_LENGTH;
    if headers_length > available {
        return Err(format!("Header length {headers_length} exceeds the {available} bytes available in the frame"));
    }
    let headers = parse_headers(&frame[PRELUDE_END..PRELUDE_END + headers_length])?;
    let payload = frame[PRELUDE_END + headers_length..frame.len() - CHECKSUM_LENGTH].to_vec();
    Ok(DecodedFrame { headers, payload })
}

struct HeaderCursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> HeaderCursor<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self.position + length;
        let slice = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| format!("Header read of {length} bytes at offset {} is out of bounds", self.position))?;
        self.position = end;
        Ok(slice)
    }

    fn unsigned(&mut self, length: usize) -> Result<u64, String> {
        Ok(self.take(length)?.iter().fold(0u64, |value, byte| (value << 8) | u64::from(*byte)))
    }

    fn signed(&mut self, length: usize) -> Result<i64, String> {
        let value = self.unsigned(length)?;
        let shift = 64 - length * 8;
        Ok(((value << shift) as i64) >> shift)
    }
}

fn parse_headers(bytes: &[u8]) -> Result<Map<String, Value>, String> {
    let mut cursor = HeaderCursor { bytes, position: 0 };
    let mut headers = Map::new();
    while cursor.position < bytes.len() {
        let name_length = cursor.unsigned(1)? as usize;
        let name = String::from_utf8_lossy(cursor.take(name_length)?).into_owned();
        let tag = cursor.unsigned(1)?;
        let value = match tag {
            0 => header_value("boolean", Value::Bool(true)),
            1 => header_value("boolean", Value::Bool(false)),
            2 => header_value("byte", Value::from(cursor.signed(1)?)),
            3 => header_value("short", Value::from(cursor.signed(2)?)),
            4 => header_value("integer", Value::from(cursor.signed(4)?)),
            5 => header_value("long", Value::from(cursor.signed(8)?)),
            6 => {
                let length = cursor.unsigned(2)? as usize;
                let items = cursor.take(length)?.iter().map(|byte| Value::from(*byte)).collect();
                header_value("binary", Value::Array(items))
            }
            7 => {
                let length = cursor.unsigned(2)? as usize;
                header_value("string", Value::String(String::from_utf8_lossy(cursor.take(length)?).into_owned()))
            }
            8 => header_value("timestamp", Value::String(crate::clock::utc_datetime(cursor.signed(8)?).iso_millis())),
            9 => header_value("uuid", Value::String(uuid_string(cursor.take(16)?))),
            other => return Err(format!("Unrecognized header type tag {other}")),
        };
        headers.insert(name, value);
    }
    Ok(headers)
}

fn header_value(tag: &str, value: Value) -> Value {
    let mut header = Map::new();
    header.insert("type".to_string(), Value::String(tag.to_string()));
    header.insert("value".to_string(), value);
    Value::Object(header)
}

fn uuid_string(bytes: &[u8]) -> String {
    let hex = bytes.iter().fold(String::with_capacity(32), |mut hex, byte| {
        hex.push_str(&format!("{byte:02x}"));
        hex
    });
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

fn frame_event(decoded: DecodedFrame, route: &str) -> Result<Option<String>, AiError> {
    let payload = String::from_utf8_lossy(&decoded.payload).into_owned();
    let message_type = header_string(&decoded.headers, ":message-type").map(str::to_string);
    let error_code = header_string(&decoded.headers, ":error-code").map(str::to_string);
    let error_message = header_string(&decoded.headers, ":error-message").map(str::to_string);
    let event_type = match message_type.as_deref() {
        Some("event") => header_string(&decoded.headers, ":event-type"),
        Some("exception") => header_string(&decoded.headers, ":exception-type"),
        _ => None,
    }
    .map(str::to_string);
    let raw_body = encode_body(&json!({"headers": Value::Object(decoded.headers), "body": &payload}));
    if message_type.as_deref() == Some("error") {
        let message = [error_code, error_message].into_iter().flatten().collect::<Vec<String>>().join(": ");
        let message = if message.is_empty() { "Bedrock Converse event-stream error".to_string() } else { message };
        return Err(shared::event_error(route, message, Some(raw_body)));
    }
    let Some(event_type) = event_type else {
        return Ok(None);
    };
    if payload.is_empty() {
        return Ok(None);
    }
    let Some(Value::Object(mut event)) = serde_json::from_str::<Value>(&payload).ok() else {
        return Err(shared::event_error(
            route,
            "Failed to parse Bedrock Converse event-stream payload",
            Some(raw_body),
        ));
    };
    event.shift_remove("p");
    let mut frame = Map::new();
    if message_type.as_deref() == Some("exception") {
        let mut exception = Map::new();
        exception.insert("type".to_string(), Value::String(event_type));
        exception.insert("details".to_string(), Value::Object(event));
        frame.insert("exception".to_string(), Value::Object(exception));
    } else {
        frame.insert(event_type, Value::Object(event));
    }
    frame.insert("rawBody".to_string(), Value::String(raw_body));
    Ok(Some(encode_body(&Value::Object(frame))))
}

fn header_string<'a>(headers: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    headers.get(name)?.get("value")?.as_str()
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn decode_error(detail: &str, frame: &[u8], route: &str) -> AiError {
    AiError {
        reason: AiErrorReason::InvalidProviderOutput {
            message: format!("Failed to decode Bedrock Converse event-stream frame: {detail}"),
            body: Some(BASE64_STANDARD.encode(frame)),
            http: None,
            classification: None,
            route: Some(route.to_string()),
        },
    }
}

fn incomplete_error(remaining: &[u8], route: &str) -> AiError {
    AiError {
        reason: AiErrorReason::InvalidProviderOutput {
            message: format!(
                "Incomplete Bedrock Converse event-stream frame: {} buffered bytes remain at end of stream",
                remaining.len()
            ),
            body: Some(BASE64_STANDARD.encode(remaining)),
            http: None,
            classification: Some(InvalidProviderOutputClassification::IncompleteStream),
            route: Some(route.to_string()),
        },
    }
}
