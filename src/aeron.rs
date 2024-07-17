/// [aeron](https://github.com/real-logic/aeron) nbio implementations
///
/// ## Functionality
///
/// This module does not aim to provide all aeron-related functionality.
/// Instead, it aims to provide implementations that allow aeron publications and subscriptions
/// to fit the core nbio patterns while providing idiomatic Rust patterns that are memory-safe.
///
/// ## Dependencies
///
/// This module utilizes the [libaeron-sys](https://github.com/bspeice/libaeron-sys) library to provide aeron C bindings.
/// `libaeron-sys` requires that `cmake` and `clang` are installed on the system in order to build aeron.
use std::{
    collections::VecDeque,
    ffi::{c_char, c_void, CStr, CString},
    fmt::Debug,
    io,
    marker::PhantomData,
    ptr::null_mut,
    sync::Arc,
};

use internal::{
    assemble, drop_client, drop_context, drop_fragment_assembler, drop_publication,
    drop_subscription, forget, on_image_available, on_image_unavailable, parse_reserved_value,
    ManagedPtr, PublicationConnection, SubscriptionConnection, SubscriptionHandler,
};
use libaeron_sys::{
    aeron_async_add_publication, aeron_async_add_publication_poll, aeron_async_add_publication_t,
    aeron_async_add_subscription, aeron_async_add_subscription_poll,
    aeron_async_add_subscription_t, aeron_context_init, aeron_context_t, aeron_errmsg,
    aeron_fragment_assembler_create, aeron_fragment_assembler_handler, aeron_fragment_assembler_t,
    aeron_image_constants_t, aeron_init, aeron_publication_offer, aeron_publication_t, aeron_start,
    aeron_subscription_poll, aeron_subscription_t, aeron_t, AERON_PUBLICATION_ADMIN_ACTION,
    AERON_PUBLICATION_BACK_PRESSURED, AERON_PUBLICATION_CLOSED, AERON_PUBLICATION_ERROR,
    AERON_PUBLICATION_MAX_POSITION_EXCEEDED, AERON_PUBLICATION_NOT_CONNECTED,
};

use crate::{
    Callback, DriveOutcome, Publish, PublishOutcome, Receive, ReceiveOutcome, Session,
    SessionStatus,
};

/// The aeron context, which is used to instantiate a [`Client`].
///
/// ## Example
///
/// ```no_run
/// use nbio::aeron::Context;
/// let context = Context::new()
///     .unwrap()
///     .with_dir("/dev/shm/aeron-default")
///     .unwrap();
/// ```
pub struct Context {
    context: Box<ManagedPtr<aeron_context_t>>,
}
impl Context {
    /// Create a new context to be used by a [`Client`].
    pub fn new() -> Result<Self, io::Error> {
        let context = unsafe {
            let in_ptr = &mut null_mut() as *mut *mut aeron_context_t;
            if aeron_context_init(in_ptr) < 0 {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!(
                        "aeron_context_init failed: {:?}",
                        CStr::from_ptr(aeron_errmsg()).to_string_lossy()
                    ),
                ));
            }
            if (*in_ptr).is_null() {
                return Err(io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "aeron_context_init did not allocate",
                ));
            }
            Box::new(ManagedPtr::new(*in_ptr).with_drop_fn(drop_context))
        };
        Ok(Self { context })
    }

    /// Override the default `client_name` for this context
    pub fn with_client_name(self, value: &str) -> Result<Self, io::Error> {
        unsafe {
            libaeron_sys::aeron_context_set_client_name(
                self.context.ptr(),
                CString::new(value)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?
                    .as_ptr(),
            );
        }
        Ok(self)
    }

    /// Override the default directory of the aeron media driver to be used by this context
    pub fn with_dir(self, value: &str) -> Result<Self, io::Error> {
        unsafe {
            libaeron_sys::aeron_context_set_dir(
                self.context.ptr(),
                CString::new(value)
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?
                    .as_ptr(),
            );
        }
        Ok(self)
    }

    /// Override the default `driver_timeout_ms`
    pub fn with_driver_timeout_ms(self, value: u64) -> Self {
        unsafe {
            libaeron_sys::aeron_context_set_driver_timeout_ms(self.context.ptr(), value);
        }
        self
    }

    /// Override the default `idle_sleep_duration_ns`
    pub fn with_idle_sleep_duration_ns(self, value: u64) -> Self {
        unsafe {
            libaeron_sys::aeron_context_set_idle_sleep_duration_ns(self.context.ptr(), value);
        }
        self
    }

    /// Override the default `keepalive_interval_ns`
    pub fn with_keepalive_interval_ns(self, value: u64) -> Self {
        unsafe {
            libaeron_sys::aeron_context_set_keepalive_interval_ns(self.context.ptr(), value);
        }
        self
    }

    /// Override the default `pre_touch_mapped_memory` flag
    pub fn with_pre_touch_mapped_memory(self, value: bool) -> Self {
        unsafe {
            libaeron_sys::aeron_context_set_pre_touch_mapped_memory(self.context.ptr(), value);
        }
        self
    }

    /// Override the default `resource_linger_duration_ns`
    pub fn with_resource_linger_duration_ns(self, value: u64) -> Self {
        unsafe {
            libaeron_sys::aeron_context_set_resource_linger_duration_ns(self.context.ptr(), value);
        }
        self
    }

    /// Override the default `use_conductor_agent_invoker` flag
    pub fn with_use_conductor_agent_invoker(self, value: bool) -> Self {
        unsafe {
            libaeron_sys::aeron_context_set_use_conductor_agent_invoker(self.context.ptr(), value);
        }
        self
    }

    // libaeron_sys::aeron_context_set_agent_on_start_function(context, value, state)
    // libaeron_sys::aeron_context_set_error_handler(context, handler, clientd)
    // libaeron_sys::aeron_context_set_on_available_counter(context, handler, clientd)
    // libaeron_sys::aeron_context_set_on_close_client(context, handler, clientd)
    // libaeron_sys::aeron_context_set_on_new_exclusive_publication(context, handler, clientd)
    // libaeron_sys::aeron_context_set_on_new_publication(context, handler, clientd)
    // libaeron_sys::aeron_context_set_on_new_subscription(context, handler, clientd)
    // libaeron_sys::aeron_context_set_on_unavailable_counter(context, handler, clientd)
}

