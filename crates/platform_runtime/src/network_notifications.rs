//! Native network-availability notification boundary.

use crate::CapabilityAvailability;
use std::{
    error::Error,
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::sync::{
    atomic::AtomicU8,
    mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
};

#[cfg(target_os = "macos")]
use std::sync::Mutex;

#[cfg(target_os = "windows")]
use std::sync::mpsc::RecvTimeoutError;

#[cfg(target_os = "linux")]
use zbus::{
    MatchRule,
    blocking::{Connection, MessageIterator, Proxy, connection},
    message::Type as MessageType,
};

#[cfg(target_os = "linux")]
const NETWORK_MANAGER_DESTINATION: &str = "org.freedesktop.NetworkManager";
#[cfg(target_os = "linux")]
const NETWORK_MANAGER_PATH: &str = "/org/freedesktop/NetworkManager";
#[cfg(target_os = "linux")]
const NETWORK_MANAGER_INTERFACE: &str = "org.freedesktop.NetworkManager";
#[cfg(target_os = "linux")]
const STATE_CHANGED_SIGNAL: &str = "StateChanged";
#[cfg(target_os = "linux")]
const STATE_PROPERTY: &str = "State";
#[cfg(target_os = "linux")]
const MAX_QUEUED_NETWORK_EVENTS: usize = 16;
#[cfg(target_os = "linux")]
const MAX_OWNER_RECONCILIATION_ATTEMPTS: usize = 3;
#[cfg(target_os = "linux")]
const DBUS_DESTINATION: &str = "org.freedesktop.DBus";
#[cfg(target_os = "linux")]
const DBUS_PATH: &str = "/org/freedesktop/DBus";
#[cfg(target_os = "linux")]
const DBUS_INTERFACE: &str = "org.freedesktop.DBus";
#[cfg(target_os = "linux")]
const NAME_OWNER_CHANGED_SIGNAL: &str = "NameOwnerChanged";

#[cfg(any(target_os = "macos", target_os = "windows"))]
struct NetworkEventPublisher {
    latest: Arc<AtomicU8>,
    wake: SyncSender<()>,
    #[cfg(target_os = "macos")]
    closed: Arc<AtomicBool>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl NetworkEventPublisher {
    fn publish(&self, event: NetworkEvent) {
        self.latest
            .store(encode_network_event(event), Ordering::Release);
        match self.wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {}
        }
    }

    #[cfg(target_os = "macos")]
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        match self.wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {}
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
struct NetworkEventInbox {
    latest: Arc<AtomicU8>,
    cancelled: Arc<AtomicBool>,
    wake: Receiver<()>,
    #[cfg(target_os = "macos")]
    closed: Arc<AtomicBool>,
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
impl NetworkEventInbox {
    #[cfg(target_os = "windows")]
    fn channel() -> (
        Arc<NetworkEventPublisher>,
        Self,
        NativeNetworkMonitorCancellation,
    ) {
        let latest = Arc::new(AtomicU8::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let (wake, receiver) = sync_channel(1);
        (
            Arc::new(NetworkEventPublisher {
                latest: Arc::clone(&latest),
                wake: wake.clone(),
            }),
            Self {
                latest,
                cancelled: Arc::clone(&cancelled),
                wake: receiver,
            },
            NativeNetworkMonitorCancellation { cancelled, wake },
        )
    }

    #[cfg(target_os = "macos")]
    fn channel(
        run_loop: Arc<Mutex<Option<usize>>>,
    ) -> (
        Arc<NetworkEventPublisher>,
        Self,
        NativeNetworkMonitorCancellation,
    ) {
        let latest = Arc::new(AtomicU8::new(0));
        let cancelled = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(AtomicBool::new(false));
        let (wake, receiver) = sync_channel(1);
        (
            Arc::new(NetworkEventPublisher {
                latest: Arc::clone(&latest),
                wake: wake.clone(),
                closed: Arc::clone(&closed),
            }),
            Self {
                latest,
                cancelled: Arc::clone(&cancelled),
                wake: receiver,
                closed,
            },
            NativeNetworkMonitorCancellation {
                cancelled,
                wake,
                run_loop,
            },
        )
    }

    fn recv(&self) -> Result<NetworkEvent, NetworkNotificationError> {
        loop {
            if self.cancelled.load(Ordering::Acquire) {
                return Err(NetworkNotificationError::Cancelled);
            }
            if let Some(event) = self.take_latest() {
                return Ok(event);
            }
            #[cfg(target_os = "macos")]
            if self.closed.load(Ordering::Acquire) {
                return Err(NetworkNotificationError::StreamClosed);
            }
            self.wake
                .recv()
                .map_err(|_| NetworkNotificationError::StreamClosed)?;
        }
    }

    #[cfg(target_os = "windows")]
    fn recv_timeout(
        &self,
        timeout: std::time::Duration,
    ) -> Result<NetworkEvent, NetworkNotificationError> {
        if let Some(event) = self.take_latest() {
            return Ok(event);
        }
        match self.wake.recv_timeout(timeout) {
            Ok(()) => self
                .take_latest()
                .ok_or(NetworkNotificationError::InitialNotificationTimedOut),
            Err(RecvTimeoutError::Timeout) => {
                Err(NetworkNotificationError::InitialNotificationTimedOut)
            }
            Err(RecvTimeoutError::Disconnected) => Err(NetworkNotificationError::StreamClosed),
        }
    }

    fn take_latest(&self) -> Option<NetworkEvent> {
        decode_network_event(self.latest.swap(0, Ordering::AcqRel))
    }
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
mod macos {
    use super::{
        NativeNetworkMonitorCancellation, NetworkEvent, NetworkEventInbox, NetworkEventPublisher,
        NetworkNotificationError,
    };
    use std::{
        ffi::c_void,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
            mpsc::{SyncSender, sync_channel},
        },
        thread::{self, JoinHandle},
    };

    const AF_INET: u8 = 2;

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
        static kCFRunLoopDefaultMode: RunLoopMode;
        fn CFRelease(value: *const c_void);
        fn CFRunLoopGetCurrent() -> RunLoopRef;
        fn CFRunLoopRun();
        fn CFRunLoopStop(run_loop: RunLoopRef);
    }

    pub(super) struct Registration {
        cancellation: NativeNetworkMonitorCancellation,
        thread: Option<JoinHandle<()>>,
    }

    impl Registration {
        pub(super) fn connect() -> Result<
            (
                Self,
                NetworkEventInbox,
                NetworkEvent,
                NativeNetworkMonitorCancellation,
            ),
            NetworkNotificationError,
        > {
            let run_loop = Arc::new(Mutex::new(None));
            let (publisher, events, cancellation) =
                NetworkEventInbox::channel(Arc::clone(&run_loop));
            let cancelled = Arc::clone(&events.cancelled);
            let (ready_tx, ready_rx) = sync_channel(1);
            let thread = thread::Builder::new()
                .name("asceify-native-network-monitor".to_string())
                .spawn(move || {
                    let result = run_monitor(&publisher, &run_loop, &cancelled, &ready_tx);
                    if let Err(error) = result {
                        let _ = ready_tx.try_send(Err(error));
                    }
                    publisher.close();
                })
                .map_err(|_| NetworkNotificationError::MacOsPlatform {
                    operation: "spawn reachability monitor thread",
                })?;

            let current = match ready_rx.recv() {
                Ok(Ok(current)) => current,
                Ok(Err(error)) => {
                    let _ = thread.join();
                    return Err(error);
                }
                Err(_) => {
                    let _ = thread.join();
                    return Err(NetworkNotificationError::StreamClosed);
                }
            };
            Ok((
                Self {
                    cancellation: cancellation.clone(),
                    thread: Some(thread),
                },
                events,
                current,
                cancellation,
            ))
        }
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            self.cancellation.clone().cancel();
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn run_monitor(
        publisher: &NetworkEventPublisher,
        run_loop_slot: &Mutex<Option<usize>>,
        cancelled: &AtomicBool,
        ready: &SyncSender<Result<NetworkEvent, NetworkNotificationError>>,
    ) -> Result<(), NetworkNotificationError> {
        let address = SockAddrIn {
            len: u8::try_from(std::mem::size_of::<SockAddrIn>()).expect("sockaddr_in fits in u8"),
            family: AF_INET,
            port: 0,
            address: 0,
            zero: [0; 8],
        };
        // SAFETY: the sockaddr has Darwin's `sockaddr_in` layout and remains
        // alive for the duration of the creation call.
        let reachability = unsafe {
            SCNetworkReachabilityCreateWithAddress(
                std::ptr::null(),
                std::ptr::from_ref(&address).cast(),
            )
        };
        if reachability.is_null() {
            return Err(NetworkNotificationError::MacOsPlatform {
                operation: "SCNetworkReachabilityCreateWithAddress",
            });
        }
        let mut guard = ReachabilityGuard::new(reachability);
        let mut context = ReachabilityContext {
            version: 0,
            info: std::ptr::from_ref(publisher).cast_mut().cast(),
            retain: None,
            release: None,
            copy_description: None,
        };
        // SAFETY: `publisher` outlives this run loop thread and callback
        // scheduling; the callback only takes a shared reference to it.
        if unsafe {
            SCNetworkReachabilitySetCallback(
                reachability,
                Some(reachability_callback),
                &raw mut context,
            )
        } == 0
        {
            return Err(NetworkNotificationError::MacOsPlatform {
                operation: "SCNetworkReachabilitySetCallback",
            });
        }
        // SAFETY: CoreFoundation returns the current thread's live run loop.
        let run_loop = unsafe { CFRunLoopGetCurrent() };
        if run_loop.is_null() {
            return Err(NetworkNotificationError::MacOsPlatform {
                operation: "CFRunLoopGetCurrent",
            });
        }
        // SAFETY: reachability, run loop and the exported default mode are live
        // CoreFoundation objects on this thread.
        if unsafe {
            SCNetworkReachabilityScheduleWithRunLoop(reachability, run_loop, kCFRunLoopDefaultMode)
        } == 0
        {
            return Err(NetworkNotificationError::MacOsPlatform {
                operation: "SCNetworkReachabilityScheduleWithRunLoop",
            });
        }
        guard.1 = Some(run_loop);
        *run_loop_slot
            .lock()
            .map_err(|_| NetworkNotificationError::MacOsPlatform {
                operation: "store reachability run loop",
            })? = Some(run_loop as usize);

        let mut flags = 0_u32;
        // SAFETY: `reachability` remains owned by `guard` and `flags` is a
        // writable out-parameter for the synchronous state read.
        if unsafe { SCNetworkReachabilityGetFlags(reachability, &raw mut flags) } == 0 {
            clear_run_loop(run_loop_slot);
            return Err(NetworkNotificationError::MacOsPlatform {
                operation: "SCNetworkReachabilityGetFlags",
            });
        }
        let current = NetworkEvent::from_macos_reachability_flags(flags);
        ready
            .send(Ok(current))
            .map_err(|_| NetworkNotificationError::StreamClosed)?;
        if cancelled.load(Ordering::Acquire) {
            clear_run_loop(run_loop_slot);
            return Ok(());
        }

        // SAFETY: the reachability source is scheduled on this thread's run
        // loop. `cancel` stops this loop from another thread.
        unsafe { CFRunLoopRun() };
        clear_run_loop(run_loop_slot);
        Ok(())
    }

    struct ReachabilityGuard(ReachabilityRef, Option<RunLoopRef>);

    impl ReachabilityGuard {
        fn new(reachability: ReachabilityRef) -> Self {
            Self(reachability, None)
        }
    }

    impl Drop for ReachabilityGuard {
        fn drop(&mut self) {
            if let Some(run_loop) = self.1 {
                // SAFETY: this is the same reachability/run-loop pair that was
                // successfully scheduled above; cleanup happens before release.
                let _ = unsafe {
                    SCNetworkReachabilityUnscheduleFromRunLoop(
                        self.0,
                        run_loop,
                        kCFRunLoopDefaultMode,
                    )
                };
            }
            // SAFETY: this thread owns the create-rule reference exactly once.
            unsafe { CFRelease(self.0) };
        }
    }

    unsafe extern "C" fn reachability_callback(
        _target: ReachabilityRef,
        flags: u32,
        info: *mut c_void,
    ) {
        if info.is_null() {
            return;
        }
        // SAFETY: the registration context points at the publisher owned by the
        // run-loop thread and is unscheduled before that publisher is dropped.
        let publisher = unsafe { &*info.cast::<NetworkEventPublisher>() };
        publisher.publish(NetworkEvent::from_macos_reachability_flags(flags));
    }

    fn clear_run_loop(run_loop: &Mutex<Option<usize>>) {
        if let Ok(mut run_loop) = run_loop.lock() {
            *run_loop = None;
        }
    }

    pub(super) fn stop_run_loop(run_loop: &Mutex<Option<usize>>) {
        if let Ok(run_loop) = run_loop.lock()
            && let Some(run_loop) = *run_loop
        {
            let run_loop = run_loop as RunLoopRef;
            // SAFETY: the slot is populated only with the live run loop owned by
            // the registration thread. Holding the slot lock prevents that thread
            // from clearing the pointer and exiting while this call uses it.
            unsafe { CFRunLoopStop(run_loop) };
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
const fn encode_network_event(event: NetworkEvent) -> u8 {
    match event {
        NetworkEvent::Unavailable => 1,
        NetworkEvent::Available => 2,
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
const fn decode_network_event(value: u8) -> Option<NetworkEvent> {
    match value {
        1 => Some(NetworkEvent::Unavailable),
        2 => Some(NetworkEvent::Available),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows {
    // The callback context is an Arc kept alive until Win32 confirms
    // cancellation. Failed cancellation leaks that Arc deliberately so a late
    // operating-system callback can never dereference freed memory.
    use super::{NetworkEvent, NetworkEventInbox, NetworkEventPublisher, NetworkNotificationError};
    use std::{ffi::c_void, sync::Arc, time::Duration};
    use windows_sys::Win32::{
        Foundation::{ERROR_SUCCESS, HANDLE},
        NetworkManagement::IpHelper::{
            CancelMibChangeNotify2, NotifyNetworkConnectivityHintChange,
        },
        Networking::WinSock::NL_NETWORK_CONNECTIVITY_HINT,
    };

    const INITIAL_NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(5);

    pub(super) struct Registration {
        handle: usize,
        context: Option<Arc<NetworkEventPublisher>>,
    }

    impl Registration {
        pub(super) fn connect() -> Result<
            (
                Self,
                NetworkEventInbox,
                NetworkEvent,
                super::NativeNetworkMonitorCancellation,
            ),
            NetworkNotificationError,
        > {
            let (publisher, events, cancellation) = NetworkEventInbox::channel();
            let context = publisher;
            let mut handle: HANDLE = std::ptr::null_mut();
            // SAFETY: the Arc allocation remains valid for the notification
            // lifetime, and the callback matches Win32's documented ABI.
            let result = unsafe {
                NotifyNetworkConnectivityHintChange(
                    Some(network_callback),
                    Arc::as_ptr(&context).cast(),
                    true,
                    &raw mut handle,
                )
            };
            if result != ERROR_SUCCESS {
                return Err(NetworkNotificationError::WindowsPlatform {
                    operation: "NotifyNetworkConnectivityHintChange",
                    code: result,
                });
            }
            let registration = Self {
                handle: handle as usize,
                context: Some(context),
            };
            let current = events.recv_timeout(INITIAL_NOTIFICATION_TIMEOUT)?;
            Ok((registration, events, current, cancellation))
        }
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            // SAFETY: handle was returned by the matching notification call.
            let result = unsafe { CancelMibChangeNotify2(self.handle as _) };
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
        // SAFETY: registration pins this sender until cancellation succeeds;
        // on failure it is leaked. The callback only takes a shared reference.
        let events = unsafe { &*context.cast::<NetworkEventPublisher>() };
        events.publish(NetworkEvent::from_windows_connectivity_level(
            hint.ConnectivityLevel,
        ));
    }
}

#[cfg(any(target_os = "linux", test))]
const NETWORK_MANAGER_STATE_CONNECTED_GLOBAL: u32 = 70;

#[cfg(any(target_os = "macos", test))]
const MACOS_REACHABILITY_REACHABLE: u32 = 1 << 1;
#[cfg(any(target_os = "macos", test))]
const MACOS_REACHABILITY_CONNECTION_REQUIRED: u32 = 1 << 2;
#[cfg(any(target_os = "macos", test))]
const MACOS_REACHABILITY_CONNECTION_ON_TRAFFIC: u32 = 1 << 3;
#[cfg(any(target_os = "macos", test))]
const MACOS_REACHABILITY_INTERVENTION_REQUIRED: u32 = 1 << 4;
#[cfg(any(target_os = "macos", test))]
const MACOS_REACHABILITY_CONNECTION_ON_DEMAND: u32 = 1 << 5;

#[cfg(target_os = "linux")]
fn network_manager_state_rule() -> Result<MatchRule<'static>, zbus::Error> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender(NETWORK_MANAGER_DESTINATION)?
        .path(NETWORK_MANAGER_PATH)?
        .interface(NETWORK_MANAGER_INTERFACE)?
        .member(STATE_CHANGED_SIGNAL)?
        .build();
    Ok(rule)
}

#[cfg(target_os = "linux")]
fn network_manager_owner_rule() -> Result<MatchRule<'static>, zbus::Error> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .sender(DBUS_DESTINATION)?
        .path(DBUS_PATH)?
        .interface(DBUS_INTERFACE)?
        .member(NAME_OWNER_CHANGED_SIGNAL)?
        .add_arg(NETWORK_MANAGER_DESTINATION)?
        .build();
    Ok(rule)
}

/// Provider-relevant native network availability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkEvent {
    Unavailable,
    Available,
}

struct NetworkTransitionFilter {
    current: NetworkEvent,
}

impl NetworkTransitionFilter {
    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows", test))]
    const fn new(current: NetworkEvent) -> Self {
        Self { current }
    }

    #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows", test))]
    fn accept(&mut self, event: NetworkEvent) -> Option<NetworkEvent> {
        if event == self.current {
            return None;
        }
        self.current = event;
        Some(event)
    }
}

impl NetworkEvent {
    #[cfg(any(target_os = "linux", test))]
    const fn from_network_manager_state(state: u32) -> Self {
        if state == NETWORK_MANAGER_STATE_CONNECTED_GLOBAL {
            Self::Available
        } else {
            Self::Unavailable
        }
    }

    #[cfg(any(target_os = "windows", test))]
    const fn from_windows_connectivity_level(level: i32) -> Self {
        if level == 3 {
            Self::Available
        } else {
            Self::Unavailable
        }
    }

    #[cfg(any(target_os = "macos", test))]
    const fn from_macos_reachability_flags(flags: u32) -> Self {
        let reachable = flags & MACOS_REACHABILITY_REACHABLE != 0;
        let connection_required = flags & MACOS_REACHABILITY_CONNECTION_REQUIRED != 0;
        let can_connect_automatically = flags
            & (MACOS_REACHABILITY_CONNECTION_ON_DEMAND | MACOS_REACHABILITY_CONNECTION_ON_TRAFFIC)
            != 0;
        let intervention_required = flags & MACOS_REACHABILITY_INTERVENTION_REQUIRED != 0;
        if reachable
            && (!connection_required || (can_connect_automatically && !intervention_required))
        {
            Self::Available
        } else {
            Self::Unavailable
        }
    }
}

/// Blocking native network-event listener.
///
/// On Linux this subscribes to `NetworkManager`'s `StateChanged` signal on the
/// system bus. On macOS it subscribes to `SCNetworkReachability` for the default
/// route on a dedicated CoreFoundation run loop. On Windows it subscribes to
/// native connectivity-hint changes. Callback backends atomically coalesce bursts
/// to the latest availability.
/// Callers must run [`Self::next_event`] outside async executors and UI threads
/// because it blocks until availability changes.
pub struct NativeNetworkMonitor {
    transitions: NetworkTransitionFilter,
    cancellation: NativeNetworkMonitorCancellation,
    #[cfg(target_os = "linux")]
    messages: MessageIterator,
    #[cfg(target_os = "linux")]
    query_connection: Connection,
    #[cfg(target_os = "linux")]
    state_rule: MatchRule<'static>,
    #[cfg(target_os = "linux")]
    owner_rule: MatchRule<'static>,
    #[cfg(target_os = "windows")]
    events: NetworkEventInbox,
    #[cfg(target_os = "windows")]
    _registration: windows::Registration,
    #[cfg(target_os = "macos")]
    events: NetworkEventInbox,
    #[cfg(target_os = "macos")]
    _registration: macos::Registration,
}

/// Handle that unblocks a [`NativeNetworkMonitor`] waiting for its next event.
#[derive(Clone)]
pub struct NativeNetworkMonitorCancellation {
    cancelled: Arc<AtomicBool>,
    #[cfg(target_os = "linux")]
    messages: Connection,
    #[cfg(target_os = "linux")]
    query_connection: Connection,
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    wake: SyncSender<()>,
    #[cfg(target_os = "macos")]
    run_loop: Arc<Mutex<Option<usize>>>,
}

impl NativeNetworkMonitorCancellation {
    /// Cancels the matching monitor's blocking wait.
    pub fn cancel(self) {
        self.cancelled.store(true, Ordering::Release);
        #[cfg(target_os = "linux")]
        {
            let _ = self.messages.close();
            let _ = self.query_connection.close();
        }
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        match self.wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(()) | TrySendError::Disconnected(())) => {}
        }
        #[cfg(target_os = "macos")]
        macos::stop_run_loop(&self.run_loop);
    }
}

#[cfg(target_os = "linux")]
fn dbus_proxy(connection: &Connection) -> Result<Proxy<'_>, zbus::Error> {
    Proxy::new(connection, DBUS_DESTINATION, DBUS_PATH, DBUS_INTERFACE)
}

#[cfg(target_os = "linux")]
fn install_match(connection: &Connection, rule: &MatchRule<'_>) -> Result<(), zbus::Error> {
    dbus_proxy(connection)?.call("AddMatch", &(rule.to_string(),))
}

#[cfg(target_os = "linux")]
fn network_manager_has_owner(connection: &Connection) -> Result<bool, zbus::Error> {
    dbus_proxy(connection)?.call("NameHasOwner", &(NETWORK_MANAGER_DESTINATION,))
}

#[cfg(target_os = "linux")]
fn network_manager_owner(
    connection: &Connection,
) -> Result<Option<String>, NetworkNotificationError> {
    for _ in 0..MAX_OWNER_RECONCILIATION_ATTEMPTS {
        if !network_manager_has_owner(connection).map_err(NetworkNotificationError::Platform)? {
            return Ok(None);
        }
        match dbus_proxy(connection)
            .map_err(NetworkNotificationError::Platform)?
            .call("GetNameOwner", &(NETWORK_MANAGER_DESTINATION,))
        {
            Ok(owner) => return Ok(Some(owner)),
            Err(_) => {
                if !network_manager_has_owner(connection)
                    .map_err(NetworkNotificationError::Platform)?
                {
                    return Ok(None);
                }
            }
        }
    }
    Err(NetworkNotificationError::OwnerChangedRepeatedly)
}

#[cfg(target_os = "linux")]
fn read_network_event(connection: &Connection) -> Result<NetworkEvent, NetworkNotificationError> {
    for _ in 0..MAX_OWNER_RECONCILIATION_ATTEMPTS {
        let Some(owner_before) = network_manager_owner(connection)? else {
            return Ok(NetworkEvent::Unavailable);
        };
        let proxy = Proxy::new(
            connection,
            NETWORK_MANAGER_DESTINATION,
            NETWORK_MANAGER_PATH,
            NETWORK_MANAGER_INTERFACE,
        )
        .map_err(NetworkNotificationError::Platform)?;
        let state = proxy.get_property::<u32>(STATE_PROPERTY);
        let owner_after = network_manager_owner(connection)?;
        if owner_after.as_ref() != Some(&owner_before) {
            continue;
        }
        return state
            .map(NetworkEvent::from_network_manager_state)
            .map_err(NetworkNotificationError::Platform);
    }
    Err(NetworkNotificationError::OwnerChangedRepeatedly)
}

impl NativeNetworkMonitor {
    /// Reports whether this crate implements a native network-event source.
    #[must_use]
    pub const fn availability() -> CapabilityAvailability {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        return CapabilityAvailability::Available;

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        CapabilityAvailability::Unavailable
    }

    /// Connects to the native source and reads its current availability.
    ///
    /// Native notifications are registered before the initial state read so a
    /// transition cannot be silently lost during construction.
    ///
    /// # Errors
    ///
    /// Returns an error when the target is unsupported or a native subscription
    /// or state read fails. An absent Linux `NetworkManager` owner is represented
    /// as [`NetworkEvent::Unavailable`].
    pub fn connect() -> Result<Self, NetworkNotificationError> {
        #[cfg(target_os = "linux")]
        {
            let connection = connection::Builder::system()
                .map_err(NetworkNotificationError::Platform)?
                .max_queued(MAX_QUEUED_NETWORK_EVENTS)
                .build()
                .map_err(NetworkNotificationError::Platform)?;
            let messages = MessageIterator::from(&connection);
            let state_rule =
                network_manager_state_rule().map_err(NetworkNotificationError::Platform)?;
            let owner_rule =
                network_manager_owner_rule().map_err(NetworkNotificationError::Platform)?;
            install_match(&connection, &state_rule).map_err(NetworkNotificationError::Platform)?;
            install_match(&connection, &owner_rule).map_err(NetworkNotificationError::Platform)?;
            let query_connection =
                Connection::system().map_err(NetworkNotificationError::Platform)?;
            let current = read_network_event(&query_connection)?;
            let cancelled = Arc::new(AtomicBool::new(false));
            Ok(Self {
                transitions: NetworkTransitionFilter::new(current),
                cancellation: NativeNetworkMonitorCancellation {
                    cancelled,
                    messages: Connection::from(&messages),
                    query_connection: query_connection.clone(),
                },
                messages,
                query_connection,
                state_rule,
                owner_rule,
            })
        }

        #[cfg(target_os = "windows")]
        {
            let (registration, events, current, cancellation) = windows::Registration::connect()?;
            Ok(Self {
                transitions: NetworkTransitionFilter::new(current),
                cancellation,
                events,
                _registration: registration,
            })
        }

        #[cfg(target_os = "macos")]
        {
            let (registration, events, current, cancellation) = macos::Registration::connect()?;
            Ok(Self {
                transitions: NetworkTransitionFilter::new(current),
                cancellation,
                events,
                _registration: registration,
            })
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        Err(NetworkNotificationError::UnsupportedPlatform)
    }

    /// Returns the availability observed during construction or the last event.
    #[must_use]
    pub const fn current(&self) -> NetworkEvent {
        self.transitions.current
    }

    /// Returns a handle that can unblock [`Self::next_event`] from another thread.
    #[must_use]
    pub fn cancellation(&self) -> NativeNetworkMonitorCancellation {
        self.cancellation.clone()
    }

    /// Blocks until provider-relevant availability changes.
    ///
    /// On Linux each matching signal triggers a fresh owner/property read. All
    /// native backends suppress duplicate provider-availability states.
    ///
    /// # Errors
    ///
    /// Returns an error when the native stream closes or a native state operation
    /// fails.
    pub fn next_event(&mut self) -> Result<NetworkEvent, NetworkNotificationError> {
        #[cfg(target_os = "linux")]
        loop {
            if self.cancellation.cancelled.load(Ordering::Acquire) {
                return Err(NetworkNotificationError::Cancelled);
            }
            let message = self.messages.next();
            if self.cancellation.cancelled.load(Ordering::Acquire) {
                return Err(NetworkNotificationError::Cancelled);
            }
            let message = message
                .ok_or(NetworkNotificationError::StreamClosed)?
                .map_err(NetworkNotificationError::Platform)?;
            if !self
                .state_rule
                .matches(&message)
                .map_err(NetworkNotificationError::Platform)?
                && !self
                    .owner_rule
                    .matches(&message)
                    .map_err(NetworkNotificationError::Platform)?
            {
                continue;
            }
            let current = read_network_event(&self.query_connection)?;
            if self.cancellation.cancelled.load(Ordering::Acquire) {
                return Err(NetworkNotificationError::Cancelled);
            }
            if let Some(event) = self.transitions.accept(current) {
                return Ok(event);
            }
        }

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        loop {
            let current = self.events.recv()?;
            if let Some(event) = self.transitions.accept(current) {
                return Ok(event);
            }
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        Err(NetworkNotificationError::UnsupportedPlatform)
    }
}

impl fmt::Debug for NativeNetworkMonitor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeNetworkMonitor")
            .field("availability", &Self::availability())
            .field("current", &self.transitions.current)
            .finish_non_exhaustive()
    }
}

/// Errors returned by native network notification monitoring.
#[derive(Debug)]
pub enum NetworkNotificationError {
    #[cfg(target_os = "linux")]
    Platform(zbus::Error),
    #[cfg(target_os = "windows")]
    WindowsPlatform {
        operation: &'static str,
        code: u32,
    },
    #[cfg(target_os = "windows")]
    InitialNotificationTimedOut,
    #[cfg(target_os = "macos")]
    MacOsPlatform {
        operation: &'static str,
    },
    OwnerChangedRepeatedly,
    Cancelled,
    StreamClosed,
    UnsupportedPlatform,
}

impl fmt::Display for NetworkNotificationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(target_os = "linux")]
            Self::Platform(error) => {
                write!(formatter, "native network notification failed: {error}")
            }
            #[cfg(target_os = "windows")]
            Self::WindowsPlatform { operation, code } => write!(
                formatter,
                "native network notification operation {operation} failed with Windows error {code}"
            ),
            #[cfg(target_os = "windows")]
            Self::InitialNotificationTimedOut => formatter.write_str(
                "native network notification did not deliver its initial Windows state in time",
            ),
            #[cfg(target_os = "macos")]
            Self::MacOsPlatform { operation } => write!(
                formatter,
                "native network notification operation {operation} failed on macOS"
            ),
            Self::OwnerChangedRepeatedly => formatter
                .write_str("native network notification owner changed repeatedly during sampling"),
            Self::Cancelled => formatter.write_str("native network notification wait cancelled"),
            Self::StreamClosed => formatter.write_str("native network notification stream closed"),
            Self::UnsupportedPlatform => {
                formatter.write_str("native network notifications are unavailable on this platform")
            }
        }
    }
}

impl Error for NetworkNotificationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Platform(error) => Some(error),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "windows")]
    use super::NetworkEventInbox;
    use super::{NativeNetworkMonitor, NetworkEvent, NetworkTransitionFilter};
    use crate::CapabilityAvailability;

    #[test]
    fn only_global_network_manager_state_is_provider_available() {
        for state in [0, 10, 20, 30, 40, 50, 60, 80, u32::MAX] {
            assert_eq!(
                NetworkEvent::from_network_manager_state(state),
                NetworkEvent::Unavailable
            );
        }
        assert_eq!(
            NetworkEvent::from_network_manager_state(70),
            NetworkEvent::Available
        );
    }

    #[test]
    fn repeated_and_intermediate_states_emit_only_availability_changes() {
        let mut transitions = NetworkTransitionFilter::new(NetworkEvent::Unavailable);
        for state in [0, 40, 50, 60] {
            assert_eq!(
                transitions.accept(NetworkEvent::from_network_manager_state(state)),
                None
            );
        }
        assert_eq!(
            transitions.accept(NetworkEvent::from_network_manager_state(70)),
            Some(NetworkEvent::Available)
        );
        assert_eq!(
            transitions.accept(NetworkEvent::from_network_manager_state(70)),
            None
        );
        assert_eq!(
            transitions.accept(NetworkEvent::from_network_manager_state(60)),
            Some(NetworkEvent::Unavailable)
        );
        assert_eq!(
            transitions.accept(NetworkEvent::from_network_manager_state(20)),
            None
        );
    }

    #[test]
    fn only_windows_internet_access_is_provider_available() {
        for level in [0, 1, 2, 4, 5, i32::MAX] {
            assert_eq!(
                NetworkEvent::from_windows_connectivity_level(level),
                NetworkEvent::Unavailable
            );
        }
        assert_eq!(
            NetworkEvent::from_windows_connectivity_level(3),
            NetworkEvent::Available
        );
    }

    #[test]
    fn macos_reachability_requires_a_usable_route() {
        use super::{
            MACOS_REACHABILITY_CONNECTION_ON_DEMAND, MACOS_REACHABILITY_CONNECTION_ON_TRAFFIC,
            MACOS_REACHABILITY_CONNECTION_REQUIRED, MACOS_REACHABILITY_INTERVENTION_REQUIRED,
            MACOS_REACHABILITY_REACHABLE,
        };

        assert_eq!(
            NetworkEvent::from_macos_reachability_flags(0),
            NetworkEvent::Unavailable
        );
        assert_eq!(
            NetworkEvent::from_macos_reachability_flags(MACOS_REACHABILITY_REACHABLE),
            NetworkEvent::Available
        );
        assert_eq!(
            NetworkEvent::from_macos_reachability_flags(
                MACOS_REACHABILITY_REACHABLE | MACOS_REACHABILITY_CONNECTION_REQUIRED
            ),
            NetworkEvent::Unavailable
        );
        for automatic in [
            MACOS_REACHABILITY_CONNECTION_ON_DEMAND,
            MACOS_REACHABILITY_CONNECTION_ON_TRAFFIC,
        ] {
            assert_eq!(
                NetworkEvent::from_macos_reachability_flags(
                    MACOS_REACHABILITY_REACHABLE
                        | MACOS_REACHABILITY_CONNECTION_REQUIRED
                        | automatic
                ),
                NetworkEvent::Available
            );
            assert_eq!(
                NetworkEvent::from_macos_reachability_flags(
                    MACOS_REACHABILITY_REACHABLE
                        | MACOS_REACHABILITY_CONNECTION_REQUIRED
                        | automatic
                        | MACOS_REACHABILITY_INTERVENTION_REQUIRED
                ),
                NetworkEvent::Unavailable
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_mailbox_retains_the_latest_state_under_callback_bursts() {
        let (publisher, events, _cancellation) = NetworkEventInbox::channel();
        for _ in 0..64 {
            publisher.publish(NetworkEvent::Available);
            publisher.publish(NetworkEvent::Unavailable);
        }
        publisher.publish(NetworkEvent::Available);
        assert_eq!(
            events.recv().expect("latest network state is retained"),
            NetworkEvent::Available
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_match_rule_is_specific_to_network_manager_state_changes() {
        let state_rule = super::network_manager_state_rule().expect("static match rule is valid");
        assert_eq!(
            state_rule.to_string(),
            "type='signal',sender='org.freedesktop.NetworkManager',interface='org.freedesktop.NetworkManager',member='StateChanged',path='/org/freedesktop/NetworkManager'"
        );
        let owner_rule = super::network_manager_owner_rule().expect("static match rule is valid");
        assert_eq!(
            owner_rule.to_string(),
            "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',member='NameOwnerChanged',path='/org/freedesktop/DBus',arg0='org.freedesktop.NetworkManager'"
        );
    }

    #[test]
    fn availability_matches_the_implemented_native_backend() {
        #[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
        assert_eq!(
            NativeNetworkMonitor::availability(),
            CapabilityAvailability::Available
        );
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        assert_eq!(
            NativeNetworkMonitor::availability(),
            CapabilityAvailability::Unavailable
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_network_monitor_receives_bounded_initial_state() {
        let monitor = NativeNetworkMonitor::connect()
            .expect("Windows network callback provides its initial state");
        assert!(matches!(
            monitor.current(),
            NetworkEvent::Available | NetworkEvent::Unavailable
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_network_monitor_cancellation_unblocks_a_waiter() {
        let mut monitor = NativeNetworkMonitor::connect()
            .expect("Windows network callback provides its initial state");
        let cancellation = monitor.cancellation();
        let waiter = std::thread::spawn(move || monitor.next_event());
        cancellation.cancel();
        assert!(matches!(
            waiter.join().expect("join network monitor waiter"),
            Err(super::NetworkNotificationError::Cancelled)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_network_monitor_reads_initial_state() {
        let monitor = NativeNetworkMonitor::connect()
            .expect("macOS reachability monitor provides its initial state");
        assert!(matches!(
            monitor.current(),
            NetworkEvent::Available | NetworkEvent::Unavailable
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_network_monitor_cancellation_unblocks_a_waiter() {
        let mut monitor = NativeNetworkMonitor::connect()
            .expect("macOS reachability monitor provides its initial state");
        let cancellation = monitor.cancellation();
        let waiter = std::thread::spawn(move || monitor.next_event());
        cancellation.cancel();
        assert!(matches!(
            waiter.join().expect("join network monitor waiter"),
            Err(super::NetworkNotificationError::Cancelled)
        ));
    }
}
