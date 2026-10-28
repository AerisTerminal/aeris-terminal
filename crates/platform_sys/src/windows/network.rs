//! Connectivity-hint change callbacks.

use std::{ffi::c_void, sync::Arc};
use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, HANDLE},
    NetworkManagement::IpHelper::{CancelMibChangeNotify2, NotifyNetworkConnectivityHintChange},
    Networking::WinSock::NL_NETWORK_CONNECTIVITY_HINT,
};

use crate::network::{
    ChangeCallback, InterfaceChange, RegistrationError,
    availability_from_windows_connectivity_level,
};

/// The callback context is an `Arc` kept alive until Win32 confirms
/// cancellation. A failed cancellation leaks it deliberately so a late
/// operating-system callback can never dereference freed memory.
pub(crate) struct Registration {
    handle: usize,
    context: Option<Arc<ChangeCallback>>,
}

impl Registration {
    pub(crate) fn register(context: Arc<ChangeCallback>) -> Result<Self, RegistrationError> {
        let mut handle: HANDLE = std::ptr::null_mut();
        // SAFETY: The `Arc` allocation stays valid until cancellation succeeds
        // (or forever when it fails), the callback matches the documented ABI,
        // and `handle` is writable.
        let result = unsafe {
            NotifyNetworkConnectivityHintChange(
                Some(network_callback),
                Arc::as_ptr(&context).cast(),
                true,
                &raw mut handle,
            )
        };
        if result != ERROR_SUCCESS {
            return Err(RegistrationError::new(
                "NotifyNetworkConnectivityHintChange",
                Some(result),
            ));
        }
        Ok(Self {
            handle: handle as usize,
            context: Some(context),
        })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: `handle` was returned by the matching notification call and
        // is cancelled once.
        let result = unsafe { CancelMibChangeNotify2(self.handle as HANDLE) };
        if result != ERROR_SUCCESS
            && let Some(context) = self.context.take()
        {
            let _ = Arc::into_raw(context);
        }
    }
}

unsafe extern "system" fn network_callback(
    context: *const c_void,
    hint: NL_NETWORK_CONNECTIVITY_HINT,
) {
    // SAFETY: Registration keeps this context alive until cancellation
    // succeeds, and leaks it otherwise; the callback only borrows it.
    let callback = unsafe { &*context.cast::<ChangeCallback>() };
    callback.call(InterfaceChange::Availability(
        availability_from_windows_connectivity_level(hint.ConnectivityLevel),
    ));
}
