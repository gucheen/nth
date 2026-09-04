use std::ffi::{OsStr, OsString};

pub fn expand(args: &[OsString], values: &[OsString]) -> Result<Vec<OsString>, String> {
    let mut expanded = Vec::new();
    for arg in args {
        if let Some(literal) = strip_literal_prefix(arg) {
            expanded.push(literal);
            continue;
        }

        let Some(selector) = arg.to_str().and_then(parse) else {
            expanded.push(arg.clone());
            continue;
        };
        for index in selector? {
            let Some(value) = values.get(index - 1) else {
                return Err(format!(
                    "shortcut {index} is out of range (current list has {} items)",
                    values.len()
                ));
            };
            expanded.push(value.clone());
        }
    }
    Ok(expanded)
}

pub fn contains_selector(args: &[OsString]) -> bool {
    args.iter()
        .any(|arg| strip_literal_prefix(arg).is_none() && arg.to_str().and_then(parse).is_some())
}

fn parse(value: &str) -> Option<Result<Vec<usize>, String>> {
    let selector = value.strip_prefix('@').unwrap_or(value);
    if !selector.bytes().any(|byte| byte.is_ascii_digit())
        || !selector
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-' || byte == b',')
    {
        return None;
    }
    Some(parse_selector(selector).map_err(|()| format!("invalid shortcut selector {value:?}")))
}

fn parse_selector(value: &str) -> Result<Vec<usize>, ()> {
    let mut result = Vec::new();
    for part in value.split(',') {
        let mut bounds = part.split('-');
        let start = positive(bounds.next().ok_or(())?)?;
        match bounds.next() {
            None => result.push(start),
            Some(end) => {
                if bounds.next().is_some() {
                    return Err(());
                }
                let end = positive(end)?;
                if start <= end {
                    result.extend(start..=end);
                } else {
                    result.extend((end..=start).rev());
                }
            }
        }
    }
    Ok(result)
}

fn positive(value: &str) -> Result<usize, ()> {
    value.parse::<usize>().map_err(|_| ()).and_then(
        |number| {
            if number == 0 { Err(()) } else { Ok(number) }
        },
    )
}

#[cfg(unix)]
fn strip_literal_prefix(value: &OsStr) -> Option<OsString> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    value
        .as_bytes()
        .strip_prefix(b"=")
        .map(|bytes| OsString::from_vec(bytes.to_vec()))
}

#[cfg(not(unix))]
fn strip_literal_prefix(value: &OsStr) -> Option<OsString> {
    value.to_str()?.strip_prefix('=').map(OsString::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_single_ranges_lists_and_explicit_selectors() {
        let values = strings(&["alpha", "two words", "*.rs", "four"]);
        let actual = expand(&strings(&["1,3-4", "@2", "dest"]), &values).unwrap();

        assert_eq!(
            actual,
            strings(&["alpha", "*.rs", "four", "two words", "dest"])
        );
    }

    #[test]
    fn expands_descending_ranges_and_literal_arguments() {
        let actual = expand(&strings(&["3-1", "=755", "=1"]), &strings(&["a", "b", "c"])).unwrap();

        assert_eq!(actual, strings(&["c", "b", "a", "755", "1"]));
    }

    #[test]
    fn rejects_invalid_and_stale_selectors() {
        for input in ["0", "1-", "1,,2", "4"] {
            assert!(expand(&strings(&[input]), &strings(&["a", "b", "c"])).is_err());
        }
    }

    #[test]
    fn leaves_ordinary_arguments_untouched() {
        let input = strings(&["-n", "12px", "a-b", "--", "./1"]);

        assert_eq!(expand(&input, &strings(&["a"])).unwrap(), input);
    }

    #[test]
    fn detects_only_arguments_that_need_shortcut_state() {
        assert!(contains_selector(&strings(&["1", "file"])));
        assert!(contains_selector(&strings(&["1-"])));
        assert!(!contains_selector(&strings(&["=755", "12px", "./1"])));
    }

    fn strings(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }
}
