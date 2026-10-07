//! Standalone Wi-Fi antenna/RSSI comparison, with no Lattice kernel dependency.
#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use esp_hal::{
    interrupt::software::SoftwareInterruptControl, time::Duration, timer::timg::TimerGroup,
};
use esp_println::println;
use esp_radio::wifi::{
    AuthenticationMethod, Bandwidth, Config, ControllerConfig, CountryInfo, PowerSaveMode,
    SecondaryChannel, WifiController,
    ap::AccessPointConfig,
    scan::{ScanConfig, ScanTypeConfig},
    sta::StationConfig,
};

esp_bootloader_esp_idf::esp_app_desc!();
const CHANNEL: u8 = 4; // 2427 MHz, near the 2425 MHz 802.15.4 channel-15 test.
const TX_POWER_QUARTER_DBM: i8 = 80; // 20 dBm; the Wi-Fi API uses quarter-dBm units.

unsafe extern "C" {
    fn esp_wifi_get_max_tx_power(power: *mut i8) -> i32;
    fn phy_init_param_set(combo: u8);
    fn phy_wifi_enable_set(enabled: u8);
}

#[esp_hal::main]
fn main() -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init(esp_hal::Config::default());
    // The vendor Wi-Fi stack needs its own heap and scheduler. They belong only
    // to this standalone diagnostic image, never to the Lattice kernel.
    esp_alloc::heap_allocator!(size: 128 * 1024);
    let timer = TimerGroup::new(peripherals.TIMG0);
    let irq = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timer.timer0, irq.software_interrupt0);
    // Match upstream esp-hal PR #5776 for this Wi-Fi-only, no-power-save test.
    // Cached esp-phy 0.2.0 and esp-radio beta omit the paired combo/RX calls.
    unsafe { phy_init_param_set(1) };
    let mac = esp_hal::efuse::base_mac_address();
    let bytes = mac.as_bytes();
    let ssid = format!(
        "lattice-rf-{:02x}{:02x}{:02x}",
        bytes[3], bytes[4], bytes[5]
    );
    let receiver = cfg!(feature = "receiver");
    let config = if receiver {
        Config::Station(StationConfig::default().with_auth_method(AuthenticationMethod::None))
    } else {
        Config::AccessPoint(
            AccessPointConfig::default()
                .with_ssid(ssid.as_str())
                .with_channel(CHANNEL)
                .with_max_connections(1),
        )
    };
    let mut wifi = WifiController::new(
        peripherals.WIFI,
        ControllerConfig::default()
            .with_country_info(CountryInfo::from(*b"US"))
            .with_initial_config(config),
    )
    .unwrap();
    wifi.set_power_saving(PowerSaveMode::None).unwrap();
    wifi.set_max_tx_power(TX_POWER_QUARTER_DBM).unwrap();
    let bandwidth = wifi.bandwidths().unwrap().with_2_4(Bandwidth::_20MHz);
    wifi.set_bandwidths(bandwidth).unwrap();
    wifi.set_channel(CHANNEL, SecondaryChannel::None).unwrap();
    unsafe { phy_wifi_enable_set(1) };
    let mut power = 0;
    assert_eq!(unsafe { esp_wifi_get_max_tx_power(&mut power) }, 0);
    assert_eq!(power, TX_POWER_QUARTER_DBM);
    assert_eq!(wifi.channel().unwrap().0, CHANNEL);
    println!(
        "WIFI_READY base_mac={} receiver={} ssid={} channel={} tx_power_quarter_dbm={} bluetooth=off ieee802154=off",
        mac, receiver, ssid, CHANNEL, power
    );
    if !receiver {
        loop {
            esp_rtos::CurrentThreadHandle::get().delay(Duration::from_secs(2));
            println!("WIFI_AP active channel={}", wifi.channel().unwrap().0);
        }
    }
    let scan = ScanConfig::default()
        .with_channel(CHANNEL)
        .with_scan_type(ScanTypeConfig::Passive(Duration::from_millis(500)));
    let mut count = 0;
    let all_channels =
        ScanConfig::default().with_scan_type(ScanTypeConfig::Passive(Duration::from_millis(300)));
    let initial = embassy_futures::block_on(wifi.scan_async(&all_channels)).unwrap();
    let mut nearby = 0;
    let mut strongest = -128;
    for result in initial {
        nearby += 1;
        strongest = strongest.max(result.signal_strength);
    }
    println!("WIFI_SURVEY nearby={} strongest_dbm={}", nearby, strongest);
    loop {
        esp_rtos::CurrentThreadHandle::get()
            .delay(Duration::from_millis(2000 + u64::from(bytes[5])));
        let results = embassy_futures::block_on(wifi.scan_async(&scan)).unwrap();
        let mut peers = 0;
        for result in results {
            if result.ssid.as_str().starts_with("lattice-rf-") && result.ssid.as_str() != ssid {
                peers += 1;
                count += 1;
                println!(
                    "WIFI_RX peer={:02x?} count={} channel={} rssi_dbm={}",
                    result.bssid, count, result.channel, result.signal_strength
                );
            }
        }
        println!("WIFI_SCAN peers={}", peers);
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    println!("FAIL {}", info);
    loop {
        core::hint::spin_loop();
    }
}
