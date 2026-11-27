// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: Apache-2.0

//! The argparse port against CPython 3.15's argparse: every expected text and
//! value below is what `python3.15` printed or parsed for the same parser and
//! arguments, with COLUMNS=60, LINES=24 and PYTHON_COLORS=0.
use tax_radar_au::argparse::{Action, Exit, Get, Namespace, Parser, Value};

/// COLUMNS, LINES and PYTHON_COLORS for every test, set once before any test
/// reads them.
fn init() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: every test calls `init` before it formats or parses, and the
    // Once makes those reads wait until this single write is done.
    ONCE.call_once(|| unsafe {
        std::env::set_var("COLUMNS", "60");
        std::env::set_var("LINES", "24");
        std::env::set_var("PYTHON_COLORS", "0");
    });
}

/// `parse_args`, with the exit status and text argparse would print instead.
fn outcome(parser: &Parser, args: &[&str]) -> Result<Namespace, (i32, String)> {
    init();
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    parser.parse(&args).map_err(|exit| match exit {
        Exit::Help(text) => (0, text),
        Exit::Error(text) => (2, text),
    })
}

fn parsing() -> Parser {
    Parser::new("p")
        .arg(Action::flag("-v"))
        .arg(Action::flag("-q"))
        .arg(Action::option("-n").int())
        .arg(Action::option("--name"))
        .arg(Action::flag("--dry-run"))
        .arg(Action::flag("--alpha"))
        .arg(Action::flag("--alps"))
        .arg(Action::option("-foo"))
        .arg(Action::option("--items").one_or_more())
        .arg(Action::option("--mode").choices(&["alpha", "beta"]))
        .arg(Action::option("--pair").choices(&["abab", "xalphax"]))
        .arg(Action::positional("value"))
}

const P_USAGE: &str = "usage: p [-h] [-v] [-q] [-n N] [--name NAME] [--dry-run]\n         \
                       [--alpha] [--alps] [-foo FOO]\n         \
                       [--items ITEMS [ITEMS ...]] [--mode {alpha,beta}]\n         \
                       [--pair {abab,xalphax}]\n         value\n";

fn refused(args: &[&str], message: &str) {
    assert_eq!(
        outcome(&parsing(), args).unwrap_err(),
        (2, format!("{P_USAGE}p: error: {message}\n")),
        "{args:?}"
    );
}

#[test]
fn exits_report_their_status() {
    assert_eq!(Exit::Help(String::new()).report(), 0);
    assert_eq!(Exit::Error(String::new()).report(), 2);
}

#[test]
fn a_long_header_takes_its_own_line_and_blank_help_none() {
    let parser = Parser::new("prog")
        .arg(
            Action::option("--a-very-long-option-name")
                .metavar("VALUE")
                .help("Takes a value."),
        )
        .arg(Action::flag("--blank").help("   "));
    assert_eq!(
        outcome(&parser, &["-h"]).unwrap_err(),
        (
            0,
            "usage: prog [-h] [--a-very-long-option-name VALUE]\n            [--blank]\n\n\
             options:\n  -h, --help            show this help message and exit\n  \
             --a-very-long-option-name VALUE\n                        Takes a value.\n  \
             --blank               \n"
                .to_string()
        )
    );
}

#[test]
fn a_long_program_name_starts_the_usage_on_its_own_line() {
    let parser = Parser::new("a-program-name-long-enough-to-wrap-its-usage")
        .arg(Action::option("--alpha"))
        .arg(Action::option("--beta"))
        .arg(Action::option("--gamma-option"))
        .arg(Action::positional("target"))
        .arg(Action::positional("rest").one_or_more());
    let usage = "usage: a-program-name-long-enough-to-wrap-its-usage\n       \
                 [-h] [--alpha ALPHA] [--beta BETA]\n       \
                 [--gamma-option GAMMA_OPTION]\n       target rest [rest ...]\n";
    assert_eq!(
        outcome(&parser, &[]).unwrap_err(),
        (
            2,
            format!(
                "{usage}a-program-name-long-enough-to-wrap-its-usage: error: \
                 the following arguments are required: target, rest\n"
            )
        )
    );
    assert_eq!(
        outcome(&parser, &["-h"]).unwrap_err(),
        (
            0,
            format!(
                "{usage}\npositional arguments:\n  target\n  rest\n\noptions:\n  \
                 -h, --help            show this help message and exit\n  --alpha ALPHA\n  \
                 --beta BETA\n  --gamma-option GAMMA_OPTION\n"
            )
        )
    );
}

