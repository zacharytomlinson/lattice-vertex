//! Solar/date arithmetic adapted from Jarvis src/solar/mod.rs. Graph ownership,
//! allocated snapshots and host timezone/database access are deliberately absent.
use core::f64::consts::PI;
use lattice_peer_wire::clock::{Date, Event, DAY};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Crossing {
    At(i64),
    Above,
    Below,
}
fn sin(x: f64) -> f64 {
    libm::sin(x * PI / 180.0)
}
fn cos(x: f64) -> f64 {
    libm::cos(x * PI / 180.0)
}
fn norm(x: f64, n: f64) -> f64 {
    x - libm::floor(x / n) * n
}
pub(super) fn event(date: Date, latitude: i32, longitude: i32, event: Event) -> Crossing {
    let latitude = latitude as f64 / 1_000_000.0;
    let longitude = longitude as f64 / 1_000_000.0;
    let longitude_hour = longitude / 15.0;
    let hour = match event {
        Event::Sunrise => 6.0,
        Event::Noon => 12.0,
        Event::Sunset => 18.0,
    };
    let t = date.ordinal() as f64 + (hour - longitude_hour) / 24.0;
    let anomaly = 0.9856 * t - 3.289;
    let longitude_sun = norm(
        anomaly + 1.916 * sin(anomaly) + 0.020 * sin(2.0 * anomaly) + 282.634,
        360.0,
    );
    let ra = norm(
        libm::atan(0.91764 * libm::tan(longitude_sun * PI / 180.0)) * 180.0 / PI,
        360.0,
    );
    let ra = (ra + libm::floor(longitude_sun / 90.0) * 90.0 - libm::floor(ra / 90.0) * 90.0) / 15.0;
    let declination = 0.39782 * sin(longitude_sun);
    let altitude = (cos(90.833) - declination * sin(latitude))
        / (libm::sqrt(1.0 - declination * declination) * cos(latitude));
    let angle = if event == Event::Noon {
        0.0
    } else {
        if altitude > 1.0 {
            return Crossing::Below;
        }
        if altitude < -1.0 {
            return Crossing::Above;
        }
        let h = libm::acos(altitude) * 180.0 / PI;
        (if event == Event::Sunrise {
            360.0 - h
        } else {
            h
        }) / 15.0
    };
    let universal = norm(angle + ra - 0.06571 * t - 6.622 - longitude_hour, 24.0);
    let local = universal + longitude_hour;
    let day = if local < 0.0 {
        1
    } else if local >= 24.0 {
        -1
    } else {
        0
    };
    Crossing::At((date.days() + day) * DAY + libm::round(universal * 3600.0) as i64)
}
