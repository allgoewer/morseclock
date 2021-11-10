use std::io::{self, Seek, Write};
use std::{fs, num, path, str};

pub type Result<T> = ::std::result::Result<T, Error>;

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
            assert_eq!(
                parse_trigger("some [processor-14x] banana"),
                Some("processor-14x")
            );
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid duty cycle")]
    InvalidDutyCycle,
    #[error("Float parsing error: {0}")]
    ParseFloat(#[from] num::ParseFloatError),
    #[error("Int parsing error: {0}")]
    ParseInt(#[from] num::ParseIntError),
    #[error("Led error: {0}")]
    Led(#[from] io::Error),
}

#[derive(Debug)]
pub struct SysfsLed {
    max_brightness: u32,
    old_brightness: u32,
    trigger: Option<String>,
    brightness_file: fs::File,
    trigger_file: fs::File,
}

impl SysfsLed {
    pub fn new<P: AsRef<path::Path>>(path: P) -> Result<Self> {
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

    pub fn set(&mut self, value: u32) -> Result<()> {
        self.brightness_file.seek(io::SeekFrom::Start(0))?;
        self.brightness_file.write_fmt(format_args!("{}", value))?;

        Ok(())
    }

    pub fn on(&mut self) -> Result<()> {
        self.set(self.max_brightness)
    }

    pub fn off(&mut self) -> Result<()> {
        self.set(0)
    }

    fn write_trigger(file: &mut fs::File, trigger: &str) -> Result<()> {
        file.seek(io::SeekFrom::Start(0))?;
        file.write_all(trigger.as_bytes())?;

        Ok(())
    }

    fn reset_trigger(&mut self) -> Result<()> {
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
pub struct DutyCycle(pub f64);

impl str::FromStr for DutyCycle {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        let value = s.parse()?;

        if value <= 0.0 || value > 1.0 {
            Err(Error::InvalidDutyCycle)
        } else {
            Ok(DutyCycle(value))
        }
    }
}
