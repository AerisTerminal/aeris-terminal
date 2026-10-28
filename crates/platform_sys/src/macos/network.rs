//! `SCNetworkReachability` for the default route on a dedicated run-loop
//! thread.

use std::{
    ffi::c_void,
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{SyncSender, sync_channel},
    },
    thread::{self, JoinHandle},
};

use crate::network::{
    ChangeCallback, InterfaceChange, RegistrationError, availability_from_macos_reachability_flags,
};

const AF_INET: u8 = 2;
const SOCKADDR_IN_LEN: u8 = 16;
const RUN_LOOP_FINISHED: i32 = 1;
/// Bounds how long a stop requested before the run loop starts can go unseen.
const RUN_LOOP_SLICE_SECONDS: f64 = 1.0;

type ReachabilityRef = *const c_void;
type RunLoopRef = *mut c_void;
type RunLoopMode = *const c_void;

#[repr(C)]
struct SockAddrIn {
    len: u8,
    family: u8,
    port: u16,
    address: u32,
    zero: [u8; 8],
}

const _: () = assert!(std::mem::size_of::<SockAddrIn>() == SOCKADDR_IN_LEN as usize);

#[repr(C)]
struct ReachabilityContext {
    version: isize,
    info: *mut c_void,
    retain: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
    release: Option<unsafe extern "C" fn(*const c_void)>,
    copy_description: Option<unsafe extern "C" fn(*const c_void) -> *const c_void>,
}

type ReachabilityCallback = unsafe extern "C" fn(ReachabilityRef, u32, *mut c_void);

#[link(name = "SystemConfiguration", kind = "framework")]
unsafe extern "C" {
    fn SCNetworkReachabilityCreateWithAddress(
        allocator: *const c_void,
        address: *const c_void,
    ) -> ReachabilityRef;
    fn SCNetworkReachabilityGetFlags(target: ReachabilityRef, flags: *mut u32) -> u8;
    fn SCNetworkReachabilitySetCallback(
        target: ReachabilityRef,
        callback: Option<ReachabilityCallback>,
        context: *mut ReachabilityContext,
    ) -> u8;
    fn SCNetworkReachabilityScheduleWithRunLoop(
        target: ReachabilityRef,
        run_loop: RunLoopRef,
        mode: RunLoopMode,
    ) -> u8;
    fn SCNetworkReachabilityUnscheduleFromRunLoop(
        target: ReachabilityRef,
        run_loop: RunLoopRef,
        mode: RunLoopMode,
    ) -> u8;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    safe static kCFRunLoopDefaultMode: RunLoopMode;
    fn CFRelease(value: *const c_void);
    fn CFRunLoopGetCurrent() -> RunLoopRef;
    fn CFRunLoopRunInMode(mode: RunLoopMode, seconds: f64, return_after_source_handled: u8) -> i32;
    fn CFRunLoopStop(run_loop: RunLoopRef);
}

/// The worker thread that owns the reachability target and runs its run loop
/// until the registration is dropped.
pub(crate) struct Registration {
    cancelled: Arc<AtomicBool>,
    run_loop: Arc<Mutex<Option<usize>>>,
    thread: Option<JoinHandle<()>>,
}

