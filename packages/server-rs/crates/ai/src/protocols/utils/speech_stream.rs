use std::pin::Pin;
use std::sync::LazyLock;

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use futures_util::{Stream, StreamExt};
use serde_json::Number;

use crate::media::protocol::Identity;
use crate::modalities::image::MediaUsage;
use crate::modalities::speech::{SpeechEvent, SpeechTimestamp, SpeechVoice};
use crate::protocols::shared::number_from_f64;
use crate::protocols::utils::media_type::detect_media_type;
use crate::route::executor::ByteStream;
use crate::schema::{AiError, MediaAsset, MediaInfo, MediaSource, ProviderMetadata};

pub enum Frame {
    Text(String),
    Bytes(Vec<u8>),
}

pub type FrameStream = Pin<Box<dyn Stream<Item = Result<Frame, AiError>> + Send>>;

pub fn sse(bytes: ByteStream) -> FrameStream {
    Box::pin(crate::route::framing::sse_frames(bytes, None, false).map(|item| item.map(Frame::Text)))
}

pub fn lines(bytes: ByteStream) -> FrameStream {
    Box::pin(crate::route::framing::line_frames(bytes).map(|item| item.map(Frame::Text)))
}

pub fn document(bytes: ByteStream) -> FrameStream {
    Box::pin(crate::route::framing::document_frames(bytes).map(|item| item.map(Frame::Text)))
}

pub fn bytes(bytes: ByteStream) -> FrameStream {
    Box::pin(bytes.map(|item| item.map(|chunk| Frame::Bytes(chunk.to_vec()))))
}

#[derive(Debug, Clone, Default)]
pub struct Audio {
    pub chunks: Vec<Vec<u8>>,
}

pub fn delta(audio: &mut Audio, chunk: &[u8]) -> Vec<SpeechEvent> {
    if chunk.is_empty() {
        return Vec::new();
    }
    audio.chunks.push(chunk.to_vec());
    vec![SpeechEvent::AudioDelta { chunk: BASE64_STANDARD.encode(chunk) }]
}

pub fn timestamps(texts: &[String], starts: &[Number], ends: &[Number]) -> Vec<SpeechEvent> {
    if texts.is_empty() {
        return Vec::new();
    }
    let items = texts
        .iter()
        .enumerate()
        .map(|(index, text)| SpeechTimestamp {
            text: text.clone(),
            start_seconds: starts.get(index).cloned().unwrap_or_else(|| Number::from(0)),
            end_seconds: ends.get(index).cloned().unwrap_or_else(|| Number::from(0)),
        })
        .collect();
    vec![SpeechEvent::Timestamps { items }]
}

pub fn voice_id(voice: Option<&SpeechVoice>) -> Option<String> {
    voice.map(|voice| voice.id().to_string())
}

pub struct SpeechOutput {
    pub media_type: Option<String>,
    pub info: Option<MediaInfo>,
    pub usage: Option<MediaUsage>,
    pub provider_metadata: Option<ProviderMetadata>,
    pub detail: Option<String>,
}

pub fn container(format: &str, sample_rate: Option<Number>) -> SpeechOutput {
    let media_type = match format {
        "mp3" => Some("audio/mpeg"),
        "wav" => Some("audio/wav"),
        "opus" => Some("audio/ogg"),
        "aac" => Some("audio/aac"),
        "flac" => Some("audio/flac"),
        _ => None,
    };
    SpeechOutput {
        media_type: media_type.map(str::to_string),
        info: Some(MediaInfo { format: Some(format.to_string()), sample_rate, ..MediaInfo::default() }),
        usage: None,
        provider_metadata: None,
        detail: None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcmEncoding {
    S16le,
    F32le,
    Mulaw,
    Alaw,
}

impl PcmEncoding {
    pub fn as_str(&self) -> &'static str {
        match self {
            PcmEncoding::S16le => "pcm_s16le",
            PcmEncoding::F32le => "pcm_f32le",
            PcmEncoding::Mulaw => "pcm_mulaw",
            PcmEncoding::Alaw => "pcm_alaw",
        }
    }
}

pub fn pcm(encoding: PcmEncoding, sample_rate: Option<Number>, media_type: Option<String>) -> SpeechOutput {
    let media_type = media_type.or_else(|| {
        let media_type = match encoding {
            PcmEncoding::S16le | PcmEncoding::F32le => "audio/pcm",
            PcmEncoding::Mulaw => "audio/mulaw",
            PcmEncoding::Alaw => "audio/alaw",
        };
        Some(media_type.to_string())
    });
    SpeechOutput {
        media_type,
        info: Some(MediaInfo {
            format: Some("pcm".to_string()),
            encoding: Some(encoding.as_str().to_string()),
            sample_rate,
            channels: Some(Number::from(1)),
            ..MediaInfo::default()
        }),
        usage: None,
        provider_metadata: None,
        detail: None,
    }
}

pub fn sample_rate(media_type: Option<&str>) -> Option<Number> {
    static RATE: LazyLock<regex_lite::Regex> =
        LazyLock::new(|| regex_lite::Regex::new(r"(?i)rate=(\d+)").expect("rate pattern compiles"));
    let rate = RATE.captures(media_type?)?.get(1)?.as_str();
    rate.parse::<f64>().ok().and_then(number_from_f64)
}

pub fn finish(identity: &Identity, audio: &Audio, output: SpeechOutput) -> Result<Vec<SpeechEvent>, AiError> {
    if audio.chunks.is_empty() {
        let detail = match output.detail {
            Some(detail) => format!(" ({detail})"),
            None => String::new(),
        };
        return Err(identity.frame_error(format!("The provider returned no audio{detail}"), None));
    }
    let bytes = audio.chunks.concat();
    let media_type = output
        .media_type
        .or_else(|| detect_media_type(&bytes).map(str::to_string))
        .unwrap_or_else(|| "application/octet-stream".to_string());
    let audio = MediaAsset {
        source: MediaSource::Bytes { data: BASE64_STANDARD.encode(&bytes), media_type },
        info: output.info,
        provider_metadata: None,
        headers: None,
    };
    Ok(vec![SpeechEvent::Finish { audio, usage: output.usage, provider_metadata: output.provider_metadata }])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderUsageKind {
    Characters,
    Credits,
}

pub fn header_usage(kind: HeaderUsageKind, value: Option<&str>) -> Option<MediaUsage> {
    let amount = value?.parse::<f64>().ok().filter(|amount| amount.is_finite())?;
    let amount = number_from_f64(amount)?;
    Some(match kind {
        HeaderUsageKind::Characters => MediaUsage::Characters { characters: amount },
        HeaderUsageKind::Credits => MediaUsage::Credits { credits: amount },
    })
}
