//! argparse's reading of the binaries' options, and the checkout they run for.
#![forbid(unsafe_code)]

use std::path::PathBuf;

use recog_copilot::cli::{parse_options, repo_root};

fn parsed(args: &[&str]) -> (Result<bool, String>, Vec<(String, String)>) {
    let mut seen = Vec::new();
    let result = parse_options(args.iter().map(|a| (*a).to_owned()), &["--a", "--b"], |flag, value| {
        if value == "refused" {
            return Err(format!("{flag} refused"));
        }
        seen.push((flag.to_owned(), value));
        Ok(())
    });
    (result, seen)
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

#[test]
fn options_when_read_then_each_value_applied_in_order_inline_or_next() {
    // given / when
    let (result, seen) = parsed(&["--a", "1", "--b=x=y", "--a=", "--b", "-h"]);
    // then: `-h` after `--b` is its value, not the help flag
    assert_eq!(result, Ok(true));
    assert_eq!(seen, pairs(&[("--a", "1"), ("--b", "x=y"), ("--a", ""), ("--b", "-h")]));
}

#[test]
fn help_when_anywhere_then_false_and_later_arguments_ignored() {
    // given / when / then
    assert_eq!(parsed(&["--a", "1", "--help", "--bogus"]), (Ok(false), pairs(&[("--a", "1")])));
    assert_eq!(parsed(&["--help=1"]).0, Ok(false));
    assert_eq!(parsed(&[]).0, Ok(true));
}

#[test]
fn bad_arguments_when_read_then_argparse_messages_at_the_first_problem() {
    // given / when / then
    assert_eq!(parsed(&["--a", "1", "--c", "2"]).0, Err("unrecognized arguments: --c".to_owned()));
    assert_eq!(parsed(&["-h=1"]).0, Err("unrecognized arguments: -h=1".to_owned()));
    assert_eq!(parsed(&["--b"]).0, Err("argument --b: expected one argument".to_owned()));
    assert_eq!(parsed(&["--a", "refused", "--c"]).0, Err("--a refused".to_owned()));
}

#[test]
fn named_checkout_when_given_then_used_as_is() {
    // given / when / then
    assert_eq!(repo_root(Some(PathBuf::from("/some/checkout"))), Ok(PathBuf::from("/some/checkout")));
}
