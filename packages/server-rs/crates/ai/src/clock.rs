use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_millis() as u64)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FixedClock {
    now_ms: u64,
}

impl FixedClock {
    pub fn new(now_ms: u64) -> Self {
        Self { now_ms }
    }
}

impl Clock for FixedClock {
    fn now_ms(&self) -> u64 {
        self.now_ms
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcDateTime {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millisecond: u32,
}

pub fn utc_datetime(epoch_ms: i64) -> UtcDateTime {
    let days = epoch_ms.div_euclid(86_400_000);
    let remainder = epoch_ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    UtcDateTime {
        year,
        month,
        day,
        hour: (remainder / 3_600_000) as u32,
        minute: (remainder / 60_000 % 60) as u32,
        second: (remainder / 1000 % 60) as u32,
        millisecond: (remainder % 1000) as u32,
    }
}

impl UtcDateTime {
    pub fn amz(&self) -> String {
        format!(
            "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    pub fn iso_millis(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second, self.millisecond
        )
    }
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 { shifted } else { shifted - 146_096 } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 { month_prime + 3 } else { month_prime - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u32, day as u32)
}
