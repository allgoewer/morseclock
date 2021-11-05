use chrono::{offset::Local, Timelike};
use morseclock::{Clock, Format, Symbol};
use std::env;
use std::fs;
use std::io::{self, Seek, Write};
use std::path;
use std::thread;
use std::time::Duration;
use std::sync::{self, atomic};

pub mod parser {
    use nom::bytes::complete::tag;
    use nom::error::{ErrorKind, ParseError};
    use nom::sequence::delimited;
    use nom::{AsChar, Finish, IResult, InputTakeAtPosition};

    fn trigger_char1<T, E: ParseError<T>>(input: T) -> IResult<T, T, E>
    where
        T: InputTakeAtPosition,
        <T as InputTakeAtPosition>::Item: nom::AsChar,
    {
        input.split_at_position1(
            |item| !matches!(item.as_char(), '0'..='9' | 'a'..='z' | 'A'..='Z' | '-'),
            ErrorKind::AlphaNumeric,
        )
    }

    pub fn parse_trigger(input: &str) -> Option<&str> {
        let pos = input.find("[")?;
        let input = &input[pos..];

        let trigger: Result<_, ()> = delimited(tag("["), trigger_char1, tag("]"))(input)
            .finish()
            .map(|(_, trigger)| match trigger {
                "none" => None,
                trigger => Some(trigger),
            });

        trigger.unwrap_or(None)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn trigger() {
            assert_eq!(parse_trigger("[none]"), None);
            assert_eq!(parse_trigger("[usb-gadget]"), Some("usb-gadget"));
            assert_eq!(parse_trigger("[cpu3]"), Some("cpu3"));
        }

        #[test]
        fn find_trigger() {
            assert_eq!(parse_trigger("some other [none]"), None);
        }
    }
}

#[derive(Debug)]
struct SysfsLed {
    max_brightness: u32,
    trigger: Option<String>,
    brightness_file: fs::File,
    trigger_file: fs::File,
}

impl SysfsLed {
    pub fn new<P: AsRef<path::Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();

        // Generate all the necessary paths
        let brightness_path: path::PathBuf =
            [path, path::Path::new("brightness")].into_iter().collect();
        let trigger_path: path::PathBuf = [path, path::Path::new("trigger")].into_iter().collect();
        let max_brightness_path: path::PathBuf = [path, path::Path::new("max_brightness")]
            .into_iter()
            .collect();

        let trigger = fs::read_to_string(&trigger_path)?;
        let brightness = fs::read_to_string(&max_brightness_path)?;

        Ok(SysfsLed {
            max_brightness: brightness.trim().parse()?,
            trigger: parser::parse_trigger(&trigger).map(|t| t.to_owned()),
            brightness_file: fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(brightness_path)?,
            trigger_file: fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(trigger_path)?,
        })
    }

    pub fn set(&mut self, value: u32) -> anyhow::Result<()> {
        self.brightness_file.seek(io::SeekFrom::Start(0))?;
        self.brightness_file.write_fmt(format_args!("{}", value))?;

        Ok(())
    }

    pub fn blink(&mut self, on_duration: Duration, off_duration: Duration) -> anyhow::Result<()> {
        self.set(self.max_brightness)?;
        thread::sleep(on_duration);
        self.set(0)?;
        thread::sleep(off_duration);

        Ok(())
    }

    fn reset_trigger(&mut self) -> anyhow::Result<()> {
        if let Some(trigger) = &self.trigger {
            self.trigger_file.seek(io::SeekFrom::Start(0))?;
            self.trigger_file.write_all(trigger.as_bytes())?;
        }

        Ok(())
    }
}

impl Drop for SysfsLed {
    fn drop(&mut self) {
        self.set(0).unwrap();
        self.reset_trigger().unwrap();
    }
}

fn app() -> anyhow::Result<()> {
    let path = "/sys/class/leds/thingm0:blue:led0/";
    let mut led = SysfsLed::new(path)?;

    let running = sync::Arc::new(atomic::AtomicBool::new(true));

    ctrlc::set_handler({
        let running = running.clone();
        move || {
            eprintln!("Exiting..");
            running.store(false, atomic::Ordering::Relaxed);
        }
    })?;

    'outer: while running.load(atomic::Ordering::Relaxed) {
        let now = Local::now();
        let hour = now.hour().try_into()?;
        let minute = now.minute().try_into()?;

        let clock = Clock::new(hour, minute, Format::Hour12);

        for sym in clock {
            match sym {
                Symbol::Break => {
                    thread::sleep(Duration::from_millis(1000));
                }
                Symbol::Short => {
                    led.blink(Duration::from_millis(100), Duration::from_millis(900))?;
                }
                Symbol::Long => {
                    led.blink(Duration::from_millis(500), Duration::from_millis(500))?;
                }
            }
        }

        for _ in 0..10 {
            if !running.load(atomic::Ordering::Relaxed) {
                break 'outer;
            }

            thread::sleep(Duration::from_millis(500));
        }
    }

    Ok(())
}

fn main() {
    if let Err(e) = app() {
        eprintln!("{}", e);
        std::process::exit(1);
    }
}
