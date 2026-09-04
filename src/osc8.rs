use std::ffi::OsString;

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;

struct Link {
    offset: usize,
    path: OsString,
}

pub fn annotate(input: &[u8]) -> Result<(Vec<u8>, Vec<OsString>), String> {
    let links = links(input)?;
    let width = links.len().max(1).ilog10() as usize + 1;
    let mut output = Vec::with_capacity(input.len() + links.len() * (width + 3));
    let mut copied = 0;
    let mut paths = Vec::with_capacity(links.len());
    for (index, link) in links.into_iter().enumerate() {
        output.extend_from_slice(&input[copied..link.offset]);
        output.extend_from_slice(format!("[{:>width$}] ", index + 1).as_bytes());
        copied = link.offset;
        paths.push(link.path);
    }
    output.extend_from_slice(&input[copied..]);
    Ok((output, paths))
}

fn links(input: &[u8]) -> Result<Vec<Link>, String> {
    let mut result = Vec::new();
    let mut offset = 0;
    while offset + 4 <= input.len() {
        if &input[offset..offset + 4] != b"\x1b]8;" {
            offset += 1;
            continue;
        }
        let params_end = input[offset + 4..]
            .iter()
            .position(|byte| *byte == b';')
            .map(|relative| offset + 4 + relative)
            .ok_or_else(|| "unterminated OSC 8 parameters".to_owned())?;
        let uri_start = params_end + 1;
        let (uri_end, terminator_length) =
            terminator(input, uri_start).ok_or_else(|| "unterminated OSC 8 URI".to_owned())?;
        let uri = &input[uri_start..uri_end];
        if !uri.is_empty()
            && let Some(path) = file_uri(uri)?
        {
            result.push(Link { offset, path });
        }
        offset = uri_end + terminator_length;
    }
    Ok(result)
}

fn terminator(input: &[u8], start: usize) -> Option<(usize, usize)> {
    let mut offset = start;
    while offset < input.len() {
        if input[offset] == 0x07 {
            return Some((offset, 1));
        }
        if input[offset] == 0x1b && input.get(offset + 1) == Some(&b'\\') {
            return Some((offset, 2));
        }
        offset += 1;
    }
    None
}

fn file_uri(uri: &[u8]) -> Result<Option<OsString>, String> {
    let Some(mut location) = uri.strip_prefix(b"file://") else {
        return Ok(None);
    };
    if !location.starts_with(b"/") {
        let Some(path_start) = location.iter().position(|byte| *byte == b'/') else {
            return Err("file hyperlink has no absolute path".to_owned());
        };
        location = &location[path_start..];
    }
    let decoded = percent_decode(location)?;
    Ok(Some(os_string(decoded)))
}

fn percent_decode(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut output = Vec::with_capacity(input.len());
    let mut offset = 0;
    while offset < input.len() {
        if input[offset] != b'%' {
            output.push(input[offset]);
            offset += 1;
            continue;
        }
        let Some(hex) = input.get(offset + 1..offset + 3) else {
            return Err("file hyperlink has a truncated percent escape".to_owned());
        };
        let high = hex_digit(hex[0])?;
        let low = hex_digit(hex[1])?;
        output.push(high << 4 | low);
        offset += 3;
    }
    Ok(output)
}

fn hex_digit(byte: u8) -> Result<u8, String> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err("file hyperlink has an invalid percent escape".to_owned()),
    }
}

#[cfg(unix)]
fn os_string(value: Vec<u8>) -> OsString {
    OsString::from_vec(value)
}

#[cfg(not(unix))]
fn os_string(value: Vec<u8>) -> OsString {
    String::from_utf8_lossy(&value).into_owned().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_st_and_bel_terminated_file_links() {
        let input = b"icon \x1b]8;;file:///tmp/a%20b\x1b\\a b\x1b]8;;\x1b\\\n\x1b]8;;file:///tmp/c\x07c\x1b]8;;\x07\n";
        let (output, paths) = annotate(input).unwrap();

        assert!(String::from_utf8_lossy(&output).contains("icon [1] \u{1b}]8;;"));
        assert!(String::from_utf8_lossy(&output).contains("\n[2] \u{1b}]8;;"));
        assert_eq!(paths, ["/tmp/a b", "/tmp/c"]);
    }

    #[test]
    fn ignores_non_file_hyperlinks_and_closing_links() {
        let input = b"\x1b]8;;https://example.com\x1b\\web\x1b]8;;\x1b\\";
        let (output, paths) = annotate(input).unwrap();

        assert_eq!(output, input);
        assert!(paths.is_empty());
    }
}
