//! Reparse-point probing — symlink / junction / mount-point detection.
//!
//! Implements the **DO NOT FOLLOW** contract (SPEC §17 / INV-004): the probe
//! opens the path with `FILE_FLAG_OPEN_REPARSE_POINT`, so it inspects the
//! reparse point itself and never resolves through to its target. Phase 5
//! uses this to refuse traversal into symlinks/junctions and to revalidate
//! targets before deletion.

use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use windows::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES};
use windows::Win32::System::Ioctl::FSCTL_GET_REPARSE_POINT;
use windows::Win32::System::SystemServices::{IO_REPARSE_TAG_MOUNT_POINT, IO_REPARSE_TAG_SYMLINK};

use crate::ffi::{classify_error, open_existing};
use crate::filesystem::extended_for_access;
use crate::FileSystemError;

/// The kind of reparse point found at a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReparseTag {
    /// Symbolic link (`IO_REPARSE_TAG_SYMLINK`).
    Symlink,
    /// Directory junction (`IO_REPARSE_TAG_MOUNT_POINT` over a directory).
    Junction,
    /// Volume mount point (`IO_REPARSE_TAG_MOUNT_POINT` over a volume).
    MountPoint,
    /// Any other reparse tag (cloud placeholders, WSL, ...). Never followed.
    Other,
}

/// Result of [`probe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReparseInfo {
    /// `Some(tag)` when the path is a reparse point, `None` when it is a
    /// regular file/directory.
    pub tag: Option<ReparseTag>,
    /// Resolved target for symlinks/junctions/mount points (substitute name
    /// with the `\??\` device prefix stripped), `None` for `Other` tags.
    pub target: Option<PathBuf>,
}

/// Maximum reparse data buffer (16 KiB). `REPARSE_DATA_BUFFER` is
/// variable-length; 16 KiB comfortably covers the longest legal reparse data.
const REPARSE_DATA_BUFFER_CAPACITY: usize = 16 * 1024;

/// Win32 error code `ERROR_NOT_A_REPARSE_POINT`.
const ERROR_NOT_A_REPARSE_POINT: u32 = 4390;

/// Byte layout of the fixed `REPARSE_DATA_BUFFER` header.
///
/// Layout (both tags): `ReparseTag` DWORD @0, `ReparseDataLength` WORD @4,
/// `Reserved` WORD @6, then the tag-specific union starting @8:
///
/// - `SubstituteNameOffset` u16 @8, `SubstituteNameLength` u16 @10
///   (the offset is relative to the start of `PathBuffer`);
/// - mount points keep `PathBuffer` at byte 16;
/// - symlinks carry an extra `Flags` DWORD, so their `PathBuffer` sits at
///   byte 20.
const OFF_TAG: usize = 0; // u32: reparse tag
const OFF_SUBSTITUTE_OFFSET: usize = 8; // u16: offset of substitute name (in PathBuffer)
const OFF_SUBSTITUTE_LENGTH: usize = 10; // u16: byte length of substitute name
const PATH_BUFFER_OFFSET_MOUNT_POINT: usize = 16;
const PATH_BUFFER_OFFSET_SYMLINK: usize = 20;

