//! Quarantined unsafe `AF_XDP` socket, `UMEM`, and ring calls.
//!
//! Every unsafe call into the native dependency lives here. The safe driver above
//! owns descriptor provenance and retains user ownership until submission.

#![allow(unsafe_code)]

use crate::config::{
    AXIUSFLOW_EXPERIMENTAL_ETHERTYPE, AfXdpConfig, ETHERNET_HEADER_BYTES, NATIVE_UMEM_FRAME_BYTES,
    RECEIVE_POLL_TIMEOUT_MILLIS,
};
use axiusflow_transport::QueueBinding;
use std::{error::Error as _, num::NonZeroU32};
use xsk_rs::{
    CompQueue, FillQueue, FrameDesc, RxQueue, TxQueue, Umem,
    config::{BindFlags, FrameSize, Interface, QueueSize, SocketConfig, UmemConfig, XdpFlags},
    socket::Socket,
};

#[derive(Debug)]
pub(super) struct ReceiveOutcome {
    pub(super) dropped_frames: u64,
    pub(super) dropped_bytes: u64,
}

#[derive(Debug)]
pub(super) struct NativeOpenError {
    message: String,
    queue_busy: bool,
}

impl NativeOpenError {
    pub(super) fn queue_busy(&self) -> bool {
        self.queue_busy
    }

    fn other(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            queue_busy: false,
        }
    }

    fn socket(error: &xsk_rs::socket::SocketCreateError) -> Self {
        const LINUX_EBUSY: i32 = 16;
        Self {
            queue_busy: error
                .source()
                .and_then(|source| source.downcast_ref::<std::io::Error>())
                .is_some_and(|error| error.raw_os_error() == Some(LINUX_EBUSY)),
            message: match error.source() {
                Some(source) => format!("{error}: {source}"),
                None => error.to_string(),
            },
        }
    }
}

impl std::fmt::Display for NativeOpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Debug)]
/// Field declaration order is drop order, and it is load-bearing here.
///
/// Every queue below holds an `Arc<Mutex<SocketInner>>` reference to the `AF_XDP`
/// socket. `xsk_umem__delete` fails while any socket still references the UMEM, so
/// dropping `umem` first leaves both the UMEM and the socket file descriptor alive.
/// The kernel then keeps the queue binding, and the next open on the same
/// `(interface, queue_id)` pair fails with `EBUSY`. `umem` therefore drops last.
pub(super) struct CopySocket {
    _tx: TxQueue,
    rx: RxQueue,
    fill: FillQueue,
    _completion: CompQueue,
    scratch: Vec<FrameDesc>,
    in_flight: Vec<FrameDesc>,
    pending_recycle: Vec<FrameDesc>,
    umem: Umem,
}

impl CopySocket {
    pub(super) fn open(
        config: &AfXdpConfig,
        binding: QueueBinding,
    ) -> Result<Self, NativeOpenError> {
        let frame_count = NonZeroU32::new(
            u32::try_from(config.frame_count.get())
                .map_err(|_| NativeOpenError::other("frame count does not fit the native API"))?,
        )
        .ok_or_else(|| NativeOpenError::other("frame count cannot be zero"))?;
        let ring_size = QueueSize::new(frame_count.get())
            .map_err(|error| NativeOpenError::other(error.to_string()))?;
        let frame_size = FrameSize::new(u32::try_from(NATIVE_UMEM_FRAME_BYTES).map_err(|_| {
            NativeOpenError::other("fixed UMEM frame size does not fit the native API")
        })?)
        .map_err(|error| NativeOpenError::other(error.to_string()))?;
        let umem_config = UmemConfig::builder()
            .frame_size(frame_size)
            .fill_queue_size(ring_size)
            .comp_queue_size(ring_size)
            .build()
            .map_err(|error| NativeOpenError::other(error.to_string()))?;
        if binding.maximum_frame_bytes.get()
            > usize::try_from(umem_config.mtu()).unwrap_or(usize::MAX)
        {
            return Err(NativeOpenError::other(
                "configured packet limit exceeds the fixed UMEM MTU",
            ));
        }
        let (umem, descriptors) = Umem::new(umem_config, frame_count, false)
            .map_err(|error| NativeOpenError::other(error.to_string()))?;
        let socket_config = SocketConfig::builder()
            .rx_queue_size(ring_size)
            .tx_queue_size(ring_size)
            .xdp_flags(XdpFlags::XDP_FLAGS_SKB_MODE)
            .bind_flags(BindFlags::XDP_COPY)
            .build();
        let interface: Interface = config
            .interface_name
            .parse()
            .map_err(|error: std::ffi::NulError| NativeOpenError::other(error.to_string()))?;
        // SAFETY: `umem` is newly allocated, is not shared with another socket, and
        // remains owned by this `CopySocket` for longer than every returned queue.
        let (tx, rx, queues) =
            unsafe { Socket::new(socket_config, &umem, &interface, u32::from(config.queue_id)) }
                .map_err(|error| NativeOpenError::socket(&error))?;
        let (mut fill, completion) = queues.ok_or_else(|| {
            NativeOpenError::other("new non-shared UMEM did not return fill/completion rings")
        })?;
        // SAFETY: every descriptor was created by this exact `umem`, descriptors are
        // unique, and userspace has not submitted or otherwise aliased any of them.
        let submitted = unsafe { fill.produce(&descriptors) };
        if submitted != descriptors.len() {
            return Err(NativeOpenError::other(format!(
                "fill ring accepted {submitted} of {} initial descriptors",
                descriptors.len()
            )));
        }
        Ok(Self {
            _tx: tx,
            rx,
            fill,
            _completion: completion,
            scratch: vec![FrameDesc::default(); binding.maximum_batch_items.get()],
            in_flight: Vec::with_capacity(binding.maximum_batch_items.get()),
            pending_recycle: Vec::with_capacity(config.frame_count.get()),
            umem,
        })
    }

