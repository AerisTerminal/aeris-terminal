//! Embedded Authenticode verification through `WinVerifyTrust`.

use std::{io, os::windows::ffi::OsStrExt as _, path::Path, ptr};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Security::Cryptography::{
    CERT_CONTEXT, CERT_SHA1_HASH_PROP_ID, CertGetCertificateContextProperty,
};
use windows_sys::Win32::Security::WinTrust::{
    CRYPT_PROVIDER_SGNR, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0,
    WINTRUST_FILE_INFO, WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE, WTD_DISABLE_MD2_MD4,
    WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrust,
};

const SHA1_BYTES: usize = 20;

/// The trusted embedded signature of one file.
pub(crate) struct EmbeddedSignature {
    pub(crate) signer_thumbprint: [u8; SHA1_BYTES],
    pub(crate) timestamped: bool,
}

pub(crate) enum Verification {
    Trusted(EmbeddedSignature),
    /// `WinVerifyTrust` refused the file with this status.
    Untrusted(i32),
    /// Trusted, but the provider exposed no readable signer certificate.
    SignerUnavailable,
}

/// Verifies the file's embedded signature and certificate chain. Catalog
/// signatures are not consulted: release executables carry their own.
pub(crate) fn verify_embedded_signature(path: &Path) -> io::Result<Verification> {
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: struct_size::<WINTRUST_FILE_INFO>()?,
        pcwszFilePath: path.as_ptr(),
        ..WINTRUST_FILE_INFO::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: struct_size::<WINTRUST_DATA>()?,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_NONE,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 {
            pFile: &raw mut file,
        },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_CACHE_ONLY_URL_RETRIEVAL | WTD_DISABLE_MD2_MD4,
        ..WINTRUST_DATA::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    // SAFETY: `data` is a fully initialized WINTRUST_DATA whose file info
    // and NUL-terminated path buffer outlive this call and the close below;
    // WTD_UI_NONE needs no window.
    let status = unsafe {
        WinVerifyTrust(
            ptr::null_mut(),
            &raw mut action,
            (&raw mut data).cast::<core::ffi::c_void>(),
        )
    };
    let verification = if status == 0 {
        trusted_signature(data.hWVTStateData)
            .map_or(Verification::SignerUnavailable, Verification::Trusted)
    } else {
        Verification::Untrusted(status)
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    // SAFETY: As above; the close action releases the provider state the
    // verify action opened, whatever its verdict, and reports no verdict.
    unsafe {
        WinVerifyTrust(
            ptr::null_mut(),
            &raw mut action,
            (&raw mut data).cast::<core::ffi::c_void>(),
        )
    };
    Ok(verification)
}

fn struct_size<T>() -> io::Result<u32> {
    u32::try_from(std::mem::size_of::<T>()).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))
}

/// Reads the signer and timestamp of a successful verification while its
/// provider state is open; every pointer it follows is owned by that state.
fn trusted_signature(state: HANDLE) -> Option<EmbeddedSignature> {
    // SAFETY: `state` is the open state handle WinVerifyTrust returned for
    // this verification, and the helper only reads it.
    let provider = unsafe { WTHelperProvDataFromStateData(state) };
    if provider.is_null() {
        return None;
    }
    // SAFETY: `provider` is the non-null provider data of the open state;
    // the helper returns a pointer into it, or null for a missing index.
    let signer = unsafe { WTHelperGetProvSignerFromChain(provider, 0, 0, 0) };
    // SAFETY: A non-null signer points at provider-owned data that stays
    // valid and unaliased by writers until the state closes.
    let signer = unsafe { signer.as_ref() }?;
    let signer_thumbprint = sha1_thumbprint(leaf_certificate(signer)?)?;
    let timestamped = signer.csCounterSigners > 0 && {
        // SAFETY: As above; counter-signer index 0 exists because the
        // signer reports at least one.
        let timestamper = unsafe { WTHelperGetProvSignerFromChain(provider, 0, 1, 0) };
        // SAFETY: As for the signer above.
        unsafe { timestamper.as_ref() }
            .and_then(leaf_certificate)
            .is_some()
    };
    Some(EmbeddedSignature {
        signer_thumbprint,
        timestamped,
    })
}

fn leaf_certificate(signer: &CRYPT_PROVIDER_SGNR) -> Option<*const CERT_CONTEXT> {
    if signer.csCertChain == 0 {
        return None;
    }
    // SAFETY: `pasCertChain` holds `csCertChain` provider-owned entries
    // while the state is open, and the first is the signing certificate.
    let leaf = unsafe { signer.pasCertChain.as_ref() }?;
    (!leaf.pCert.is_null()).then_some(leaf.pCert)
}

fn sha1_thumbprint(certificate: *const CERT_CONTEXT) -> Option<[u8; SHA1_BYTES]> {
    let mut thumbprint = [0_u8; SHA1_BYTES];
    let mut length = u32::try_from(SHA1_BYTES).ok()?;
    // SAFETY: `certificate` is a live provider-owned context, `thumbprint` is
    // writable for `length` bytes, and `length` is a valid in/out pointer for
    // this single call.
    let read = unsafe {
        CertGetCertificateContextProperty(
            certificate,
            CERT_SHA1_HASH_PROP_ID,
            thumbprint.as_mut_ptr().cast::<core::ffi::c_void>(),
            &raw mut length,
        )
    };
    (read != 0 && usize::try_from(length).ok() == Some(SHA1_BYTES)).then_some(thumbprint)
}