/// Probes `path` and reports whether it is a reparse point and, when it is, of
/// which kind and to which target.
///
/// - Never follows the reparse point (`FILE_FLAG_OPEN_REPARSE_POINT`).
/// - Regular files/directories yield `tag: None` (not an error).
/// - A missing / inaccessible path is reported as a structured error so the
///   caller can decide (Phase 5 fails closed on unknown).
pub fn probe(path: impl AsRef<Path>) -> Result<ReparseInfo, FileSystemError> {
    let path = path.as_ref();
    let target = extended_for_access(path);
    // Opening a directory requires BACKUP_SEMANTICS; OPEN_REPARSE_POINT keeps
    // us on the link itself. READ_ATTRIBUTES is the least privilege needed to
    // issue the reparse query.
    let handle = open_existing(
        &target,
        FILE_READ_ATTRIBUTES.0,
        FILE_FLAG_OPEN_REPARSE_POINT,
    )?;

    let mut buffer = vec![0u8; REPARSE_DATA_BUFFER_CAPACITY];
    let mut bytes_returned = 0u32;

    // SAFETY: `buffer` is a valid 16 KiB mutable buffer owned for the call;
    // `bytes_returned` is written by the kernel only on success. `handle` is
    // the open file handle from the caller-supplied path. No aliasing: the
    // output buffer is not reachable from the input side (input is NULL).
    #[allow(unsafe_code)]
    let ioctl = unsafe {
        windows::Win32::System::IO::DeviceIoControl(
            handle.raw(),
            FSCTL_GET_REPARSE_POINT,
            None,
            0,
            Some(buffer.as_mut_ptr().cast()),
            buffer.len() as u32,
            Some(&mut bytes_returned),
            None,
        )
    };

    match ioctl {
        Ok(()) => {}
        Err(e) => {
            let code = (e.code().0 as u32) & 0xFFFF;
            if code == ERROR_NOT_A_REPARSE_POINT {
                return Ok(ReparseInfo {
                    tag: None,
                    target: None,
                });
            }
            return Err(classify_error(path, &e));
        }
    }

    let used = (bytes_returned as usize).min(buffer.len());
    if used < OFF_SUBSTITUTE_LENGTH + 2 {
        return Err(FileSystemError::Other {
            path: path.to_path_buf(),
            code: 0,
            message: "FSCTL_GET_REPARSE_POINT returned a truncated buffer".into(),
        });
    }

    let tag_value = u32::from_le_bytes(buffer[OFF_TAG..OFF_TAG + 4].try_into().unwrap());

    match tag_value {
        IO_REPARSE_TAG_SYMLINK => {
            let target = read_substitute_name(&buffer, used, /* symlink */ true);
            Ok(ReparseInfo {
                tag: Some(ReparseTag::Symlink),
                target,
            })
        }
        IO_REPARSE_TAG_MOUNT_POINT => {
            let target = read_substitute_name(&buffer, used, /* symlink */ false);
            let tag = if let Some(t) = &target {
                // Distinguish junction from volume mount point by inspecting
                // the target shape: volume mount points point at a
                // `\\?\Volume{GUID}\` path.
                if is_volume_path(t) {
                    ReparseTag::MountPoint
                } else {
                    ReparseTag::Junction
                }
            } else {
                ReparseTag::Junction
            };
            Ok(ReparseInfo {
                tag: Some(tag),
                target,
            })
        }
        _ => Ok(ReparseInfo {
            tag: Some(ReparseTag::Other),
            target: None,
        }),
    }
}

fn is_volume_path(target: &Path) -> bool {
    // Volume mount points use `\\?\Volume{...}\` as target.
    let s = target.to_string_lossy();
    s.starts_with("\\\\?\\Volume{")
}

/// Extracts the substitute name from a reparse data buffer and converts it to
/// a usable [`PathBuf`].
fn read_substitute_name(buffer: &[u8], used: usize, is_symlink: bool) -> Option<PathBuf> {
    let path_buffer_start = if is_symlink {
        PATH_BUFFER_OFFSET_SYMLINK
    } else {
        PATH_BUFFER_OFFSET_MOUNT_POINT
    };
    let sub_offset = u16::from_le_bytes(
        buffer[OFF_SUBSTITUTE_OFFSET..OFF_SUBSTITUTE_OFFSET + 2]
            .try_into()
            .unwrap(),
    ) as usize;
    let sub_len = u16::from_le_bytes(
        buffer[OFF_SUBSTITUTE_LENGTH..OFF_SUBSTITUTE_LENGTH + 2]
            .try_into()
            .unwrap(),
    ) as usize;

    let begin = path_buffer_start + sub_offset;
    let end = begin + sub_len;
    if sub_len == 0 || sub_len % 2 != 0 || end > used || end > buffer.len() {
        return None;
    }

    let mut units = Vec::with_capacity(sub_len / 2);
    for chunk in buffer[begin..end].chunks_exact(2) {
        let u = u16::from_le_bytes([chunk[0], chunk[1]]);
        if u == 0 {
            break; // defensive: some buffers include a trailing NUL
        }
        units.push(u);
    }
    if units.is_empty() {
        return None;
    }
    Some(substitute_to_path(&units))
}

