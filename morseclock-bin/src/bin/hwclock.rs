use chrono::{offset::Local, Timelike};
use morseclock::{Clock, Format, Symbol};
use morseclock_bin as lib;
use std::convert::Infallible;
use std::ffi::OsString;
use std::process;
use std::sync::{self, atomic};
use std::thread;
use std::time::Duration;

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
    let long_duty = args.value_from_str::<_, lib::DutyCycle>(["-l", "--long-duty"])?;
    let short_duty = args.value_from_str::<_, lib::DutyCycle>(["-s", "--short-duty"])?;

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

fn blink(
    led: &mut lib::SysfsLed,
    on_duration: Duration,
    off_duration: Duration,
) -> anyhow::Result<()> {
    led.on()?;
    thread::sleep(on_duration);
    led.off()?;
    thread::sleep(off_duration);

    Ok(())
}

fn approximate_pause_repeats(target_duration: u64) -> (u64, u64) {
    if target_duration <= 200 {
        (target_duration, 1)
    } else {
        let approx_repeats = target_duration / 200;
        let approx_break_duration = target_duration / approx_repeats;

        (
            approx_break_duration,
            target_duration / approx_break_duration,
        )
    }
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

    let mut led = lib::SysfsLed::new(&args.path)?;

    // drop to an unprivileged user
    if let Some(user) = args.user {
        privdrop::PrivDrop::default().user(user).apply()?;
    }

    // break up the break duration into smaller chunks of ~ 200 ms to be able to exit ASAP
    let (break_duration, break_repeats) = approximate_pause_repeats(args.break_duration);

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
                    blink(
                        &mut led,
                        Duration::from_millis(args.short_on_duration),
                        Duration::from_millis(args.short_off_duration),
                    )?;
                }
                Symbol::Long => {
                    blink(
                        &mut led,
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
