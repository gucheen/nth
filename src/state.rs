use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

const MAGIC: &[u8; 5] = b"NTHS\x01";

pub struct State {
    pub items: Vec<OsString>,
}

pub fn save<'a>(items: impl Iterator<Item = &'a OsStr>) -> Result<(), String> {
    let items = items.map(os_bytes).collect::<Vec<_>>();
    let mut data = Vec::new();
    data.extend_from_slice(MAGIC);
    write_length(&mut data, items.len())?;
    for item in items {
        write_length(&mut data, item.len())?;
        data.extend_from_slice(item);
    }

    let path = state_path();
    let directory = path.parent().expect("state path must have a parent");
    let directory_is_new = !directory.exists();
    fs::create_dir_all(directory)
        .map_err(|error| format!("cannot create state directory: {error}"))?;
    if directory_is_new {
        secure_directory(directory)?;
    }
    let temporary = temporary_path(directory);
    let write_result =
        write_temporary(&temporary, &data).and_then(|()| fs::rename(&temporary, &path));
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(format!("cannot save shortcut state: {error}"));
    }
    Ok(())
}

pub fn load() -> Result<State, String> {
    let data = fs::read(state_path()).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            "no shortcuts yet; run an observed command or 'nth capture' first".to_owned()
        } else {
            format!("cannot read shortcut state: {error}")
        }
    })?;
    decode(&data)
}

pub fn clear() -> Result<(), String> {
    match fs::remove_file(state_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot clear shortcut state: {error}")),
    }
}

fn decode(data: &[u8]) -> Result<State, String> {
    let mut reader = Cursor::new(data);
    let mut magic = [0; MAGIC.len()];
    reader.read_exact(&mut magic).map_err(corrupt)?;
    if &magic != MAGIC {
        return Err("shortcut state has an unsupported format".to_owned());
    }

    let count = read_length(&mut reader)?;
    let remaining = data.len().saturating_sub(reader.position() as usize);
    if count > remaining / size_of::<u32>() {
        return Err("shortcut state is corrupt: invalid item count".to_owned());
    }
    let mut items = Vec::with_capacity(count);
    for _ in 0..count {
        let length = read_length(&mut reader)?;
        let remaining = data.len().saturating_sub(reader.position() as usize);
        if length > remaining {
            return Err("shortcut state is corrupt: invalid item length".to_owned());
        }
        let mut value = vec![0; length];
        reader.read_exact(&mut value).map_err(corrupt)?;
        items.push(os_string(value));
    }
    if reader.position() != data.len() as u64 {
        return Err("shortcut state contains trailing data".to_owned());
    }
    Ok(State { items })
}

fn write_length(output: &mut Vec<u8>, value: usize) -> Result<(), String> {
    let value = u32::try_from(value).map_err(|_| "shortcut state is too large".to_owned())?;
    output.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn read_length(reader: &mut Cursor<&[u8]>) -> Result<usize, String> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes).map_err(corrupt)?;
    Ok(u32::from_le_bytes(bytes) as usize)
}

fn corrupt(error: io::Error) -> String {
    format!("shortcut state is corrupt: {error}")
}

fn state_path() -> PathBuf {
    let directory = if let Some(path) = env::var_os("NTH_STATE_DIR") {
        PathBuf::from(path)
    } else if let Some(path) = env::var_os("XDG_RUNTIME_DIR") {
        PathBuf::from(path).join("nth")
    } else if let Some(path) = env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(path).join("nth/runtime")
    } else if let Some(path) = env::var_os("HOME") {
        PathBuf::from(path).join(".cache/nth/runtime")
    } else {
        env::temp_dir().join("nth-runtime")
    };
    let session = env::var_os("NTH_SESSION").unwrap_or_else(|| OsString::from("default"));
    directory.join(format!("{:016x}.state", fnv1a(os_bytes(&session))))
}

fn temporary_path(directory: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    directory.join(format!(".state-{}-{nanos}", process::id()))
}

#[cfg(unix)]
fn write_temporary(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(data)
}

#[cfg(not(unix))]
fn write_temporary(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(data)
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> Result<(), String> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure state directory: {error}"))
}

#[cfg(not(unix))]
fn secure_directory(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(unix)]
fn os_bytes(value: &OsStr) -> &[u8] {
    value.as_bytes()
}

#[cfg(not(unix))]
fn os_bytes(value: &OsStr) -> &[u8] {
    value.to_str().unwrap_or_default().as_bytes()
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
    fn binary_format_round_trips_arbitrary_bytes() {
        let values = vec![OsString::from("alpha"), os_string(vec![0xff, b'\n', b'x'])];
        let mut encoded = Vec::from(MAGIC);
        write_length(&mut encoded, values.len()).unwrap();
        for value in &values {
            let bytes = os_bytes(value);
            write_length(&mut encoded, bytes.len()).unwrap();
            encoded.extend_from_slice(bytes);
        }

        assert_eq!(decode(&encoded).unwrap().items, values);
    }

    #[test]
    fn rejects_truncated_and_trailing_data() {
        assert!(decode(MAGIC).is_err());
        let mut data = Vec::from(MAGIC);
        write_length(&mut data, 0).unwrap();
        data.push(1);

        assert!(decode(&data).is_err());
    }
}