/// Turns a reparse substitute name into a plain target path.
///
/// Substitute names are NT device paths: `\??\C:\dir` for local absolutes,
/// `\??\UNC\server\share\dir` for network paths and `\??\Volume{...}\` for
/// volume mount points. The device prefix is mapped back onto the user-visible
/// namespace (`\??\UNC\...` → `\\server\share`, `\??\Volume{...}` →
/// `\\?\Volume{...}`).
fn substitute_to_path(units: &[u16]) -> PathBuf {
    const PREFIX_DOS_DEVICE: [u16; 4] = [b'\\' as u16, b'?' as u16, b'?' as u16, b'\\' as u16];
    const PREFIX_EXTENDED: [u16; 4] = [b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
    const UNC: [u16; 4] = [b'U' as u16, b'N' as u16, b'C' as u16, b'\\' as u16];
    const VOLUME: [u16; 7] = [
        b'V' as u16,
        b'o' as u16,
        b'l' as u16,
        b'u' as u16,
        b'm' as u16,
        b'e' as u16,
        b'{' as u16,
    ];

    fn is_drive(rest: &[u16]) -> bool {
        rest.len() >= 2 && is_ascii_letter(rest[0]) && rest[1] == b':' as u16
    }

    fn is_ascii_letter(u: u16) -> bool {
        (0x41..=0x5A).contains(&u) || (0x61..=0x7A).contains(&u)
    }

    fn starts_with(units: &[u16], lit: &[u16]) -> bool {
        units.len() >= lit.len() && units[..lit.len()] == *lit
    }

    let out = if starts_with(units, &PREFIX_DOS_DEVICE) {
        let rest = &units[4..];
        if starts_with(rest, &UNC) {
            // `\??\UNC\server\share\...` → `\\server\share\...`
            let mut v = vec![b'\\' as u16, b'\\' as u16];
            v.extend_from_slice(&rest[4..]);
            v
        } else if starts_with(rest, &VOLUME) {
            // `\??\Volume{GUID}\...` → `\\?\Volume{GUID}\...`
            let mut v = vec![b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
            v.extend_from_slice(rest);
            v
        } else if is_drive(rest) {
            // `\??\C:\...` → `C:\...` (native drive path)
            rest.to_vec()
        } else {
            rest.to_vec()
        }
    } else if starts_with(units, &PREFIX_EXTENDED) {
        let rest = &units[4..];
        if starts_with(rest, &UNC) {
            let mut v = vec![b'\\' as u16, b'\\' as u16];
            v.extend_from_slice(&rest[4..]);
            v
        } else {
            // Keep the extended prefix (volume GUID paths etc.).
            units.to_vec()
        }
    } else {
        units.to_vec()
    };

    PathBuf::from(std::ffi::OsString::from_wide(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn units(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn substitute_strips_dos_device_prefix() {
        let p = substitute_to_path(&units("\\??\\C:\\dir\\target"));
        assert_eq!(p, PathBuf::from("C:\\dir\\target"));
    }

    #[test]
    fn substitute_converts_unc_form() {
        let p = substitute_to_path(&units("\\??\\UNC\\server\\share\\dir"));
        assert_eq!(p, PathBuf::from("\\\\server\\share\\dir"));
    }

    #[test]
    fn substitute_keeps_extended_and_relative_forms() {
        assert_eq!(
            substitute_to_path(&units("\\\\?\\Volume{abc}\\x")),
            PathBuf::from("\\\\?\\Volume{abc}\\x")
        );
        // Relative substitute names (rare) pass through untouched.
        assert_eq!(
            substitute_to_path(&units("..\\relative")),
            PathBuf::from("..\\relative")
        );
    }

    #[test]
    fn volume_mount_point_detection() {
        assert!(is_volume_path(Path::new("\\\\?\\Volume{1a2b3c4d}\\x")));
        assert!(!is_volume_path(Path::new("\\\\?\\C:\\x")));
        assert!(!is_volume_path(Path::new("C:\\x")));
    }
}
