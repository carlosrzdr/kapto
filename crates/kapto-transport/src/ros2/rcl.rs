// SPDX-License-Identifier: Apache-2.0

//! Subscribing to ROS 2 topics and taking their messages through rcl's C
//! functions, as bound by `r2r_rcl`: everything here is a call into C.
//!
//! The node is Kapto's own, not `r2r`'s, whose handles aren't reachable from
//! outside it. A message type's support library is loaded by its name when
//! the topic is subscribed to, as rosbag2 does, so any type installed where
//! Kapto runs can be subscribed to, the robot's own included, without
//! generating code for it. Each message is taken still serialised into a
//! buffer kept per subscription, which the middleware grows only for a
//! message larger than any before.

#![allow(
    unsafe_code,
    reason = "rcl, and loading a message type's support library, are C functions"
)]

use std::ffi::{CStr, CString, c_char, c_void};
use std::ptr;
use std::time::Duration;

use kapto_capture::Publication;
use kapto_core::{ErrorCode, KaptoError};
use r2r_rcl::{
    RCL_RET_OK, RCL_RET_SUBSCRIPTION_TAKE_FAILED, RCL_RET_TIMEOUT, rcl_context_t,
    rcl_get_zero_initialized_context, rcl_get_zero_initialized_init_options,
    rcl_get_zero_initialized_node, rcl_get_zero_initialized_subscription,
    rcl_get_zero_initialized_wait_set, rcl_init, rcl_init_options_fini, rcl_init_options_init,
    rcl_node_get_default_options, rcl_node_init, rcl_node_t, rcl_ret_t, rcl_serialized_message_t,
    rcl_subscription_get_default_options, rcl_subscription_init, rcl_subscription_t,
    rcl_take_serialized_message, rcl_wait, rcl_wait_set_add_subscription, rcl_wait_set_clear,
    rcl_wait_set_init, rcl_wait_set_t, rcutils_get_default_allocator, rcutils_get_error_string,
    rcutils_get_zero_initialized_uint8_array, rcutils_reset_error, rcutils_uint8_array_init,
    rmw_message_info_t, rmw_qos_durability_policy_e, rmw_qos_history_policy_e,
    rmw_qos_reliability_policy_e, rosidl_message_type_support_t,
};

use super::publishers::Publishers;
use super::{Durability, Qos, Reliability};

/// A topic to subscribe to, of a message type.
pub(super) struct Wanted<'a> {
    pub(super) topic: &'a str,
    pub(super) message_type: &'a str,
    pub(super) qos: Qos,
}

/// A node of Kapto's own with its subscriptions, a buffer for each, and the
/// wait set that waits on all of them. The context, node and wait set are
/// boxed, as rcl keeps pointers to them; the subscriptions' vector has room
/// for all of them from the start, so it never moves them either.
pub(super) struct Node {
    context: Box<rcl_context_t>,
    handle: Box<rcl_node_t>,
    subscriptions: Vec<rcl_subscription_t>,
    /// Each subscription's publishers, as their messages say who sent them.
    publishers: Vec<Publishers>,
    /// Each subscription's last message, still serialised.
    buffers: Vec<rcl_serialized_message_t>,
    wait_set: Box<rcl_wait_set_t>,
    /// Filled by each take: the publisher's timestamp, among others.
    info: rmw_message_info_t,
}

// SAFETY: rcl's handles may be used from any thread as long as no two use
// the same one at once, which `&mut self` on every call ensures; nothing in
// them is tied to the thread that created them.
unsafe impl Send for Node {}

