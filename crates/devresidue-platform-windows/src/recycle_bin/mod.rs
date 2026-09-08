//! Windows Recycle Bin capability.
//!
//! Moves a file-system object to the Recycle Bin through the Shell API
//! (`IFileOperation` with `FOFX_RECYCLEONDELETE`).
//!
//! **Capability primitive only.** Nothing in this crate decides *whether* an
//! object should be recycled. The only intended caller is the Phase 6
//! `CleanupEngine` (the single deletion authority, INV-010) executing an
//! approved `RecycleBin` plan; until then nothing outside tests may call this.

use std::path::{Path, PathBuf};

use thiserror::Error;
use windows::core::{GUID, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Shell::{
    IFileOperation, IShellItem, SHCreateItemFromParsingName, FOFX_RECYCLEONDELETE,
    FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT,
};

use crate::ffi::to_wide;

/// The `FileOperation` COM class id (`CLSID_FileOperation`, from
/// `ShObjIdl_core.h`): `{3AD05575-8857-4850-9277-11B85BDB8E09}`. Note this is
/// *not* the `IFileOperation` interface id (`{947AAB5F-...}`) exposed by the
/// `windows` crate.
const CLSID_FILE_OPERATION: GUID = GUID::from_u128(0x3AD05575_8857_4850_9277_11B85BDB8E09);

/// HRESULT `RPC_E_CHANGED_MODE` — the calling thread is already in an
/// apartment of a different model, so our `CoInitializeEx(MTA)` cannot run.
const RPC_E_CHANGED_MODE: i32 = -2_147_024_634; // 0x80010106

/// Errors from the recycle-bin capability.
#[derive(Debug, Error)]
pub enum RecycleBinError {
    #[error("recycle target does not exist: {path}")]
    NotFound { path: PathBuf },
    #[error("COM could not be initialised on this thread (HRESULT 0x{hr:08X})")]
    ComInitFailed { hr: i32 },
    #[error("failed to create the shell FileOperation object: {message} (0x{code:08X})")]
    CreateOperationFailed { code: i32, message: String },
    #[error("failed to resolve '{path}' as a shell item: {message} (0x{code:08X})")]
    ItemCreationFailed {
        path: PathBuf,
        code: i32,
        message: String,
    },
    #[error("recycle operation failed for '{path}': {message} (0x{code:08X})")]
    PerformFailed {
        path: PathBuf,
        code: i32,
        message: String,
    },
}

/// Com init bookkeeping so `CoUninitialize` is only called when *we* were the
/// ones who initialised this thread's COM.
enum ComInit {
    /// `CoInitializeEx` returned S_OK — we own the initialisation.
    New,
    /// Thread already had a compatible COM apartment (S_FALSE) or an
    /// incompatible one (`RPC_E_CHANGED_MODE`): we must not un-initialise.
    PreExisting,
}

fn init_com() -> Result<ComInit, RecycleBinError> {
    // SAFETY: CoInitializeEx takes NULL for its reserved argument; the
    // returned HRESULT tells us whether the thread was newly initialised.
    #[allow(unsafe_code)]
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_ok() {
        Ok(if hr.0 == 0 {
            ComInit::New
        } else {
            ComInit::PreExisting
        })
    } else if hr.0 == RPC_E_CHANGED_MODE {
        // Thread is in a different (STA) apartment — reuse it as-is.
        Ok(ComInit::PreExisting)
    } else {
        Err(RecycleBinError::ComInitFailed { hr: hr.0 })
    }
}

/// Moves `path` into the Windows Recycle Bin.
///
/// The operation is silent and non-interactive (`FOF_SILENT |
/// FOF_NOCONFIRMATION | FOF_NOERRORUI`) and requests recycle-on-delete
/// (`FOFX_RECYCLEONDELETE`).
pub fn recycle(path: impl AsRef<Path>) -> Result<(), RecycleBinError> {
    let path = path.as_ref();
    if !path.exists() {
        return Err(RecycleBinError::NotFound {
            path: path.to_path_buf(),
        });
    }

    let init = init_com()?;
    let result = do_recycle(path);
    if matches!(init, ComInit::New) {
        // SAFETY: balanced against the CoInitializeEx that returned S_OK above.
        #[allow(unsafe_code)]
        unsafe {
            CoUninitialize();
        }
    }
    result
}

fn do_recycle(path: &Path) -> Result<(), RecycleBinError> {
    // SAFETY: CoCreateInstance fills the strongly-typed interface; CLSCTX_ALL
    // is a plain constant, `punkOuter` is NULL for non-aggregated creation.
    #[allow(unsafe_code)]
    let operation: IFileOperation = unsafe {
        CoCreateInstance(&CLSID_FILE_OPERATION, None, CLSCTX_ALL)
    }
    .map_err(|e| RecycleBinError::CreateOperationFailed {
        code: e.code().0,
        message: e.message().to_string(),
    })?;

    // SAFETY: flags are plain bit constants; no pointers involved.
    #[allow(unsafe_code)]
    unsafe {
        operation.SetOperationFlags(
            FOF_SILENT | FOF_NOCONFIRMATION | FOF_NOERRORUI | FOFX_RECYCLEONDELETE,
        )
    }
    .map_err(|e| RecycleBinError::CreateOperationFailed {
        code: e.code().0,
        message: e.message().to_string(),
    })?;

    let wide = to_wide(path);
    // SAFETY: `wide` (nul-terminated) outlives this call and the shell item
    // borrows the string only during creation. Returns a strongly-typed
    // `IShellItem`.
    #[allow(unsafe_code)]
    let item: IShellItem =
        unsafe { SHCreateItemFromParsingName::<_, _, IShellItem>(PCWSTR(wide.as_ptr()), None) }
            .map_err(|e| RecycleBinError::ItemCreationFailed {
                path: path.to_path_buf(),
                code: e.code().0,
                message: e.message().to_string(),
            })?;

    // SAFETY: `item` is a valid IShellItem from SHCreateItemFromParsingName;
    // the second parameter (a per-item FILEOP_FLAGS) is optional.
    #[allow(unsafe_code)]
    unsafe { operation.DeleteItem(&item, None) }.map_err(|e| RecycleBinError::PerformFailed {
        path: path.to_path_buf(),
        code: e.code().0,
        message: e.message().to_string(),
    })?;

    // SAFETY: no parameters.
    #[allow(unsafe_code)]
    unsafe { operation.PerformOperations() }.map_err(|e| RecycleBinError::PerformFailed {
        path: path.to_path_buf(),
        code: e.code().0,
        message: e.message().to_string(),
    })?;

    Ok(())
}