/// The aeron client, which is used to instantiate a [`Publication`] or [`Subscription`].
///
/// ## Example
///
/// ```no_run
/// use nbio::aeron::{Client, Context};
/// let context = Context::new().unwrap();
/// let client = Client::new(context).unwrap();
/// client.start().unwrap();
/// ```
pub struct Client {
    _context: Context, // enforce keeping reference to outlive the client
    client: Box<ManagedPtr<aeron_t>>,
}
impl Client {
    /// Create a new client for the given context, which will need to be started by calling [`Client::start`].
    pub fn new(context: Context) -> Result<Arc<Self>, io::Error> {
        let client = unsafe {
            let in_ptr = &mut null_mut() as *mut *mut aeron_t;
            if aeron_init(in_ptr, context.context.ptr()) < 0 {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!(
                        "aeron_init failed: {:?}",
                        CStr::from_ptr(aeron_errmsg()).to_string_lossy()
                    ),
                ));
            }
            if (*in_ptr).is_null() {
                return Err(io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "aeron_init did not allocate",
                ));
            }
            Box::new(ManagedPtr::new(*in_ptr).with_drop_fn(drop_client))
        };
        Ok(Arc::new(Self {
            _context: context,
            client,
        }))
    }

    /// Start the client
    pub fn start(&self) -> Result<(), io::Error> {
        unsafe {
            if aeron_start(self.client.ptr()) < 0 {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    format!(
                        "aeron_start failed: {:?}",
                        CStr::from_ptr(aeron_errmsg()).to_string_lossy()
                    ),
                ));
            }
        }
        Ok(())
    }
}

/// A message received by a [`Subscription`] that is owned a cloneable
#[derive(Debug, Clone)]
pub struct OwnedMessage {
    pub term_offset: i32,
    pub session_id: i32,
    pub stream_id: i32,
    pub term_id: i32,
    pub reserved_value: i64,
    pub payload: Vec<u8>,
}

/// A message received by a [`Subscription`] whos data is referenced in a slice
#[derive(Debug)]
pub struct BorrowedMessage<'a> {
    pub term_offset: i32,
    pub session_id: i32,
    pub stream_id: i32,
    pub term_id: i32,
    pub reserved_value: i64,
    pub payload: &'a [u8],
}

/// Used by the [`OnImageAvailable`] and [`OnImageUnavailable`] callbacks
#[derive(Debug)]
pub struct ImageDetails {
    pub source_identity: CString,
    pub correlation_id: i64,
    pub join_position: i64,
    pub position_bits_to_shift: usize,
    pub term_buffer_length: usize,
    pub mtu_length: usize,
    pub session_id: i32,
    pub initial_term_id: i32,
    pub subscriber_position_id: i32,
}
impl From<&aeron_image_constants_t> for ImageDetails {
    fn from(image: &aeron_image_constants_t) -> Self {
        let source_identity =
            unsafe { CStr::from_ptr(image.source_identity as *const c_char).to_owned() };
        Self {
            source_identity,
            correlation_id: image.correlation_id,
            join_position: image.join_position,
            position_bits_to_shift: image.position_bits_to_shift,
            term_buffer_length: image.term_buffer_length,
            mtu_length: image.mtu_length,
            session_id: image.session_id,
            initial_term_id: image.initial_term_id,
            subscriber_position_id: image.subscriber_position_id,
        }
    }
}

