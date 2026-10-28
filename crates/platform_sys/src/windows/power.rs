//! Power-manager suspend/resume callbacks.

use std::{ffi::c_void, ptr, sync::Arc};
use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, HANDLE},
    System::Power::{
        DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, PowerRegisterSuspendResumeNotification,
        PowerUnregisterSuspendResumeNotification,
    },
    UI::WindowsAndMessaging::{DEVICE_NOTIFY_CALLBACK, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND},
};

use crate::power::{PowerCallback, PowerEvent, RegistrationError};

/// The callback context is an `Arc` kept alive until Win32 confirms
/// unregistration. A failed unregistration leaks it deliberately so a late
/// operating-system callback can never dereference freed memory.
pub(crate) struct Registration {
    handle: isize,
    parameters: usize,
    context: Option<Arc<PowerCallback>>,
}

impl Registration {
    pub(crate) fn register(context: Arc<PowerCallback>) -> Result<Self, RegistrationError> {
        let mut parameters = Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS {
            Callback: Some(power_callback),
            Context: Arc::as_ptr(&context).cast_mut().cast(),
        });
        let mut handle = ptr::null_mut();
        // SAFETY: `parameters` and its `Arc` context stay valid until the
        // matching unregistration succeeds (or forever when it fails), the
        // callback matches the documented ABI, and `handle` is writable.
        let result = unsafe {
            PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                (&raw mut *parameters).cast::<c_void>() as HANDLE,
                &raw mut handle,
            )
        };
        if result != ERROR_SUCCESS {
            return Err(RegistrationError::new(
                "PowerRegisterSuspendResumeNotification",
                Some(result),
            ));
        }
        Ok(Self {
            handle: handle.addr().cast_signed(),
            parameters: Box::into_raw(parameters).addr(),
            context: Some(context),
        })
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: `handle` was returned by the matching registration call and
        // is unregistered once.
        let result = unsafe { PowerUnregisterSuspendResumeNotification(self.handle) };
        if result == ERROR_SUCCESS {
            // SAFETY: This address came from `Box::into_raw` in `register`,
            // and successful unregistration means Win32 no longer uses it.
            drop(unsafe {
                Box::from_raw(self.parameters as *mut DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS)
            });
        } else if let Some(context) = self.context.take() {
            let _ = Arc::into_raw(context);
        }
    }
}

const fn power_event(event_type: u32) -> Option<PowerEvent> {
    match event_type {
        PBT_APMSUSPEND => Some(PowerEvent::Suspending),
        PBT_APMRESUMEAUTOMATIC => Some(PowerEvent::Resumed),
        _ => None,
    }
}

unsafe extern "system" fn power_callback(
    context: *const c_void,
    event_type: u32,
    _setting: *const c_void,
) -> u32 {
    let Some(event) = power_event(event_type) else {
        return ERROR_SUCCESS;
    };
    // SAFETY: Registration keeps this context alive until unregistration
    // succeeds, and leaks it otherwise; the callback only borrows it.
    let callback = unsafe { &*context.cast::<PowerCallback>() };
    callback.call(event);
    ERROR_SUCCESS
}

#[cfg(test)]
mod tests {
    use super::power_event;
    use crate::power::PowerEvent;

    #[test]
    fn windows_power_event_types_map_without_duplicate_resume_events() {
        assert_eq!(power_event(4), Some(PowerEvent::Suspending));
        assert_eq!(power_event(18), Some(PowerEvent::Resumed));
        for event_type in [0, 6, 7, 8, u32::MAX] {
            assert_eq!(power_event(event_type), None);
        }
    }
}
