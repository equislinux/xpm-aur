//! yay/pacman-style argv compatibility.
//!
//! The native CLI is the source of truth, but users should be able to type the
//! common yay forms: `-S`, `-Syu`, `-Ss`, `-Si`, `-Rns`, `-Qm`, `-Sc`.
//! Unsupported operations are returned unchanged so clap can report a precise
//! error instead of silently changing their meaning.

use std::ffi::OsString;

const OPERATIONS: &[char] = &['S', 'Q', 'R', 'U', 'D', 'T', 'F', 'G', 'V'];

pub fn rewrite_args(args: Vec<OsString>) -> Vec<OsString> {
    if args.len() < 2 {
        return args;
    }

    let Some(index) = operation_index(&args) else {
        return args;
    };
    let Some(token) = args[index].to_str() else {
        return args;
    };
    let Some(operation) = token
        .strip_prefix('-')
        .and_then(|cluster| cluster.chars().next())
    else {
        return args;
    };
    if !OPERATIONS.contains(&operation) {
        return args;
    }

    let modifiers: Vec<char> = token
        .trim_start_matches('-')
        .chars()
        .filter(|c| *c != operation)
        .collect();

    let Some(subcommand) = map_operation(operation, &modifiers) else {
        return args;
    };

    let mut output = vec![args[0].clone()];
    if subcommand == "--version" {
        output.push(OsString::from("--version"));
        return output;
    }
    output.push(OsString::from(subcommand));
    for flag in operation_flags(operation, &modifiers) {
        output.push(OsString::from(flag));
    }

    let mut index = 0;
    let mut after_operation = false;
    while index < args.len() {
        if index == 0 {
            index += 1;
            continue;
        }
        if index == operation_position(&args) {
            after_operation = true;
            index += 1;
            continue;
        }
        let arg = &args[index];
        let Some(text) = arg.to_str() else {
            output.push(arg.clone());
            index += 1;
            continue;
        };

        if after_operation && !text.starts_with('-') {
            output.push(arg.clone());
            index += 1;
            continue;
        }

        match text {
            "--noconfirm" | "--no-confirm" => output.push("--noconfirm".into()),
            "--needed" | "--asdeps" | "--devel" | "--aur" | "--repo" | "--rebuild" | "--print"
            | "--remove-conflicts" => output.push(arg.clone()),
            "--noprogressbar" => {}
            "--config" | "--build-dir" | "--cache-dir" | "--state" => {
                output.push(arg.clone());
                if let Some(value) = args.get(index + 1) {
                    output.push(value.clone());
                    index += 1;
                }
            }
            _ if text.starts_with("--color=") => {}
            "--color" => {
                index += 1; // skip value
            }
            _ if text.starts_with("--") => {
                // Unknown long option: preserve it so clap can explain it.
                output.push(arg.clone());
            }
            _ if text.starts_with('-') && text.len() > 1 => {
                // Short options after the operation are not supported by this
                // compatibility layer; let clap see them unchanged.
                output.push(arg.clone());
            }
            _ => output.push(arg.clone()),
        }
        index += 1;
    }

    output
}

fn operation_position(args: &[OsString]) -> usize {
    operation_index(args).unwrap_or(args.len())
}

fn operation_index(args: &[OsString]) -> Option<usize> {
    for (index, arg) in args.iter().enumerate().skip(1) {
        let text = arg.to_str()?;
        if text == "--" {
            return None;
        }
        if text.starts_with("--") {
            continue;
        }
        if text.starts_with('-') && text.len() > 1 {
            let first = text.chars().nth(1)?;
            if OPERATIONS.contains(&first) {
                return Some(index);
            }
        }
    }
    None
}

fn map_operation(operation: char, modifiers: &[char]) -> Option<&'static str> {
    match operation {
        'S' => {
            let c = count(modifiers, 'c');
            if c > 0 {
                return Some("clean");
            }
            if modifiers.contains(&'y') && modifiers.contains(&'u') {
                return Some("upgrade");
            }
            if modifiers.contains(&'y') {
                return Some("sync");
            }
            if modifiers.contains(&'u') {
                return Some("upgrade");
            }
            if modifiers.contains(&'s') {
                return Some("search");
            }
            if modifiers.contains(&'i') {
                return Some("info");
            }
            if modifiers.contains(&'p') {
                return Some("install");
            }
            Some("install")
        }
        'Q' => Some("query"),
        'R' => Some("remove"),
        'U' => Some("install"),
        'G' => Some("getpkgbuild"),
        'V' if modifiers.is_empty() => Some("--version"),
        _ => None,
    }
}