/// Optional callback to be used by a [`Subscription`] when a new image is available
pub trait OnImageAvailable {
    fn on_image_available(&mut self, image: ImageDetails);
}
impl<F: FnMut(ImageDetails)> OnImageAvailable for F {
    fn on_image_available(&mut self, image: ImageDetails) {
        self(image)
    }
}

/// Optional callback to be used by a [`Subscription`] when a new image is unavailable
pub trait OnImageUnavailable {
    fn on_image_unavailable(&mut self, image: ImageDetails);
}
impl<F: FnMut(ImageDetails)> OnImageUnavailable for F {
    fn on_image_unavailable(&mut self, image: ImageDetails) {
        self(image)
    }
}

/// An aeron subscription, which can either [`Receive`] or act as a [`crate::Callback`].
///
/// ## Receive
///
/// A subscription acts as a receiver where assembled fragments are returned when [`Receive::receive`] is called.
///
/// By default, a `Subscription` is a `Subscription<Buffer>` which will [`Recieve`] `Vec<u8>` payloads.
/// To receive other metadata associated with the payload, instantiate a `Subscription<Message>` to
/// [`Receive`] an [`OwnedMessage`] instead, which will contain the `stream_id`, `reserved_value`, etc.
///
/// ## Callback
///
/// Callbacks can be used instead of [`Receive::receive`] to handle copy-free aeron messages.
/// This is done by utilizing [`Self::with_callback`] with a `Box<dyn for<'a> Callback<BorrowedMessage<'a>> + 'static>`.
/// When callback mode is enabled, calls to [`Receive::receive`] will fail with an error.
///
/// ## Receive Example
///
/// ```no_run
/// use std::sync::Arc;
/// use nbio::{Receive, ReceiveOutcome, Session, SessionStatus};
/// use nbio::aeron::{Client, Context, Subscription};
///
/// let context = Context::new().unwrap();
/// let client = Arc::new(Client::new(context).unwrap());
/// client.start().unwrap();
///
/// let mut subscription: Subscription = Subscription::new(Arc::clone(&client), "aeron:ipc".to_owned(), 1);
/// while subscription.status() == SessionStatus::Establishing {
///     subscription.drive().unwrap();
/// }
///
/// loop {
///     subscription.drive().unwrap();
///     if let ReceiveOutcome::Payload(x) = subscription.receive().unwrap() {
///         println!("received: {:?}", x);
///     }
/// }
/// ```
///
/// ## Callback Example
///
/// ```no_run
/// use std::sync::Arc;
/// use nbio::{Callback, Receive, ReceiveOutcome, Session, SessionStatus};
/// use nbio::aeron::{BorrowedMessage, Client, Context, Subscription};
///
/// let context = Context::new().unwrap();
/// let client = Arc::new(Client::new(context).unwrap());
/// client.start().unwrap();
/// let callback = |x: BorrowedMessage| {
///     println!("received! {:?}", String::from_utf8_lossy(&x.payload));
/// };
///
/// let mut subscription: Subscription = Subscription::new(Arc::clone(&client), "aeron:ipc".to_owned(), 1)
///     .with_callback(Box::new(callback)).unwrap()
///     .with_on_image_available_callback(Box::new(|x| println!("image available: {x:?}")))
///     .with_on_image_unavailable_callback(Box::new(|x| println!("image unavailable: {x:?}")));
///
/// loop {
///     subscription.drive().unwrap();
/// }
/// ```
pub struct Subscription<T: AeronMarker = Buffer> {
    uri: String,
    stream_id: i32,
    fragment_limit: usize,
    state: Box<SubscriptionState>,
    client: Arc<Client>,
    _phantom: PhantomData<fn(&T)>,
}
impl<T: AeronMarker> Subscription<T> {
    /// Create a new subscription
    pub fn new(client: Arc<Client>, uri: String, stream_id: i32) -> Self {
        Self {
            client,
            uri,
            stream_id,
            fragment_limit: 10,
            state: Box::new(SubscriptionState {
                connection: SubscriptionConnection::Initialized,
                fragment_assembler: ManagedPtr::null(),
                handler: SubscriptionHandler::Receiver(VecDeque::new()),
                on_image_available_callback: None,
                on_image_unavailable_callback: None,
            }),
            _phantom: PhantomData,
        }
    }

    /// Override the default `fragment_limit` of 10
    pub fn with_fragment_limit(mut self, fragment_limit: usize) -> Self {
        self.fragment_limit = fragment_limit;
        self
    }

