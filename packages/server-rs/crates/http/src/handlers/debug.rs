use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use http::StatusCode;
use opencode_contract::codec::encode_json;
use opencode_contract::generated::operations::{debug_heap_dump, debug_location_evict};
use opencode_contract::generated::{
    AbsolutePath, DebugHeapDumpResponse, DebugHeapDumpResponseMemory, DebugHeapDumpResponseMemoryArrayBuffers,
    DebugHeapDumpResponseMemoryArrayBuffersNumber, DebugHeapDumpResponseMemoryExternal,
    DebugHeapDumpResponseMemoryExternalNumber, DebugHeapDumpResponseMemoryHeapTotal,
    DebugHeapDumpResponseMemoryHeapTotalNumber, DebugHeapDumpResponseMemoryHeapUsed,
    DebugHeapDumpResponseMemoryHeapUsedNumber, DebugHeapDumpResponseMemoryRss, DebugHeapDumpResponseMemoryRssNumber,
    InvalidRequestError, InvalidRequestErrorTag, LocationPublicRef, UnknownError, UnknownErrorTag,
};
use opencode_contract::request::RawRequest;

use crate::handlers::{defect, invalid};
use crate::response;
use crate::services::GlobalServices;

static HEAP_DUMP_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub async fn heap_dump(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    let query = match debug_heap_dump::decode_query(raw) {
        Ok(query) => query,
        Err(error) => return invalid::request(invalid::Kind::Query, error),
    };
    let requested = query.directory.flatten();
    if let Some(directory) = &requested
        && !Path::new(directory).is_absolute()
    {
        return invalid_request("directory must be an absolute path", "directory");
    }
    let directory = requested.map_or_else(|| services.paths.log.clone(), PathBuf::from);
    let _guard = HEAP_DUMP_LOCK.lock().await;
    let written = tokio::task::spawn_blocking(move || write_report(&directory))
        .await
        .unwrap_or_else(|error| Err(std::io::Error::other(error)));
    match written {
        Ok(report) => response::bytes(StatusCode::OK, "application/json", encode_json(&report)),
        Err(error) => {
            let reference = defect::reference();
            log::error!("failed to write heap snapshot ref={reference}: {error}");
            response::bytes(
                StatusCode::INTERNAL_SERVER_ERROR,
                "application/json",
                encode_json(&UnknownError {
                    tag: UnknownErrorTag::UnknownError,
                    message: format!("Failed to write heap snapshot: {error}"),
                    r#ref: Some(Some(reference)),
                }),
            )
        }
    }
}

pub async fn location_list(services: &GlobalServices) -> response::Response {
    let locations = services
        .locations
        .keys()
        .await
        .into_iter()
        .map(|key| LocationPublicRef { directory: AbsolutePath(key.directory) })
        .collect::<Vec<_>>();
    response::bytes(StatusCode::OK, "application/json", encode_json(&locations))
}

pub async fn location_evict(services: &GlobalServices, raw: &RawRequest) -> response::Response {
    if let Err(error) = debug_location_evict::decode_query(raw) {
        return invalid::request(invalid::Kind::Query, error);
    }
    let reference = opencode_location::resolve::request_ref(raw, &services.start_directory);
    services.locations.invalidate(&reference).await;
    response::empty(StatusCode::NO_CONTENT)
}

fn write_report(directory: &Path) -> std::io::Result<DebugHeapDumpResponse> {
    std::fs::create_dir_all(directory)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut millis = now_millis();
    let (mut file, path) = loop {
        let path = directory.join(format!("heap-{}-{}.json", std::process::id(), compact_iso_time(millis)));
        match options.open(&path) {
            Ok(file) => break (file, path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => millis += 1,
            Err(error) => return Err(error),
        }
    };
    let rss = resident_bytes();
    let report = serde_json::json!({
        "pid": std::process::id(),
        "rss": rss,
        "status": status_fields(),
    });
    file.write_all(&serde_json::to_vec_pretty(&report).map_err(std::io::Error::other)?)?;
    Ok(DebugHeapDumpResponse {
        path: path.to_string_lossy().into_owned(),
        pid: f64::from(std::process::id()),
        memory: DebugHeapDumpResponseMemory {
            rss: DebugHeapDumpResponseMemoryRss::Number(DebugHeapDumpResponseMemoryRssNumber(rss as f64)),
            heap_total: DebugHeapDumpResponseMemoryHeapTotal::Number(DebugHeapDumpResponseMemoryHeapTotalNumber(0.0)),
            heap_used: DebugHeapDumpResponseMemoryHeapUsed::Number(DebugHeapDumpResponseMemoryHeapUsedNumber(0.0)),
            external: DebugHeapDumpResponseMemoryExternal::Number(DebugHeapDumpResponseMemoryExternalNumber(0.0)),
            array_buffers: DebugHeapDumpResponseMemoryArrayBuffers::Number(
                DebugHeapDumpResponseMemoryArrayBuffersNumber(0.0),
            ),
        },
    })
}

#[cfg(not(windows))]
fn resident_bytes() -> u64 {
    let Ok(statm) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    let Some(pages) = statm.split_whitespace().nth(1).and_then(|value| value.parse::<u64>().ok()) else {
        return 0;
    };
    pages.saturating_mul(rustix::param::page_size() as u64)
}

#[cfg(windows)]
fn resident_bytes() -> u64 {
    0
}

fn status_fields() -> serde_json::Map<String, serde_json::Value> {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return serde_json::Map::new();
    };
    status
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.to_owned(), serde_json::Value::String(value.trim().to_owned())))
        })
        .collect()
}

fn now_millis() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

fn compact_iso_time(millis: i64) -> String {
    let seconds = millis.div_euclid(1000);
    let milliseconds = millis.rem_euclid(1000);
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let seconds_of_day = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}{:02}{:02}{milliseconds:03}Z",
        seconds_of_day / 3600,
        (seconds_of_day % 3600) / 60,
        seconds_of_day % 60,
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 { shifted } else { shifted - 146_096 } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_position + 2) / 5 + 1) as u32;
    let month = if month_position < 10 { month_position + 3 } else { month_position - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn invalid_request(message: &str, field: &str) -> response::Response {
    response::bytes(
        StatusCode::BAD_REQUEST,
        "application/json",
        encode_json(&InvalidRequestError {
            tag: InvalidRequestErrorTag::InvalidRequestError,
            message: message.to_owned(),
            kind: None,
            field: Some(Some(field.to_owned())),
        }),
    )
}