impl Node {
    /// Starts a node named `name` and subscribes to each of `wanted`, in
    /// order: the subscriptions' indices are their positions.
    pub(super) fn open(name: &str, wanted: &[Wanted<'_>]) -> Result<Self, KaptoError> {
        let name = c_string(name, ErrorCode::TransportRos2NodeFailed)?;
        let mut node = Self {
            // SAFETY: each returns a plain zeroed struct, which is what the
            // matching `_init` function expects, and `_fini` accepts.
            context: Box::new(unsafe { rcl_get_zero_initialized_context() }),
            // SAFETY: as above.
            handle: Box::new(unsafe { rcl_get_zero_initialized_node() }),
            subscriptions: Vec::with_capacity(wanted.len()),
            publishers: Vec::with_capacity(wanted.len()),
            buffers: Vec::with_capacity(wanted.len()),
            // SAFETY: as above.
            wait_set: Box::new(unsafe { rcl_get_zero_initialized_wait_set() }),
            info: rmw_message_info_t::default(),
        };
        let failed = ErrorCode::TransportRos2NodeFailed;
        // SAFETY: the options are zero-initialised before `_init` and
        // finalised once `rcl_init` has copied them; the context is zeroed
        // and boxed, so it stays where rcl was told it is. No arguments are
        // passed (argc 0, argv null), which rcl allows.
        let started = unsafe {
            let mut options = rcl_get_zero_initialized_init_options();
            let mut ret = rcl_init_options_init(&raw mut options, rcutils_get_default_allocator());
            if ret == ok() {
                ret = rcl_init(0, ptr::null(), &raw const options, &raw mut *node.context);
                rcl_init_options_fini(&raw mut options);
            }
            ret
        };
        check(started, failed, "ROS 2 can't be initialised")?;
        // SAFETY: the node is zeroed and boxed, the context initialised just
        // above, and the name and namespace are C strings that outlive the
        // call (rcl copies them).
        let created = unsafe {
            let options = rcl_node_get_default_options();
            rcl_node_init(
                &raw mut *node.handle,
                name.as_ptr(),
                c"".as_ptr(),
                &raw mut *node.context,
                &raw const options,
            )
        };
        check(created, failed, "Kapto's node can't start")?;
        for wanted in wanted {
            let subscription = node.subscribe(wanted)?;
            node.subscriptions.push(subscription);
            node.publishers.push(Publishers::default());
            // SAFETY: a zeroed array, as `_init` expects.
            let mut buffer = unsafe { rcutils_get_zero_initialized_uint8_array() };
            // SAFETY: the buffer starts empty; the middleware grows it with
            // the same allocator when a message needs more.
            let made = unsafe {
                rcutils_uint8_array_init(&raw mut buffer, 0, &rcutils_get_default_allocator())
            };
            check(made, failed, "no buffer for the messages")?;
            node.buffers.push(buffer);
        }
        // SAFETY: the wait set is zeroed and boxed; it is sized for every
        // subscription and nothing else, with the initialised context.
        let ready = unsafe {
            rcl_wait_set_init(
                &raw mut *node.wait_set,
                node.subscriptions.len(),
                0,
                0,
                0,
                0,
                0,
                &raw mut *node.context,
                rcutils_get_default_allocator(),
            )
        };
        check(ready, failed, "the subscriptions can't be waited on")?;
        Ok(node)
    }

    fn subscribe(&mut self, wanted: &Wanted<'_>) -> Result<rcl_subscription_t, KaptoError> {
        let failed = ErrorCode::TransportRos2SubscribeFailed;
        let type_support = type_support(wanted.message_type)?;
        let topic = c_string(wanted.topic, failed)?;
        // SAFETY: a zeroed struct, as `_init` expects.
        let mut subscription = unsafe { rcl_get_zero_initialized_subscription() };
        // SAFETY: the default options are complete, and only the QoS fields
        // Kapto sets are changed; the node is initialised, the type support
        // comes from its library, which stays loaded, and the topic is a C
        // string rcl copies.
        let subscribed = unsafe {
            let mut options = rcl_subscription_get_default_options();
            options.qos.history = rmw_qos_history_policy_e::RMW_QOS_POLICY_HISTORY_KEEP_LAST;
            options.qos.depth = wanted.qos.depth;
            options.qos.reliability = match wanted.qos.reliability {
                Reliability::BestEffort => {
                    rmw_qos_reliability_policy_e::RMW_QOS_POLICY_RELIABILITY_BEST_EFFORT
                }
                Reliability::Reliable => {
                    rmw_qos_reliability_policy_e::RMW_QOS_POLICY_RELIABILITY_RELIABLE
                }
            };
            options.qos.durability = match wanted.qos.durability {
                Durability::Volatile => {
                    rmw_qos_durability_policy_e::RMW_QOS_POLICY_DURABILITY_VOLATILE
                }
                Durability::TransientLocal => {
                    rmw_qos_durability_policy_e::RMW_QOS_POLICY_DURABILITY_TRANSIENT_LOCAL
                }
            };
            rcl_subscription_init(
                &raw mut subscription,
                &raw const *self.handle,
                type_support,
                topic.as_ptr(),
                &raw const options,
            )
        };
        check(subscribed, failed, "can't subscribe")
            .map_err(|error| error.context("topic", wanted.topic))?;
        Ok(subscription)
    }

    /// Waits up to `timeout` for a message on any subscription. Returns
    /// whether one has any; [`Node::is_ready`] says which.
    pub(super) fn wait(&mut self, timeout: Duration) -> Result<bool, KaptoError> {
        let failed = ErrorCode::TransportRos2ReceiveFailed;
        let timeout = i64::try_from(timeout.as_nanos()).unwrap_or(i64::MAX);
        // SAFETY: the wait set was initialised for exactly this many
        // subscriptions, each initialised; it is cleared first, so adding
        // them again never overflows it.
        let waited = unsafe {
            let mut ret = rcl_wait_set_clear(&raw mut *self.wait_set);
            for subscription in &self.subscriptions {
                if ret == ok() {
                    ret = rcl_wait_set_add_subscription(
                        &raw mut *self.wait_set,
                        subscription,
                        ptr::null_mut(),
                    );
                }
            }
            if ret == ok() {
                ret = rcl_wait(&raw mut *self.wait_set, timeout);
            }
            ret
        };
        if waited == code(RCL_RET_TIMEOUT) {
            return Ok(false);
        }
        check(waited, failed, "can't wait for messages").map(|()| true)
    }

    /// Whether subscription `index` had a message after the last wait.
    pub(super) fn is_ready(&self, index: usize) -> bool {
        if index >= self.wait_set.size_of_subscriptions {
            return false;
        }
        // SAFETY: rcl sized this array to `size_of_subscriptions`, checked
        // just above, and after a wait leaves an entry non-null only for a
        // subscription that has a message.
        unsafe { !(*self.wait_set.subscriptions.add(index)).is_null() }
    }

    /// Takes subscription `index`'s next message; `None` once it has no
    /// more. The bytes are only valid until the next take.
    pub(super) fn take(&mut self, index: usize) -> Result<Option<Taken<'_>>, KaptoError> {
        let (Some(subscription), Some(buffer), Some(publishers)) = (
            self.subscriptions.get(index),
            self.buffers.get_mut(index),
            self.publishers.get_mut(index),
        ) else {
            return Ok(None);
        };
        // SAFETY: the subscription and its buffer were initialised when it
        // was made; the buffer is grown by the middleware with the allocator
        // it was made with, and the message info is a plain struct to fill.
        let taken = unsafe {
            rcl_take_serialized_message(
                subscription,
                &raw mut *buffer,
                &raw mut self.info,
                ptr::null_mut(),
            )
        };
        if taken == code(RCL_RET_SUBSCRIPTION_TAKE_FAILED) {
            return Ok(None);
        }
        check(
            taken,
            ErrorCode::TransportRos2ReceiveFailed,
            "can't take a message",
        )?;
        let published = u64::try_from(self.info.source_timestamp).unwrap_or(0);
        // A middleware without the publisher's count gives the largest
        // number (or none, 0) instead.
        let number = self.info.publication_sequence_number;
        let publication = (number != 0 && number != u64::MAX)
            .then(|| publishers.index(&self.info.publisher_gid.data))
            .flatten()
            .map(|publisher| Publication { publisher, number });
        if buffer.buffer.is_null() || buffer.buffer_length == 0 {
            return Ok(Some(Taken {
                message: &[],
                published,
                publication,
            }));
        }
        // SAFETY: after a successful take the buffer holds `buffer_length`
        // bytes of the message, and is only changed by the next take, which
        // needs `&mut self` and so ends this borrow first.
        let message = unsafe { std::slice::from_raw_parts(buffer.buffer, buffer.buffer_length) };
        Ok(Some(Taken {
            message,
            published,
            publication,
        }))
    }
}