    /// Use [`crate::Callback`] mode instead of [`Receive`] mode, using the given callback on calls to [`Session::drive`].
    pub fn with_callback(
        mut self,
        callback: Box<dyn for<'a> Callback<BorrowedMessage<'a>> + 'static>,
    ) -> Result<Self, io::Error> {
        if let SubscriptionConnection::Initialized = self.state.connection {
            self.state.handler = SubscriptionHandler::Callback(callback);
            Ok(self)
        } else {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "callback must be set prior to connecting",
            ));
        }
    }

    /// Set the optional "image available" callback
    pub fn with_on_image_available_callback(
        mut self,
        callback: Box<dyn OnImageAvailable + 'static>,
    ) -> Self {
        self.state.on_image_available_callback = Some(callback);
        self
    }

    /// Set the optional "image unavailable" callback
    pub fn with_on_image_unavailable_callback(
        mut self,
        callback: Box<dyn OnImageUnavailable + 'static>,
    ) -> Self {
        self.state.on_image_unavailable_callback = Some(callback);
        self
    }
}
impl<T: AeronMarker> Session for Subscription<T> {
    fn drive(&mut self) -> Result<DriveOutcome, io::Error> {
        match &mut self.state.connection {
            SubscriptionConnection::Initialized => {
                let c_uri = CString::new(self.uri.as_str())
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                let on_image_available_ptr: Option<*mut Box<dyn OnImageAvailable>> = self
                    .state
                    .on_image_available_callback
                    .as_mut()
                    .map(|x| x as *mut Box<dyn OnImageAvailable>);
                let on_image_unavailable_ptr: Option<*mut Box<dyn OnImageUnavailable>> = self
                    .state
                    .on_image_unavailable_callback
                    .as_mut()
                    .map(|x| x as *mut Box<dyn OnImageUnavailable>);
                let async_add_subscription = unsafe {
                    let in_ptr: *mut *mut aeron_async_add_subscription_t = &mut null_mut();
                    let result = aeron_async_add_subscription(
                        in_ptr,
                        self.client.client.ptr(),
                        c_uri.as_ptr(),
                        self.stream_id,
                        Some(on_image_available),
                        on_image_available_ptr
                            .map(|x| x as *mut c_void)
                            .unwrap_or(null_mut()),
                        Some(on_image_unavailable),
                        on_image_unavailable_ptr
                            .map(|x| x as *mut c_void)
                            .unwrap_or(null_mut()),
                    );
                    if result < 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionRefused,
                            format!(
                                "aeron_async_add_subscription error {result}: {:?}",
                                CStr::from_ptr(aeron_errmsg()).to_string_lossy()
                            ),
                        ));
                    }
                    if (*in_ptr).is_null() {
                        return Err(io::Error::new(
                            io::ErrorKind::OutOfMemory,
                            "aeron_async_add_subscription did not allocate",
                        ));
                    }
                    ManagedPtr::new(*in_ptr)
                };
                self.state.connection = SubscriptionConnection::Connecting(async_add_subscription);
                Ok(DriveOutcome::Active)
            }
            SubscriptionConnection::Connecting(async_add_subscription) => unsafe {
                let in_ptr: *mut *mut aeron_subscription_t = &mut null_mut();
                if aeron_async_add_subscription_poll(in_ptr, async_add_subscription.ptr()) < 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        format!(
                            "aeron_async_add_subscription_poll failed: {:?}",
                            CStr::from_ptr(aeron_errmsg()).to_string_lossy()
                        ),
                    ));
                }
                if (*in_ptr).is_null() {
                    Ok(DriveOutcome::Idle)
                } else {
                    async_add_subscription.set_drop_fn(forget); // avoid double-free: aeron_async_add_subscription_poll does this on success
                    let subscription = ManagedPtr::new(*in_ptr).with_drop_fn(drop_subscription);
                    let in_ptr: *mut *mut aeron_fragment_assembler_t = &mut null_mut();
                    let handler_ptr: *mut SubscriptionHandler = &mut self.state.handler;
                    let result = aeron_fragment_assembler_create(
                        in_ptr,
                        Some(assemble),
                        handler_ptr as *mut c_void,
                    );
                    if result < 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionRefused,
                            format!(
                                "aeron_fragment_assembler_create error: {:?}",
                                CStr::from_ptr(aeron_errmsg()).to_string_lossy()
                            ),
                        ));
                    }
                    if (*in_ptr).is_null() {
                        return Err(io::Error::new(
                            io::ErrorKind::OutOfMemory,
                            "aeron_fragment_assembler_create did not allocate",
                        ));
                    }
                    self.state.fragment_assembler =
                        ManagedPtr::new(*in_ptr).with_drop_fn(drop_fragment_assembler);
                    self.state.connection = SubscriptionConnection::Connected(subscription);
                    Ok(DriveOutcome::Active)
                }
            },
            SubscriptionConnection::Connected(subscription) => {
                if self.state.handler.is_callback() {
                    let result = unsafe {
                        aeron_subscription_poll(
                            subscription.ptr(),
                            Some(aeron_fragment_assembler_handler),
                            self.state.fragment_assembler.ptr() as *mut c_void,
                            self.fragment_limit,
                        )
                    };
                    if result == 0 {
                        Ok(DriveOutcome::Idle)
                    } else if result > 0 {
                        Ok(DriveOutcome::Active)
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::Other,
                            format!("aeron_subscription_poll failed: {:?}", unsafe {
                                CStr::from_ptr(aeron_errmsg()).to_string_lossy()
                            }),
                        ))
                    }
                } else {
                    Ok(DriveOutcome::Idle)
                }
            }
            SubscriptionConnection::Terminated => {
                Err(io::Error::new(io::ErrorKind::NotConnected, "not connected"))
            }
        }
    }
    fn status(&self) -> SessionStatus {
        match &self.state.connection {
            SubscriptionConnection::Initialized => SessionStatus::Establishing,
            SubscriptionConnection::Connecting(_) => SessionStatus::Establishing,
            SubscriptionConnection::Connected(_) => SessionStatus::Established,
            SubscriptionConnection::Terminated => SessionStatus::Terminated,
        }
    }
}
impl Receive for Subscription<Buffer> {
    type ReceivePayload<'a> = Vec<u8>;
    fn receive<'a>(
        &'a mut self,
    ) -> Result<crate::ReceiveOutcome<Self::ReceivePayload<'a>>, io::Error> {
        match &mut self.state.connection {
            SubscriptionConnection::Connected(subscription) => {
                if let SubscriptionHandler::Receiver(received) = &mut self.state.handler {
                    if let Some(x) = received.pop_front() {
                        return Ok(ReceiveOutcome::Payload(x.payload));
                    }
                    unsafe { self.client.client.ptr() };
                    unsafe { self.client._context.context.ptr() };
                    let result = unsafe {
                        aeron_subscription_poll(
                            subscription.ptr(),
                            Some(aeron_fragment_assembler_handler),
                            self.state.fragment_assembler.ptr() as *mut c_void,
                            self.fragment_limit,
                        )
                    };
                    if result < 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            format!("aeron_subscription_poll failed: {:?}", unsafe {
                                CStr::from_ptr(aeron_errmsg()).to_string_lossy()
                            }),
                        ));
                    }
                    match received.pop_front() {
                        None => Ok(ReceiveOutcome::Idle),
                        Some(x) => Ok(ReceiveOutcome::Payload(x.payload)),
                    }
                } else {
                    Err(io::Error::new(io::ErrorKind::Other, "subscription is configured for callback mode, polling receive is not enabled"))
                }
            }
            _ => Err(io::Error::new(io::ErrorKind::NotConnected, "not connected")),
        }
    }
}
impl Receive for Subscription<Message> {
    type ReceivePayload<'a> = OwnedMessage;
    fn receive<'a>(
        &'a mut self,
    ) -> Result<crate::ReceiveOutcome<Self::ReceivePayload<'a>>, io::Error> {
        match &mut self.state.connection {
            SubscriptionConnection::Connected(subscription) => {
                if let SubscriptionHandler::Receiver(received) = &mut self.state.handler {
                    if let Some(x) = received.pop_front() {
                        return Ok(ReceiveOutcome::Payload(x));
                    }
                    let result = unsafe {
                        aeron_subscription_poll(
                            subscription.ptr(),
                            Some(aeron_fragment_assembler_handler),
                            self.state.fragment_assembler.ptr() as *mut c_void,
                            self.fragment_limit,
                        )
                    };
                    if result < 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::Other,
                            format!("aeron_subscription_poll failed: {:?}", unsafe {
                                CStr::from_ptr(aeron_errmsg()).to_string_lossy()
                            }),
                        ));
                    }

                    match received.pop_front() {
                        None => Ok(ReceiveOutcome::Idle),
                        Some(x) => Ok(ReceiveOutcome::Payload(x)),
                    }
                } else {
                    Err(io::Error::new(io::ErrorKind::Other, "subscription is configured for callback mode, polling receive is not enabled"))
                }
            }
            _ => Err(io::Error::new(io::ErrorKind::NotConnected, "not connected")),
        }
    }
}
impl<T: AeronMarker> Debug for Subscription<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("uri", &self.uri)
            .field("stream_id", &self.stream_id)
            .finish()
    }
}
impl<T: AeronMarker> Drop for Subscription<T> {
    fn drop(&mut self) {
        // explicitly drop connection here so it will always drop before Arc<Client>
        self.state.connection = SubscriptionConnection::Terminated;
    }
}

