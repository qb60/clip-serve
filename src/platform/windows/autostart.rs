use std::{ffi::OsString, io, path::Path};

use winreg::{
    RegKey, RegValue,
    enums::{HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE},
    types::ToRegValue,
};

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "ClipServe";

pub struct Change {
    key: RegKey,
    previous: Option<RegValue<'static>>,
}

impl Change {
    pub fn apply(exe: &Path, enabled: bool) -> io::Result<Option<Self>> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let key = match hkcu.open_subkey_with_flags(RUN, KEY_QUERY_VALUE | KEY_SET_VALUE) {
            Ok(key) => key,
            Err(err) if err.kind() == io::ErrorKind::NotFound && enabled => {
                hkcu.create_subkey(RUN)?.0
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let previous = match key.get_raw_value(VALUE) {
            Ok(value) => Some(value),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(err),
        };
        if enabled {
            let mut command = OsString::from("\"");
            command.push(exe);
            command.push("\"");
            let desired = command.to_reg_value();
            if previous.as_ref() == Some(&desired) {
                return Ok(None);
            }
            key.set_raw_value(VALUE, &desired)?;
        } else if previous.is_some() {
            delete(&key)?;
        } else {
            return Ok(None);
        }
        Ok(Some(Self { key, previous }))
    }

    pub fn rollback(self) -> io::Result<()> {
        match self.previous {
            Some(value) => self.key.set_raw_value(VALUE, &value),
            None => delete(&self.key),
        }
    }
}

fn delete(key: &RegKey) -> io::Result<()> {
    match key.delete_value(VALUE) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}