/// A message taken, still serialised as it arrived.
pub(super) struct Taken<'a> {
    pub(super) message: &'a [u8],
    /// The time its publisher gave it, nanoseconds since the Unix epoch on
    /// the publisher's clock; 0 if the middleware has none.
    pub(super) published: u64,
    /// Which publisher sent it and its count of it, if the middleware says.
    pub(super) publication: Option<Publication>,
}

unsafe extern "C" {
    /// Finalises a node's wait set, buffers, subscriptions, node and
    /// context, in that order, in C++ (`teardown.cpp`): an exception the
    /// middleware throws on the way stops there instead of aborting Kapto.
    /// Returns how many of the calls threw.
    fn kapto_rcl_teardown(
        wait_set: *mut c_void,
        buffers: *mut c_void,
        buffer_size: usize,
        buffer_count: usize,
        subscriptions: *mut c_void,
        subscription_size: usize,
        subscription_count: usize,
        node: *mut c_void,
        context: *mut c_void,
    ) -> usize;
}

impl Drop for Node {
    fn drop(&mut self) {
        // SAFETY: each is finalised once, in the reverse order of being made,
        // and each `_fini` accepts one that was zeroed but never initialised,
        // as when opening failed part way; the vectors' elements lie
        // `size_of` apart from their pointers, as the C++ steps through them.
        // Their results, and an exception thrown, can't be acted on: the
        // node goes as the receiver ends.
        unsafe {
            kapto_rcl_teardown(
                (&raw mut *self.wait_set).cast(),
                self.buffers.as_mut_ptr().cast(),
                size_of::<rcl_serialized_message_t>(),
                self.buffers.len(),
                self.subscriptions.as_mut_ptr().cast(),
                size_of::<rcl_subscription_t>(),
                self.subscriptions.len(),
                (&raw mut *self.handle).cast(),
                (&raw mut *self.context).cast(),
            );
        }
    }
}