    pub(super) fn receive(
        &mut self,
        maximum_batch_items: usize,
        maximum_frame_bytes: usize,
    ) -> Result<ReceiveOutcome, String> {
        if !self.in_flight.is_empty() {
            return Err("receive attempted while descriptors remain in flight".to_string());
        }
        self.flush_recycle()?;
        let scratch = self
            .scratch
            .get_mut(..maximum_batch_items)
            .ok_or_else(|| "batch limit exceeds fixed receive storage".to_string())?;
        // SAFETY: `scratch` is only descriptor output storage. `rx` and every
        // descriptor it returns are tied to this `umem`; consumed descriptors are
        // moved to either `in_flight` or `pending_recycle` before another poll.
        let received = unsafe {
            self.rx
                .poll_and_consume(scratch, RECEIVE_POLL_TIMEOUT_MILLIS)
        }
        .map_err(|error| error.to_string())?;
        let mut dropped_frames = 0_u64;
        let mut dropped_bytes = 0_u64;
        for descriptor in scratch.iter().copied().take(received) {
            // SAFETY: this descriptor was just consumed from this socket's RX
            // ring and has not been returned to the fill ring. The immutable
            // view is dropped before descriptor ownership is moved below.
            let packet = unsafe { self.umem.data(&descriptor) }.contents();
            let payload_length = packet
                .get(12..14)
                .filter(|ether_type| **ether_type == AXIUSFLOW_EXPERIMENTAL_ETHERTYPE)
                .and_then(|_| packet.len().checked_sub(ETHERNET_HEADER_BYTES));
            if payload_length.is_none_or(|length| length > maximum_frame_bytes) {
                dropped_frames = dropped_frames.saturating_add(1);
                dropped_bytes =
                    dropped_bytes.saturating_add(u64::try_from(packet.len()).unwrap_or(u64::MAX));
                self.pending_recycle.push(descriptor);
            } else {
                self.in_flight.push(descriptor);
            }
        }
        if let Err(error) = self.flush_recycle() {
            self.pending_recycle.append(&mut self.in_flight);
            return Err(error);
        }
        Ok(ReceiveOutcome {
            dropped_frames,
            dropped_bytes,
        })
    }

    pub(super) fn batch_len(&self) -> usize {
        self.in_flight.len()
    }

    pub(super) fn owned_len(&self) -> usize {
        self.in_flight
            .len()
            .saturating_add(self.pending_recycle.len())
    }

    pub(super) fn frame_bytes(&self, index: usize) -> Option<&[u8]> {
        let descriptor = self.in_flight.get(index)?;
        // SAFETY: the descriptor originated from this socket's RX ring and remains
        // in `in_flight`, so userspace exclusively owns it and it has not been
        // resubmitted to the fill ring. Only an immutable view is returned.
        let packet = unsafe { self.umem.data(descriptor) }.contents();
        if packet.get(12..14)? != AXIUSFLOW_EXPERIMENTAL_ETHERTYPE {
            return None;
        }
        packet.get(ETHERNET_HEADER_BYTES..)
    }

    pub(super) fn recycle_batch(&mut self) -> Result<(), String> {
        self.pending_recycle.append(&mut self.in_flight);
        self.flush_recycle()
    }

    pub(super) fn flush_recycle(&mut self) -> Result<(), String> {
        if self.pending_recycle.is_empty() {
            return Ok(());
        }
        // SAFETY: every pending descriptor originated from this socket's RX ring,
        // belongs to this `umem`, is uniquely held by `pending_recycle`, and has not
        // been submitted since userspace regained ownership from RX.
        let submitted = unsafe { self.fill.produce(&self.pending_recycle) };
        if submitted > 0 {
            self.pending_recycle.drain(..submitted);
        }
        if self.pending_recycle.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "fill ring retained {} descriptors after recycle",
                self.pending_recycle.len()
            ))
        }
    }
}
