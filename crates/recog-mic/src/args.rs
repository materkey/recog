//! The worker's command line, the same as `recog/mic_worker.py` takes.

/// What the parent asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// Rate of the frames on stdout, in Hz.
    pub sample_rate: u32,
    /// Input device name; the default input without it.
    pub device: Option<String>,
}

/// Usage text for `--help` and errors.
pub const USAGE: &str = "usage: recog-mic --sample-rate N [--device NAME]";

/// Outcome of parsing: arguments, or a request for help.
#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    /// Run with these.
    Run(Args),
    /// `--help` was given.
    Help,
}

/// Parses the arguments after the program name; `--opt value` and `--opt=value` both work.
pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Parsed, String> {
    let mut sample_rate = None;
    let mut device = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let (name, inline) = split_inline(&arg);
        let mut value = |name: &str| inline.clone().or_else(|| args.next()).ok_or(format!("{name} needs a value"));
        match name.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--sample-rate" => sample_rate = Some(positive_rate(&value("--sample-rate")?)?),
            "--device" => device = Some(value("--device")?),
            _ => return Err(format!("unrecognized argument: {arg}")),
        }
    }
    let sample_rate = sample_rate.ok_or("the following arguments are required: --sample-rate")?;
    Ok(Parsed::Run(Args { sample_rate, device }))
}

/// `--opt=value` as the option and its value; any other argument as it is.
fn split_inline(arg: &str) -> (String, Option<String>) {
    match arg.split_once('=') {
        Some((name, value)) if name.starts_with("--") => (name.to_owned(), Some(value.to_owned())),
        _ => (arg.to_owned(), None),
    }
}

/// The value of `--sample-rate`, which must be a positive integer.
fn positive_rate(raw: &str) -> Result<u32, String> {
    let rate = raw.parse::<u32>().ok().filter(|rate| *rate > 0);
    rate.ok_or(format!("--sample-rate: not a positive integer: {raw:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> Result<Parsed, String> {
        parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn the_parents_argv_with_a_device_gives_its_rate_and_name() {
        // given / when
        let parsed = run(&["--sample-rate", "16000", "--device", "Микрофон MacBook Pro"]);
        // then
        let expected = Args { sample_rate: 16_000, device: Some("Микрофон MacBook Pro".into()) };
        assert_eq!(parsed, Ok(Parsed::Run(expected)));
    }

    #[test]
    fn no_device_means_the_default_input() {
        // given / when / then
        assert_eq!(run(&["--sample-rate", "16000"]), Ok(Parsed::Run(Args { sample_rate: 16_000, device: None })));
    }

    #[test]
    fn values_after_an_equals_sign_are_accepted_like_argparse_does() {
        // given / when / then
        let expected = Args { sample_rate: 8_000, device: Some("A=B".into()) };
        assert_eq!(run(&["--sample-rate=8000", "--device=A=B"]), Ok(Parsed::Run(expected)));
    }

    #[test]
    fn a_missing_rate_a_bad_rate_and_an_unknown_flag_are_refused() {
        // given / when / then
        assert!(run(&["--device", "X"]).unwrap_err().contains("--sample-rate"));
        assert!(run(&["--sample-rate", "0"]).is_err());
        assert!(run(&["--sample-rate", "fast"]).is_err());
        assert!(run(&["--sample-rate"]).is_err());
        assert!(run(&["--sample-rate", "16000", "--loud"]).unwrap_err().contains("--loud"));
    }

    #[test]
    fn a_refused_call_says_why_in_argparses_words() {
        // given / when / then
        assert_eq!(run(&["--sample-rate=fast"]), Err("--sample-rate: not a positive integer: \"fast\"".into()));
        assert_eq!(run(&["--sample-rate", "16000", "--device"]), Err("--device needs a value".into()));
        assert_eq!(run(&["-x=1"]), Err("unrecognized argument: -x=1".into()));
        assert_eq!(run(&[]), Err("the following arguments are required: --sample-rate".into()));
    }

    #[test]
    fn help_is_recognised() {
        // given / when / then
        assert_eq!(run(&["--help"]), Ok(Parsed::Help));
    }
}