#[test]
fn combined_short_options_are_split_as_python_splits_them() {
    for args in [
        &["-vqn5", "x"][..],
        &["-vn=5", "x"][..],
        &["-vn", "5", "x"][..],
    ] {
        let ns = outcome(&parsing(), args).unwrap();
        assert_eq!(
            (ns.flag("v"), ns.flag("q"), ns.get("n"), ns.str("value")),
            (
                true,
                args[0] == "-vqn5",
                Some(&Value::Int(5)),
                Some("x".into())
            ),
            "{args:?}"
        );
    }
    refused(&["-vz", "x"], "unrecognized arguments: -z");
    refused(&["-v=1", "x"], "argument -v: ignored explicit argument '1'");
    refused(
        &["--dry-run=1", "x"],
        "argument --dry-run: ignored explicit argument '1'",
    );
}

#[test]
fn prefixes_select_an_option_unless_ambiguous() {
    let ns = outcome(&parsing(), &["--na=a", "x"]).unwrap();
    assert_eq!(ns.str("name"), Some("a".into()));
    let ns = outcome(&parsing(), &["-fo", "bar", "x"]).unwrap();
    assert_eq!(ns.str("foo"), Some("bar".into()));
    refused(
        &["--al", "x"],
        "ambiguous option: --al could match --alpha, --alps",
    );
}

#[test]
fn option_arguments_are_counted() {
    let ns = outcome(&parsing(), &["x", "--items", "a", "b"]).unwrap();
    assert_eq!(ns.list("items"), ["a", "b"]);
    refused(
        &["x", "--items"],
        "argument --items: expected at least one argument",
    );
    refused(&["x", "--name"], "argument --name: expected one argument");
    refused(&["x", "y", "-v"], "unrecognized arguments: y");
}

#[test]
fn dash_led_values_that_are_not_options_are_positional() {
    for (args, value) in [
        (&["-1.5"][..], "-1.5"),
        (&["-a b"][..], "-a b"),
        (&["--", "-v"][..], "-v"),
    ] {
        let ns = outcome(&parsing(), args).unwrap();
        assert_eq!((ns.str("value"), ns.flag("v")), (Some(value.into()), false));
    }
}

#[test]
fn invalid_choices_suggest_a_close_match() {
    let choices = "(choose from 'abab', 'xalphax')";
    refused(
        &["x", "--mode", "alpah"],
        "argument --mode: invalid choice: 'alpah', maybe you meant 'alpha'? \
         (choose from 'alpha', 'beta')",
    );
    for word in ["baba", "abba"] {
        refused(
            &["x", "--pair", word],
            &format!(
                "argument --pair: invalid choice: '{word}', maybe you meant 'abab'? {choices}"
            ),
        );
    }
    // A word of 200 or more characters drops its popular characters from the
    // match index, as difflib's autojunk does.
    let long = format!("{}alphax", "x".repeat(200));
    refused(
        &["x", "--pair", &long],
        &format!("argument --pair: invalid choice: '{long}' {choices}"),
    );
}

#[test]
fn a_sub_command_after_dashes_is_still_parsed() {
    let parser = Parser::new("c")
        .subcommands("cmd")
        .sub("run", Some("Run it."), |p| p.arg(Action::flag("--fast")));
    let ns = outcome(&parser, &["--", "run", "--fast"]).unwrap();
    assert_eq!((ns.str("cmd"), ns.flag("fast")), (Some("run".into()), true));
    assert_eq!(
        outcome(&parser, &["-h"]).unwrap_err(),
        (
            0,
            "usage: c [-h] {run} ...\n\npositional arguments:\n  {run}\n    run       Run it.\n\n\
             options:\n  -h, --help  show this help message and exit\n"
                .to_string()
        )
    );
}
