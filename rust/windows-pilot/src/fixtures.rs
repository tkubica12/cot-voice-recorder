use crate::NativeError;
use std::io::Read;
use std::os::windows::fs::MetadataExt;
use std::path::Path;

pub const SCOPE: &str = "full-client-signed-out-fixture";
pub const AUTH_SCOPE: &str = "single-google-auth-only-fixture";

pub fn validate(root: &Path, key: Option<&str>) -> Result<(), NativeError> {
    validate_scope(root, key, SCOPE)
}

pub fn validate_auth(root: &Path, key: Option<&str>) -> Result<(), NativeError> {
    validate_scope(root, key, AUTH_SCOPE)?;
    if root
        .join("auth-fixture-result.json")
        .try_exists()
        .map_err(|error| NativeError::Io("Inspect auth-only result boundary", error))?
    {
        return Err(NativeError::InvalidArgument(
            "Auth-only acceptance cannot reuse a previous attempt.",
        ));
    }
    Ok(())
}

pub(crate) fn validate_scope(
    root: &Path,
    key: Option<&str>,
    scope: &str,
) -> Result<(), NativeError> {
    if !root.is_absolute() || !root.is_dir() {
        return Err(NativeError::InvalidArgument(
            "Fixture state must be an explicit existing absolute directory.",
        ));
    }
    for path in root.ancestors() {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| NativeError::Io("Inspect fixture path boundary", error))?;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(NativeError::InvalidArgument(
                "Measurement paths cannot cross a reparse point.",
            ));
        }
    }
    let marker_path = root.join("migration-fixture.json");
    if std::fs::symlink_metadata(&marker_path)
        .map_err(|error| NativeError::Io("Inspect fixture marker", error))?
        .file_attributes()
        & 0x400
        != 0
    {
        return Err(NativeError::InvalidArgument(
            "Measurement ownership marker cannot be a reparse point.",
        ));
    }
    if root
        .join("tokens.bin")
        .try_exists()
        .map_err(|error| NativeError::Io("Inspect fixture token boundary", error))?
    {
        return Err(NativeError::InvalidArgument(
            "Signed-out measurement cannot open a token cache.",
        ));
    }
    let file = std::fs::File::open(root.join("migration-fixture.json"))
        .map_err(|error| NativeError::Io("Open explicit measurement ownership marker", error))?;
    let mut bytes = Vec::new();
    file.take(1025)
        .read_to_end(&mut bytes)
        .map_err(|error| NativeError::Io("Read measurement marker", error))?;
    let marker: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| NativeError::InvalidArgument("Invalid measurement marker."))?;
    let object = marker
        .as_object()
        .filter(|object| object.len() == 2)
        .ok_or(NativeError::InvalidArgument(
            "Unexpected measurement marker fields.",
        ))?;
    let instance = object
        .get("instance_key")
        .and_then(serde_json::Value::as_str)
        .filter(|value| value.len() == 32 && value.bytes().all(|value| value.is_ascii_hexdigit()))
        .ok_or(NativeError::InvalidArgument(
            "Measurement key must be an explicit N-format GUID.",
        ))?;
    if bytes.len() > 1024
        || object.get("scope").and_then(serde_json::Value::as_str) != Some(scope)
        || key.is_some_and(|key| key != instance)
    {
        return Err(NativeError::InvalidArgument(
            "Measurement ownership marker mismatch.",
        ));
    }
    Ok(())
}