struct SubscriptionState {
    handler: SubscriptionHandler,
    connection: SubscriptionConnection,
    fragment_assembler: ManagedPtr<aeron_fragment_assembler_t>,
    on_image_available_callback: Option<Box<dyn OnImageAvailable + 'static>>,
    on_image_unavailable_callback: Option<Box<dyn OnImageUnavailable + 'static>>,
}

/// An aeron publication, which can be used to send messages to the given `uri` and `stream_id`.
///
/// ## Reserved Value
///
/// By default, a `Publication` is a `Publication<Buffer>` which will [`Publish`] `&[u8]` slices.
/// To provide a "reserved value" to be published in the aeron header, instantiate a `Publication<Message>`,
/// which will [`Publish`] a [`PublishMessage`] instead, where a reserved value can be provided alongside the
/// payload slice.
pub struct Publication<T: AeronMarker = Buffer> {
    uri: String,
    stream_id: i32,
    state: Box<PublicationState>,
    client: Arc<Client>,
    _phantom: PhantomData<fn(&T)>,
}
impl<T: AeronMarker> Publication<T> {
    /// Create a new publication
    pub fn new(client: Arc<Client>, uri: String, stream_id: i32) -> Self {
        Self {
            client,
            uri,
            stream_id,
            state: Box::new(PublicationState {
                connection: PublicationConnection::Initialized,
            }),
            _phantom: PhantomData,
        }
    }
    fn map_result<I>(payload: I, result: i64) -> Result<PublishOutcome<I>, io::Error> {
        if result >= 0 {
            return Ok(PublishOutcome::Published);
        }
        match result as i32 {
            AERON_PUBLICATION_NOT_CONNECTED => Ok(PublishOutcome::Published), // no one to consume, normal flow
            AERON_PUBLICATION_BACK_PRESSURED => Ok(PublishOutcome::Incomplete(payload)),
            AERON_PUBLICATION_ADMIN_ACTION => Ok(PublishOutcome::Incomplete(payload)),
            AERON_PUBLICATION_CLOSED => Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                format!("aeron_publication_offer error: AERON_PUBLICATION_CLOSED"),
            )),
            AERON_PUBLICATION_MAX_POSITION_EXCEEDED => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("aeron_publication_offer error: AERON_PUBLICATION_MAX_POSITION_EXCEEDED"),
            )),
            AERON_PUBLICATION_ERROR => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("aeron_publication_offer error: AERON_PUBLICATION_ERROR"),
            )),
            result => Err(io::Error::new(
                io::ErrorKind::Other,
                format!("aeron_publication_offer error: {result}"),
            )),
        }
    }
}
impl<T: AeronMarker> Session for Publication<T> {
    fn drive(&mut self) -> Result<DriveOutcome, io::Error> {
        match &mut self.state.connection {
            PublicationConnection::Initialized => {
                let c_uri = CString::new(self.uri.as_str())
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
                let async_add_publication = unsafe {
                    let in_ptr: *mut *mut aeron_async_add_publication_t = &mut null_mut();
                    let result = aeron_async_add_publication(
                        in_ptr,
                        self.client.client.ptr(),
                        c_uri.as_ptr(),
                        self.stream_id,
                    );
                    if result < 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::ConnectionRefused,
                            format!("aeron_async_add_publication error: {result}"),
                        ));
                    }
                    if (*in_ptr).is_null() {
                        return Err(io::Error::new(
                            io::ErrorKind::OutOfMemory,
                            "aeron_async_add_publication did not allocate",
                        ));
                    }
                    ManagedPtr::new(*in_ptr)
                };
                self.state.connection = PublicationConnection::Connecting(async_add_publication);
                Ok(DriveOutcome::Active)
            }
            PublicationConnection::Connecting(async_add_publication) => unsafe {
                let in_ptr: *mut *mut aeron_publication_t = &mut null_mut();
                if aeron_async_add_publication_poll(in_ptr, async_add_publication.ptr()) < 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        format!(
                            "aeron_async_add_publication_poll failed: {:?}",
                            CStr::from_ptr(aeron_errmsg()).to_string_lossy()
                        ),
                    ));
                }
                if (*in_ptr).is_null() {
                    Ok(DriveOutcome::Idle)
                } else {
                    async_add_publication.set_drop_fn(forget); // avoid double-free: aeron_async_add_publication_poll does this on success
                    let publication = ManagedPtr::new(*in_ptr).with_drop_fn(drop_publication);
                    self.state.connection = PublicationConnection::Connected(publication);
                    Ok(DriveOutcome::Active)
                }
            },
            PublicationConnection::Connected(_) => Ok(DriveOutcome::Idle),
            PublicationConnection::Terminated => {
                Err(io::Error::new(io::ErrorKind::NotConnected, "not connected"))
            }
        }
    }
    fn status(&self) -> SessionStatus {
        match &self.state.connection {
            PublicationConnection::Initialized => SessionStatus::Establishing,
            PublicationConnection::Connecting(_) => SessionStatus::Establishing,
            PublicationConnection::Connected(_) => SessionStatus::Established,
            PublicationConnection::Terminated => SessionStatus::Terminated,
        }
    }
}
impl Publish for Publication<Buffer> {
    type PublishPayload<'a> = &'a [u8];
    fn publish<'a>(
        &mut self,
        payload: Self::PublishPayload<'a>,
    ) -> Result<crate::PublishOutcome<Self::PublishPayload<'a>>, std::io::Error> {
        match &self.state.connection {
            PublicationConnection::Connected(publication) => {
                let result = unsafe {
                    aeron_publication_offer(
                        publication.ptr(),
                        payload.as_ptr(),
                        payload.len(),
                        None,
                        null_mut(),
                    )
                };
                Self::map_result(payload, result)
            }
            _ => Err(io::Error::new(io::ErrorKind::NotConnected, "not connected")),
        }
    }
}
impl Publish for Publication<Message> {
    type PublishPayload<'a> = PublishMessage<'a>;
    fn publish<'a>(
        &mut self,
        payload: Self::PublishPayload<'a>,
    ) -> Result<crate::PublishOutcome<Self::PublishPayload<'a>>, std::io::Error> {
        match &self.state.connection {
            PublicationConnection::Connected(publication) => {
                let payload_ptr: *const Self::PublishPayload<'a> = &payload;
                let result = unsafe {
                    aeron_publication_offer(
                        publication.ptr(),
                        payload.buffer.as_ptr(),
                        payload.buffer.len(),
                        Some(parse_reserved_value),
                        payload_ptr as *mut c_void,
                    )
                };
                Self::map_result(payload, result)
            }
            _ => Err(io::Error::new(io::ErrorKind::NotConnected, "not connected")),
        }
    }
}
impl<T: AeronMarker> Debug for Publication<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Publication")
            .field("uri", &self.uri)
            .field("stream_id", &self.stream_id)
            .finish()
    }
}
impl<T: AeronMarker> Drop for Publication<T> {
    fn drop(&mut self) {
        // explicitly drop connection here so it will always drop before Arc<Client>
        self.state.connection = PublicationConnection::Terminated;
    }
}

