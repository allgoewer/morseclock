use chrono::{offset::Local, Timelike};
use morseclock::{Clock, Format, Symbol};
use std::convert::Infallible;
use std::fs;
use std::ffi;
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
    old_brightness: u32,
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
        let max_brightness = fs::read_to_string(&max_brightness_path)?;
        let old_brightness = fs::read_to_string(&brightness_path)?;

        Ok(SysfsLed {
            max_brightness: max_brightness.trim().parse()?,
            old_brightness: old_brightness.trim().parse()?,
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
        self.set(self.old_brightness).unwrap();
        self.reset_trigger().unwrap();
    }
}

#[derive(Debug)]
pub struct Args {
    pub base_duration: u64,
    pub break_duration: u64,
    pub short_on_duration: u64,
    pub short_off_duration: u64,
    pub long_on_duration:  u64,
    pub long_off_duration: u64,
    pub path: ffi::OsString,
}

fn args() -> anyhow::Result<Args> {
    let mut args = pico_args::Arguments::from_env();

    let break_duration: u64 = args.value_from_str(["-p", "--pause-duration"])?;
    let base_duration: u64 = args.value_from_str(["-b", "--base-duration"])?;
    let long_duty = args.value_from_str::<_, f64>(["-l", "--long-duty"])?.clamp(0.001, 1.0);
    let short_duty = args.value_from_str::<_, f64>(["-s", "--short-duty"])?.clamp(0.001, 1.0);

    Ok(Args {
        base_duration,
        break_duration,
        short_on_duration: (base_duration as f64 * short_duty) as u64,
        short_off_duration: (base_duration as f64 * (1.0 - short_duty)) as u64,
        long_on_duration: (base_duration as f64 * long_duty) as u64,
        long_off_duration: (base_duration as f64 * (1.0 - long_duty)) as u64,
        path: args.free_from_os_str::<_, Infallible>(|f| Ok(f.to_owned()))?,
    })
}

fn app() -> anyhow::Result<()> {
    let args = dbg!(args()?);

    let mut led = SysfsLed::new(&args.path)?;

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
            if !running.load(atomic::Ordering::Relaxed) {
                break 'outer;
            }

            match sym {
                Symbol::Break => {
                    thread::sleep(Duration::from_millis(args.base_duration));
                }
                Symbol::Short => {
                    led.blink(Duration::from_millis(args.short_on_duration), Duration::from_millis(args.short_off_duration))?;
                }
                Symbol::Long => {
                    led.blink(Duration::from_millis(args.long_on_duration), Duration::from_millis(args.long_off_duration))?;
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
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}
