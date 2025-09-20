#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use core::cell::RefCell;
use embedded_hal_bus::spi::RefCellDevice;
use embedded_sdmmc::{Mode as FileMode, SdCard, VolumeIdx, VolumeManager};
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::{
    clock::CpuClock,
    delay::Delay as EspHalDelay,
    rng::Rng,
    spi::master::{Config as SpiMasterConfig, Spi as SpiMaster},
    spi::Mode as SpiMode,
};
use esp_println::println;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};

// Import our utility functions from the library
use esp32_sdcard::{
    format_csv_line, generate_random_filename, retry_with_backoff, DummyTimeSource,
};

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal_embassy::main]
async fn main(_spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    let timer0 = TimerGroup::new(peripherals.TIMG1);
    esp_hal_embassy::init(timer0.timer0);

    println!("\n==============================");
    println!("ESP32 SD Card Counter Example");
    println!("==============================\n");

    // Generate random filename for CSV file
    let mut rng = Rng::new(peripherals.RNG);
    let mut filename = [0u8; 12];
    generate_random_filename(&mut rng, &mut filename);
    let filename_str = core::str::from_utf8(&filename).unwrap();
    println!("Generated filename: {}", filename_str);

    // === SPI Bus Setup ===
    println!("Setting up SPI bus for SD card...");
    let spi2 = peripherals.SPI2;
    let cs = Output::new(peripherals.GPIO18, Level::High, OutputConfig::default()); // CS pin
                                                                                    // With this setup, you could add a second SPI device on this same bus with a second CS pin
    let sclk = peripherals.GPIO19; // Serial Clock
    let mosi = peripherals.GPIO23; // Master Out Slave In
    let miso = peripherals.GPIO21; // Master In Slave Out

    // Start with low frequency for initialization
    let spi_bus_config = SpiMasterConfig::default()
        .with_frequency(Rate::from_khz(400)) // 400kHz for initialization
        .with_mode(SpiMode::_0);

    let spi_bus = SpiMaster::new(spi2, spi_bus_config)
        .expect("Failed to initialize SPI bus")
        .with_miso(miso)
        .with_mosi(mosi)
        .with_sck(sclk);

    let shared_spi_bus = RefCell::new(spi_bus);
    let spi_device = RefCellDevice::new(&shared_spi_bus, cs, EspHalDelay::new())
        .expect("Failed to create SPI device");
    // Here is where you would create a second SPI device (i.e. if you have a second SD card)

    println!("    SPI bus configured");

    // Initialize SD card with retry logic
    let sdcard = SdCard::new(spi_device, EspHalDelay::new());
    println!("Initializing SD Card...");
    let sd_size =
        retry_with_backoff("SD Card initialization", || async { sdcard.num_bytes() }).await;
    if let Some(num_bytes) = sd_size {
        println!(
            "    SD Card ready - size: {} GB",
            num_bytes / 1024 / 1024 / 1024
        );
    } else {
        println!("    SD Card initialization failed");
    }

    // Open volume 0 (main partition)
    let volume_mgr = VolumeManager::new(sdcard, DummyTimeSource);
    let volume0 = if sd_size.is_some() {
        retry_with_backoff("Opening volume 0", || async {
            volume_mgr.open_volume(VolumeIdx(0))
        })
        .await
    } else {
        None
    };
    if volume0.is_some() {
        println!("    Volume 0 opened");
    }

    // Open root directory
    let root_dir = if let Some(ref volume) = volume0 {
        retry_with_backoff("Opening root directory", || async {
            volume.open_root_dir()
        })
        .await
    } else {
        None
    };
    if root_dir.is_some() {
        println!("    Root directory opened");
    }

    // After initializing the SD card, increase the SPI frequency
    shared_spi_bus
        .borrow_mut()
        .apply_config(
            &SpiMasterConfig::default()
                .with_frequency(Rate::from_mhz(2))
                .with_mode(SpiMode::_0),
        )
        .expect("Failed to apply the second SPI configuration");

    // Create CSV file
    let filename_str = core::str::from_utf8(&filename).unwrap();
    let mut file = if let Some(ref root_dir) = root_dir {
        retry_with_backoff("Creating CSV file", || async {
            root_dir.open_file_in_dir(filename_str, FileMode::ReadWriteCreateOrAppend)
        })
        .await
    } else {
        None
    };

    if file.is_some() {
        println!("    CSV file '{}' created", filename_str);
    }

    // Write CSV header
    if let Some(ref mut f) = file {
        let header_written = retry_with_backoff("Writing CSV header", || async {
            f.write(b"Timestamp,Counter,Value\n")
        })
        .await;

        if header_written.is_some() {
            println!("    CSV header written");
        } else {
            // If header write failed, disable file writing
            file = None;
        }
    }

    // Main counting loop
    let mut counter = 0u32;
    println!("Starting counter loop...\n");

    loop {
        counter += 1;
        let timestamp = embassy_time::Instant::now().as_millis();

        println!("Counter: {}", counter);

        // Write to SD card file if available
        if let Some(ref mut file) = file {
            let mut buffer = [0u8; 64];
            let line_length = format_csv_line(&mut buffer, timestamp, counter);

            match file.write(&buffer[..line_length]) {
                Ok(_) => {
                    // Don't forget to flush the file occasionally so that the directory entry is updated
                    if counter % 10 == 0 {
                        let _ = file.flush();
                        println!("    Flushed data to SD card (count: {})", counter);
                    }
                }
                Err(e) => {
                    println!("    SD write error: {:?}", e);
                }
            }
        }

        // Wait 1 second before next count
        Timer::after(Duration::from_secs(1)).await;
    }
}
