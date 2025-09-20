#![no_std]

//! ESP32 SD Card utilities and helpers
//!
//! This library provides common utilities for working with SD cards on ESP32,
//! including retry logic, time sources, and formatting helpers.

use embassy_time::{Duration, Timer};
use esp_hal::rng::Rng;

/// Maximum number of retries for SD card operations
pub const MAX_RETRIES: u8 = 4;

/// Retry operations with 500ms backoff, useful for SD card initialization
pub async fn retry_with_backoff<T, E, F, Fut>(operation_name: &str, mut operation: F) -> Option<T>
where
    F: FnMut() -> Fut,
    Fut: core::future::Future<Output = Result<T, E>>,
    E: core::fmt::Debug,
{
    for attempt in 1..=MAX_RETRIES {
        match operation().await {
            Ok(result) => return Some(result),
            Err(e) => {
                esp_println::println!(
                    "{} failed: {:?} - Retry {}/{}",
                    operation_name,
                    e,
                    attempt,
                    MAX_RETRIES
                );
                if attempt >= MAX_RETRIES {
                    esp_println::println!(
                        "{} failed after {} retries",
                        operation_name,
                        MAX_RETRIES
                    );
                    return None;
                }
                Timer::after(Duration::from_millis(500)).await;
            }
        }
    }
    None
}

/// Dummy time source for embedded-sdmmc (use RTC for real timestamps)
pub struct DummyTimeSource;

impl embedded_sdmmc::TimeSource for DummyTimeSource {
    fn get_timestamp(&self) -> embedded_sdmmc::Timestamp {
        embedded_sdmmc::Timestamp {
            year_since_1970: 0,
            zero_indexed_month: 0,
            zero_indexed_day: 0,
            hours: 0,
            minutes: 0,
            seconds: 0,
        }
    }
}

/// Generate random 8.3 filename (e.g., "ABC12345.CSV")
/// Note: This is the max length for a filename in this filesystem.
pub fn generate_random_filename(rng: &mut Rng, filename: &mut [u8; 12]) {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    for i in 0..8 {
        let idx = (rng.random() as usize) % CHARS.len();
        filename[i] = CHARS[idx];
    }
    filename[8] = b'.';
    filename[9] = b'C';
    filename[10] = b'S';
    filename[11] = b'V';
}

/// Format CSV line as "timestamp,count,counter\n", returns bytes written
pub fn format_csv_line(buffer: &mut [u8], timestamp: u64, counter: u32) -> usize {
    let mut cursor = 0;

    // Format the values
    let mut timestamp_buf = itoa::Buffer::new();
    let timestamp_str = timestamp_buf.format(timestamp);
    let mut counter_buf = itoa::Buffer::new();
    let counter_str = counter_buf.format(counter);

    // Write: timestamp,count,counter\n
    let parts = [
        timestamp_str.as_bytes(),
        b",count,",
        counter_str.as_bytes(),
        b"\n",
    ];

    for part in &parts {
        for &byte in *part {
            if cursor < buffer.len() {
                buffer[cursor] = byte;
                cursor += 1;
            } else {
                break;
            }
        }
    }

    cursor
}