struct PublicationState {
    connection: PublicationConnection,
}

/// Marker trait for [`Publication`] and [`Subscription`]
pub trait AeronMarker {}

/// The default marker trait on [`Publication`] or [`Subscription`] to [`Publish`] or [`Receive`] payload buffers.
pub struct Buffer;
impl AeronMarker for Buffer {}

/// An optional marker trait on [`Publication`] or [`Subscription`] to [`Publish`] [`PublishMessage`] or [`Receive`] an [`OwnedMessage`].
pub struct Message;
impl AeronMarker for Message {}

/// A buffer and reserved value, which can be published to a `Publication<Message>`
#[derive(Debug)]
pub struct PublishMessage<'a> {
    pub buffer: &'a [u8],
    pub reserved_value: i64,
}

mod internal {
    use libaeron_sys::{
        aeron_async_add_publication_t, aeron_async_add_subscription_t, aeron_close,
        aeron_context_close, aeron_context_t, aeron_fragment_assembler_delete,
        aeron_fragment_assembler_t, aeron_header_t, aeron_header_values, aeron_header_values_t,
        aeron_image_constants, aeron_image_constants_t, aeron_image_t, aeron_publication_close,
        aeron_publication_t, aeron_subscription_close, aeron_subscription_t, aeron_t,
    };