/// The support of `message_type` (`package/msg/Type`) for rcl, from the
/// package's `lib<package>__rosidl_typesupport_c.so`, which ROS 2 installs for
/// every message package and the setup script puts on the library path. The
/// library stays loaded: the middleware may keep the type registered for as
/// long as the process lives.
fn type_support(message_type: &str) -> Result<*const rosidl_message_type_support_t, KaptoError> {
    let unavailable = |detail: String| {
        KaptoError::new(ErrorCode::TransportRos2TypeUnavailable)
            .detail(detail)
            .context("type", message_type)
    };
    let [package, "msg", name] = message_type.split('/').collect::<Vec<_>>()[..] else {
        return Err(unavailable(
            "not a message type, `<package>/msg/<Type>`".to_owned(),
        ));
    };
    let library = c_string(
        &format!("lib{package}__rosidl_typesupport_c.so"),
        ErrorCode::TransportRos2TypeUnavailable,
    )?;
    let symbol = c_string(
        &format!("rosidl_typesupport_c__get_message_type_support_handle__{package}__msg__{name}"),
        ErrorCode::TransportRos2TypeUnavailable,
    )?;
    // SAFETY: both names are C strings; `dlopen` and `dlsym` only read them,
    // and report failure with a null pointer and `dlerror`.
    let function = unsafe {
        let handle = libc::dlopen(library.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
        if handle.is_null() {
            return Err(unavailable(format!(
                "no library {}: {}",
                library.to_string_lossy(),
                dl_error()
            )));
        }
        libc::dlsym(handle, symbol.as_ptr())
    };
    if function.is_null() {
        return Err(unavailable(format!(
            "{} has no {name}: {}",
            library.to_string_lossy(),
            dl_error()
        )));
    }
    // SAFETY: rosidl generates this symbol for every message type as
    // `const rosidl_message_type_support_t * f(void)`, which is what it is
    // called as here; it returns a pointer to a static in the library.
    let support = unsafe {
        let get: unsafe extern "C" fn() -> *const rosidl_message_type_support_t =
            std::mem::transmute(function);
        get()
    };
    if support.is_null() {
        return Err(unavailable(format!("{name}'s support is missing")));
    }
    Ok(support)
}

/// Why the last `dlopen` or `dlsym` on this thread failed.
fn dl_error() -> String {
    // SAFETY: `dlerror` returns null or a C string valid until the next
    // call on this thread, copied before then.
    unsafe {
        let error = libc::dlerror();
        if error.is_null() {
            String::new()
        } else {
            CStr::from_ptr(error).to_string_lossy().into_owned()
        }
    }
}

fn c_string(text: &str, code: ErrorCode) -> Result<CString, KaptoError> {
    CString::new(text)
        .map_err(|_| KaptoError::new(code).detail(format!("`{text}` has a NUL character")))
}

const fn code(ret: u32) -> rcl_ret_t {
    #[allow(
        clippy::cast_possible_wrap,
        reason = "rcl's return codes are small positive numbers"
    )]
    let ret = ret as rcl_ret_t;
    ret
}

const fn ok() -> rcl_ret_t {
    code(RCL_RET_OK)
}

/// `Ok` if `ret` is rcl's success, or else an error of `code` saying what
/// failed and why, as rcl put it.
fn check(ret: rcl_ret_t, error: ErrorCode, what: &str) -> Result<(), KaptoError> {
    if ret == ok() {
        return Ok(());
    }
    // SAFETY: rcl keeps the last error per thread; reading it copies the
    // message out, and resetting it clears it for the next failure.
    let reason = unsafe {
        let message = rcutils_get_error_string();
        rcutils_reset_error();
        CStr::from_ptr(message.str_.as_ptr().cast::<c_char>())
            .to_string_lossy()
            .into_owned()
    };
    Err(KaptoError::new(error).detail(format!("{what}: {reason} (rcl {ret})")))
}