fn count(modifiers: &[char], needle: char) -> usize {
    modifiers.iter().filter(|c| **c == needle).count()
}

fn operation_flags(operation: char, modifiers: &[char]) -> Vec<&'static str> {
    let mut flags = Vec::new();
    match operation {
        'S' => {
            if count(modifiers, 'c') > 1 {
                flags.push("--all");
            }
            if count(modifiers, 'p') > 0 {
                flags.push("--print");
            }
        }
        'R' => {
            if count(modifiers, 'n') > 0 {
                flags.push("--nosave");
            }
            if count(modifiers, 's') > 0 {
                flags.push("--recursive");
            }
        }
        'Q' => {
            if count(modifiers, 'm') > 0 {
                flags.push("--foreign");
            }
            if count(modifiers, 'q') > 0 {
                flags.push("--quiet");
            }
        }
        'G' if count(modifiers, 'p') > 0 => flags.push("--print"),
        _ => {}
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewrite(input: &[&str]) -> Vec<String> {
        rewrite_args(input.iter().map(OsString::from).collect())
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn upgrades_with_syu() {
        assert_eq!(
            rewrite(&["yay", "-Syu", "--noconfirm"]),
            vec!["yay", "upgrade", "--noconfirm"]
        );
    }

    #[test]
    fn installs_with_s() {
        assert_eq!(
            rewrite(&["yay", "-S", "foo", "--needed"]),
            vec!["yay", "install", "foo", "--needed"]
        );
    }

    #[test]
    fn searches_and_infos() {
        assert_eq!(
            rewrite(&["yay", "-Ss", "foo"]),
            vec!["yay", "search", "foo"]
        );
        assert_eq!(rewrite(&["yay", "-Si", "foo"]), vec!["yay", "info", "foo"]);
    }

    #[test]
    fn removes_recursively_and_without_save() {
        assert_eq!(
            rewrite(&["yay", "-Rns", "foo"]),
            vec!["yay", "remove", "--nosave", "--recursive", "foo"]
        );
    }

    #[test]
    fn maps_clean_and_version() {
        assert_eq!(rewrite(&["yay", "-Sc"]), vec!["yay", "clean"]);
        assert_eq!(rewrite(&["yay", "-Scc"]), vec!["yay", "clean", "--all"]);
        assert_eq!(rewrite(&["yay", "-V"]), vec!["yay", "--version"]);
    }

    #[test]
    fn maps_print_and_quiet_flags() {
        assert_eq!(
            rewrite(&["yay", "-Sp", "foo"]),
            vec!["yay", "install", "--print", "foo"]
        );
        assert_eq!(rewrite(&["yay", "-Qq"]), vec!["yay", "query", "--quiet"]);
    }

    #[test]
    fn maps_foreign_query_and_getpkgbuild() {
        assert_eq!(rewrite(&["yay", "-Qm"]), vec!["yay", "query", "--foreign"]);
        assert_eq!(
            rewrite(&["yay", "-Qmq"]),
            vec!["yay", "query", "--foreign", "--quiet"]
        );
        assert_eq!(
            rewrite(&["yay", "-G", "foo"]),
            vec!["yay", "getpkgbuild", "foo"]
        );
        assert_eq!(
            rewrite(&["yay", "-Gp", "foo"]),
            vec!["yay", "getpkgbuild", "--print", "foo"]
        );
    }

    #[test]
    fn leaves_native_and_unknown_forms_alone() {
        assert_eq!(
            rewrite(&["yay", "install", "foo"]),
            vec!["yay", "install", "foo"]
        );
        assert_eq!(rewrite(&["yay", "-F", "foo"]), vec!["yay", "-F", "foo"]);
    }
}