    use super::{
        BorrowedMessage, ImageDetails, OnImageAvailable, OnImageUnavailable, OwnedMessage,
        PublishMessage,
    };
    use crate::Callback;
    use std::{
        alloc::{alloc, dealloc, Layout},
        collections::VecDeque,
        ptr::null_mut,
    };

    pub enum SubscriptionHandler {
        Receiver(VecDeque<OwnedMessage>),
        Callback(Box<dyn for<'a> Callback<BorrowedMessage<'a>> + 'static>),
    }
    impl SubscriptionHandler {
        pub fn is_callback(&self) -> bool {
            match self {
                Self::Receiver(_) => false,
                Self::Callback(_) => true,
            }
        }
    }

    pub struct ManagedPtr<T> {
        data: *mut T,
        drop_fn: Destructor<T>,
    }
    impl<T> ManagedPtr<T> {
        pub fn new(data: *mut T) -> Self {
            Self {
                data,
                drop_fn: free,
            }
        }
        pub fn null() -> Self {
            Self {
                data: null_mut(),
                drop_fn: free,
            }
        }
        pub fn set_drop_fn<'a>(&'a mut self, drop_fn: Destructor<T>) -> &'a Self {
            self.drop_fn = drop_fn;
            self
        }
        pub fn with_drop_fn(mut self, drop_fn: Destructor<T>) -> Self {
            self.drop_fn = drop_fn;
            self
        }
        pub unsafe fn ptr(&self) -> *mut T {
            self.data
        }
    }
    impl<T> Drop for ManagedPtr<T> {
        fn drop(&mut self) {
            let ptr = self.data;
            if !ptr.is_null() {
                (self.drop_fn)(ptr)
            }
        }
    }

    pub type Destructor<T> = fn(*mut T);
    pub fn forget<T>(_: *mut T) {}
    pub fn free<T>(ptr: *mut T) {
        if !ptr.is_null() {
            unsafe {
                dealloc(ptr as *mut u8, Layout::new::<T>());
            }
        }
    }

    pub fn drop_client(client: *mut aeron_t) {
        unsafe {
            aeron_close(client);
        }
    }

    pub fn drop_context(context: *mut aeron_context_t) {
        unsafe {
            aeron_context_close(context);
        }
    }

    pub fn drop_fragment_assembler(fragment_assembler: *mut aeron_fragment_assembler_t) {
        unsafe {
            aeron_fragment_assembler_delete(fragment_assembler);
        }
    }

    pub fn drop_subscription(subscription: *mut aeron_subscription_t) {
        unsafe {
            aeron_subscription_close(subscription, None, null_mut());
        }
    }

    pub fn drop_publication(publication: *mut aeron_publication_t) {
        unsafe {
            aeron_publication_close(publication, None, null_mut());
        }
    }

    pub enum PublicationConnection {
        Initialized,
        Connecting(ManagedPtr<aeron_async_add_publication_t>),
        Connected(ManagedPtr<aeron_publication_t>),
        Terminated,
    }

    pub enum SubscriptionConnection {
        Initialized,
        Connecting(ManagedPtr<aeron_async_add_subscription_t>),
        Connected(ManagedPtr<aeron_subscription_t>),
        Terminated,
    }

    pub unsafe extern "C" fn on_image_available(
        clientd: *mut ::std::os::raw::c_void,
        _subscription: *mut aeron_subscription_t,
        image: *mut aeron_image_t,
    ) {
        if clientd.is_null() {
            return;
        }
        let callback: &mut Box<dyn OnImageAvailable + 'static> =
            unsafe { &mut *(clientd as *mut Box<dyn OnImageAvailable + 'static>) };
        let constants_ptr =
            alloc(Layout::new::<aeron_image_constants_t>()) as *mut aeron_image_constants_t;
        if aeron_image_constants(image, constants_ptr) == 0 {
            let constants = unsafe { &*constants_ptr };
            let details = ImageDetails::from(constants);
            callback.on_image_available(details);
        }
    }

    pub unsafe extern "C" fn on_image_unavailable(
        clientd: *mut ::std::os::raw::c_void,
        _subscription: *mut aeron_subscription_t,
        image: *mut aeron_image_t,
    ) {
        if clientd.is_null() {
            return;
        }
        let callback: &mut Box<dyn OnImageUnavailable + 'static> =
            unsafe { &mut *(clientd as *mut Box<dyn OnImageUnavailable + 'static>) };
        let constants_ptr =
            alloc(Layout::new::<aeron_image_constants_t>()) as *mut aeron_image_constants_t;
        if aeron_image_constants(image, constants_ptr) == 0 {
            let constants = unsafe { &*constants_ptr };
            let details = ImageDetails::from(constants);
            callback.on_image_unavailable(details);
        }
    }

    pub unsafe extern "C" fn parse_reserved_value(
        clientd: *mut ::std::os::raw::c_void,
        _buffer: *mut u8,
        _frame_length: usize,
    ) -> i64 {
        if clientd.is_null() {
            return 0;
        }
        let message: &PublishMessage = unsafe { &*(clientd as *const PublishMessage) };
        message.reserved_value
    }

    pub unsafe extern "C" fn assemble(
        clientd: *mut ::std::os::raw::c_void,
        buffer: *const u8,
        length: usize,
        header_ptr: *mut aeron_header_t,
    ) {
        let payload = std::slice::from_raw_parts(buffer, length);
        let values_ptr =
            alloc(Layout::new::<aeron_header_values_t>()) as *mut aeron_header_values_t;
        aeron_header_values(header_ptr, values_ptr);
        let values = unsafe { &*values_ptr };
        let handler: &mut SubscriptionHandler =
            unsafe { &mut *(clientd as *mut SubscriptionHandler) };
        match handler {
            SubscriptionHandler::Receiver(received) => {
                received.push_back(OwnedMessage {
                    term_offset: values.frame.term_offset,
                    session_id: values.frame.session_id,
                    stream_id: values.frame.stream_id,
                    term_id: values.frame.term_id,
                    reserved_value: values.frame.reserved_value,
                    payload: payload.to_vec(),
                });
            }
            SubscriptionHandler::Callback(callback) => callback.callback(BorrowedMessage {
                term_offset: values.frame.term_offset,
                session_id: values.frame.session_id,
                stream_id: values.frame.stream_id,
                term_id: values.frame.term_id,
                reserved_value: values.frame.reserved_value,
                payload,
            }),
        }
        values_ptr.drop_in_place();
    }
}