impl Registration {
    pub(crate) fn register(callback: Arc<ChangeCallback>) -> Result<Self, RegistrationError> {
        let cancelled = Arc::new(AtomicBool::new(false));
        let run_loop = Arc::new(Mutex::new(None));
        let thread_cancelled = Arc::clone(&cancelled);
        let thread_run_loop = Arc::clone(&run_loop);
        let (ready_tx, ready_rx) = sync_channel(1);
        let thread = thread::Builder::new()
            .name("aeris-native-network-monitor".to_string())
            .spawn(move || {
                if let Err(error) =
                    run_monitor(&callback, &thread_run_loop, &thread_cancelled, &ready_tx)
                {
                    let _ = ready_tx.try_send(Err(error));
                }
                callback.call(InterfaceChange::Stopped);
            })
            .map_err(|_| RegistrationError::new("spawn reachability monitor thread", None))?;
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                cancelled,
                run_loop,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                let _ = thread.join();
                Err(RegistrationError::new("reachability monitor thread", None))
            }
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        stop_run_loop(&self.run_loop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_monitor(
    callback: &ChangeCallback,
    run_loop_slot: &Mutex<Option<usize>>,
    cancelled: &AtomicBool,
    ready: &SyncSender<Result<(), RegistrationError>>,
) -> Result<(), RegistrationError> {
    let address = SockAddrIn {
        len: SOCKADDR_IN_LEN,
        family: AF_INET,
        port: 0,
        address: 0,
        zero: [0; 8],
    };
    // SAFETY: `address` has Darwin's `sockaddr_in` layout and outlives the
    // call, which copies it.
    let reachability = unsafe {
        SCNetworkReachabilityCreateWithAddress(ptr::null(), ptr::from_ref(&address).cast())
    };
    if reachability.is_null() {
        return Err(RegistrationError::new(
            "SCNetworkReachabilityCreateWithAddress",
            None,
        ));
    }
    let mut target = ReachabilityTarget {
        reachability,
        scheduled_on: None,
    };
    let mut context = ReachabilityContext {
        version: 0,
        info: ptr::from_ref(callback).cast_mut().cast(),
        retain: None,
        release: None,
        copy_description: None,
    };
    // SAFETY: The call copies `context`. Its `info` callback outlives the
    // target, which `target` unschedules and releases before this function
    // returns, and callbacks fire only on this thread's run loop.
    if unsafe {
        SCNetworkReachabilitySetCallback(
            reachability,
            Some(reachability_callback),
            &raw mut context,
        )
    } == 0
    {
        return Err(RegistrationError::new(
            "SCNetworkReachabilitySetCallback",
            None,
        ));
    }
    // SAFETY: CFRunLoopGetCurrent has no preconditions.
    let run_loop = unsafe { CFRunLoopGetCurrent() };
    if run_loop.is_null() {
        return Err(RegistrationError::new("CFRunLoopGetCurrent", None));
    }
    // SAFETY: The target and this thread's run loop are live, and the mode is
    // CoreFoundation's exported default.
    if unsafe {
        SCNetworkReachabilityScheduleWithRunLoop(reachability, run_loop, kCFRunLoopDefaultMode)
    } == 0
    {
        return Err(RegistrationError::new(
            "SCNetworkReachabilityScheduleWithRunLoop",
            None,
        ));
    }
    target.scheduled_on = Some(run_loop);
    let _published = PublishedRunLoop::publish(run_loop_slot, run_loop)?;

    let mut flags = 0_u32;
    // SAFETY: The target is live and `flags` is a writable out-parameter.
    if unsafe { SCNetworkReachabilityGetFlags(reachability, &raw mut flags) } == 0 {
        return Err(RegistrationError::new(
            "SCNetworkReachabilityGetFlags",
            None,
        ));
    }
    callback.call(InterfaceChange::Availability(
        availability_from_macos_reachability_flags(flags),
    ));
    ready
        .send(Ok(()))
        .map_err(|_| RegistrationError::new("reachability monitor ready", None))?;

    while !cancelled.load(Ordering::Acquire) {
        // SAFETY: This runs the current thread's run loop, which holds the
        // scheduled reachability source, for one bounded slice.
        if unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, RUN_LOOP_SLICE_SECONDS, 0) }
            == RUN_LOOP_FINISHED
        {
            break;
        }
    }
    Ok(())
}

/// Owns the create-rule reference and, once scheduled, the run-loop schedule.
struct ReachabilityTarget {
    reachability: ReachabilityRef,
    scheduled_on: Option<RunLoopRef>,
}

impl Drop for ReachabilityTarget {
    fn drop(&mut self) {
        if let Some(run_loop) = self.scheduled_on {
            // SAFETY: This is the target/run-loop pair scheduled in
            // `run_monitor`, unscheduled once before release.
            let _ = unsafe {
                SCNetworkReachabilityUnscheduleFromRunLoop(
                    self.reachability,
                    run_loop,
                    kCFRunLoopDefaultMode,
                )
            };
        }
        // SAFETY: This thread owns the create-rule reference exactly once.
        unsafe { CFRelease(self.reachability) };
    }
}

/// Publishes the worker's run loop for [`stop_run_loop`] and withdraws it
/// before the run loop can exit.
struct PublishedRunLoop<'a>(&'a Mutex<Option<usize>>);

impl<'a> PublishedRunLoop<'a> {
    fn publish(
        slot: &'a Mutex<Option<usize>>,
        run_loop: RunLoopRef,
    ) -> Result<Self, RegistrationError> {
        *slot
            .lock()
            .map_err(|_| RegistrationError::new("store reachability run loop", None))? =
            Some(run_loop as usize);
        Ok(Self(slot))
    }
}

impl Drop for PublishedRunLoop<'_> {
    fn drop(&mut self) {
        if let Ok(mut run_loop) = self.0.lock() {
            *run_loop = None;
        }
    }
}

unsafe extern "C" fn reachability_callback(
    _target: ReachabilityRef,
    flags: u32,
    info: *mut c_void,
) {
    // SAFETY: `info` is the `ChangeCallback` set in `run_monitor`, which
    // outlives the scheduled target.
    let Some(callback) = (unsafe { info.cast::<ChangeCallback>().as_ref() }) else {
        return;
    };
    callback.call(InterfaceChange::Availability(
        availability_from_macos_reachability_flags(flags),
    ));
}

fn stop_run_loop(run_loop: &Mutex<Option<usize>>) {
    if let Ok(run_loop) = run_loop.lock()
        && let Some(run_loop) = *run_loop
    {
        // SAFETY: The slot holds only the live run loop of the worker thread.
        // Holding its lock keeps the worker from withdrawing the pointer and
        // exiting while this call uses it.
        unsafe { CFRunLoopStop(run_loop as RunLoopRef) };
    }
}
