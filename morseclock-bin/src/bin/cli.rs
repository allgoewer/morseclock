use chrono::{offset::Local, Timelike};
use morseclock::{Clock, Format, MorseExt};

fn app() -> Result<(), morseclock::Error> {
    let now = Local::now();

    let hour = now.hour().try_into()?;
    let minute = now.minute().try_into()?;

    let time: String = Clock::new(hour, minute, Format::Hour12)
        .into_iter()
        .morse()
        .collect();

    println!("{}", time);

    Ok(())
}

fn main() {
    if let Err(e) = app() {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
