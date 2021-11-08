use chrono::{offset::Local, Timelike};
use morseclock::{Clock, Format, Symbol};
use std::convert::Infallible;
use std::error;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Seek, Write};
use std::num;
use std::path;
use std::process;
use std::str;
use std::sync::{self, atomic};
use std::thread;
use std::time::Duration;

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
        let pos = input.find('[')?;
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
            assert_eq!(parse_trigger("some other"), None);
            assert_eq!(parse_trigger("some other [none]"), None);
            assert_eq!(parse_trigger("some [processor-14x] banana"), Some("processor-14x"));
        }
    }
}

#[derive(Debug)]
enum Error {
    InvalidDutyCycle,
    ParseError(num::ParseFloatError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDutyCycle => f.write_str("Invalid duty cycle"),
            Self::ParseError(e) => write!(f, "Parsing failed: {}", e),
        }
    }
}

impl error::Error for Error {}

impl From<num::ParseFloatError> for Error {
    fn from(error: num::ParseFloatError) -> Self {
        Self::ParseError(error)
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

        let mut trigger_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(trigger_path)?;

        Self::write_trigger(&mut trigger_file, "none")?;

        Ok(SysfsLed {
            max_brightness: max_brightness.trim().parse()?,
            old_brightness: old_brightness.trim().parse()?,
            trigger: parser::parse_trigger(&trigger).map(|t| t.to_owned()),
            brightness_file: fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(brightness_path)?,
            trigger_file,
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

    fn write_trigger(file: &mut fs::File, trigger: &str) -> anyhow::Result<()> {
        file.seek(io::SeekFrom::Start(0))?;
        file.write_all(trigger.as_bytes())?;

        Ok(())
    }

    fn reset_trigger(&mut self) -> anyhow::Result<()> {
        if let Some(trigger) = &self.trigger {
            Self::write_trigger(&mut self.trigger_file, trigger)
        } else {
            Ok(())
        }
    }
}

impl Drop for SysfsLed {
    fn drop(&mut self) {
        self.set(self.old_brightness).unwrap();
        self.reset_trigger().unwrap();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
struct DutyCycle(f64);

impl str::FromStr for DutyCycle {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let value = s.parse()?;

        if value <= 0.0 || value > 1.0 {
            Err(Error::InvalidDutyCycle)
        } else {
            Ok(DutyCycle(value))
        }
    }
}

#[derive(Debug)]
pub struct Args {
    pub base_duration: u64,
    pub break_duration: u64,
    pub short_on_duration: u64,
    pub short_off_duration: u64,
    pub long_on_duration: u64,
    pub long_off_duration: u64,
    pub user: Option<OsString>,
    pub path: OsString,
}

fn help() {
    println!(
        r#"
morseclock-hw - Yet another not-so-useful LED clock

Usage: morseclock-hw [PARAMS] [OPTIONS] LED_SYSFS_DIR

Parameters:
    -p, --pause-duration    Duration of pause between hour and minute
    -b, --base-duration     Base duration of a blink
    -l, --long-duty         Duty cycle of the long blink
    -s, --short-duration    Duty cycle of the short blink

Options:
    -h, --help              Print this help message
    -u, --user              User to drop privileges to

"#
    );
}

fn args() -> anyhow::Result<Args> {
    let mut args = pico_args::Arguments::from_env();

    if args.contains(["-h", "--help"]) {
        help();
        process::exit(0);
    }

    let break_duration = args.value_from_str(["-p", "--pause-duration"])?;
    let base_duration = args.value_from_str(["-b", "--base-duration"])?;
    let long_duty = args.value_from_str::<_, DutyCycle>(["-l", "--long-duty"])?;
    let short_duty = args.value_from_str::<_, DutyCycle>(["-s", "--short-duty"])?;

    Ok(Args {
        base_duration,
        break_duration,
        short_on_duration: (base_duration as f64 * short_duty.0) as u64,
        short_off_duration: (base_duration as f64 * (1.0 - short_duty.0)) as u64,
        long_on_duration: (base_duration as f64 * long_duty.0) as u64,
        long_off_duration: (base_duration as f64 * (1.0 - long_duty.0)) as u64,
        user: args
            .opt_value_from_os_str::<_, _, Infallible>(["-u", "--user"], |u| Ok(u.to_owned()))?,
        path: args.free_from_os_str::<_, Infallible>(|f| Ok(f.to_owned()))?,
    })
}

fn app() -> anyhow::Result<()> {
    let args = match args() {
        Ok(args) => args,
        Err(e) => {
            eprintln!("Argument error: {}", e);
            help();
            process::exit(1);
        }
    };

    let mut led = SysfsLed::new(&args.path)?;

    // drop to an unprivileged user
    if let Some(user) = args.user {
        privdrop::PrivDrop::default().user(user).apply()?;
    }

    // break up the break duration into smaller chunks of ~ 200 ms to be able to exit ASAP
    let (break_duration, break_repeats) = if args.break_duration <= 200 {
        (args.break_duration, 1)
    } else {
        let approx_repeats = args.break_duration / 200;
        let approx_break_duration = args.break_duration / approx_repeats;

        (
            approx_break_duration,
            args.break_duration / approx_break_duration,
        )
    };

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
                    led.blink(
                        Duration::from_millis(args.short_on_duration),
                        Duration::from_millis(args.short_off_duration),
                    )?;
                }
                Symbol::Long => {
                    led.blink(
                        Duration::from_millis(args.long_on_duration),
                        Duration::from_millis(args.long_off_duration),
                    )?;
                }
            }
        }

        for _ in 0..break_repeats {
            if !running.load(atomic::Ordering::Relaxed) {
                break 'outer;
            }

            thread::sleep(Duration::from_millis(break_duration));
        }
    }

    Ok(())
}

fn main() {
    if let Err(e) = app() {
        eprintln!("Error: {}", e);
        process::exit(1);
    }
}
