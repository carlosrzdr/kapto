// SPDX-License-Identifier: Apache-2.0

//! What every transport must do, checked the same way for each.

#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test loudly when the setup itself breaks"
)]

use std::time::Duration;

use kapto_capture::Batch;
use kapto_transport::Transport;

/// Long enough that a message already sent over loopback has always arrived.
pub(crate) const WAIT: Duration = Duration::from_secs(5);

/// Sends one message to the transport under test.
pub(crate) type Sender = Box<dyn Fn(&[u8])>;

/// Receives into `batch` until it holds `count` messages, or a receive
/// gets nothing to keep.
pub(crate) fn receive_until<T: Transport>(transport: &mut T, batch: &mut Batch, count: usize) {
    while batch.len() < count && transport.receive(batch, WAIT).unwrap() > 0 {}
}

/// What every transport must do, given a way to make one that receives
/// whatever `send` sends, as stream 0.
pub(crate) fn conformance<T: Transport>(make: impl Fn() -> (T, Sender)) {
    // Nothing to receive: zero after the timeout, batch untouched.
    let (mut transport, _) = make();
    let mut batch = Batch::new(4, 64);
    assert_eq!(
        transport
            .receive(&mut batch, Duration::from_millis(10))
            .unwrap(),
        0
    );
    assert!(batch.is_empty());

    // Messages arrive in order, sharing one receive time.
    let (mut transport, send) = make();
    send(b"one");
    send(b"two");
    receive_until(&mut transport, &mut batch, 2);
    let messages: Vec<_> = batch.messages().collect();
    assert_eq!(messages, [(0, &b"one"[..]), (0, &b"two"[..])]);
    assert!(batch.recv_time().as_nanos() > 0);

    // A full batch takes nothing; the message waits for the next one.
    let mut full = Batch::new(1, 64);
    full.push(0, b"old");
    send(b"waiting");
    assert_eq!(transport.receive(&mut full, WAIT).unwrap(), 0);
    full.clear();
    assert_eq!(transport.receive(&mut full, WAIT).unwrap(), 1);
    assert_eq!(full.messages().next(), Some((0, &b"waiting"[..])));
}
