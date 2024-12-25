//! A collection of impls that provide cross-compatibility between different usage patterns and payload types.
use std::{
    cell::RefCell,
    collections::LinkedList,
    fmt::Debug,
    io::Error,
    marker::PhantomData,
    sync::{Arc, Mutex},
};

use crate::{
    Callback, CallbackRef, DriveOutcome, Flush, Publish, Receive, ReceiveOutcome, Session,
    SessionStatus,
};

/// Map a callback from one payload type to another.
///
/// When given a `Fn(I) -> O`, this implements [`Callback`].
/// When given a `Fn(&I) -> O`, this implements [`CallbackRef`].
pub struct MappingCallback<MapFunc, UnderlyingCallback, CallbackPayload, MappedPayload> {
    func: MapFunc,
    callback: UnderlyingCallback,
    _phantom: PhantomData<fn(&CallbackPayload, &MappedPayload)>,
}
impl<MapFunc, UnderlyingCallback, CallbackPayload, MappedPayload>
    MappingCallback<MapFunc, UnderlyingCallback, CallbackPayload, MappedPayload>
{
    pub fn new(func: MapFunc, callback: UnderlyingCallback) -> Self {
        Self {
            func,
            callback,
            _phantom: PhantomData,
        }
    }
}
impl<MapFunc, UnderlyingCallback, CallbackPayload, MappedPayload> Callback<CallbackPayload>
    for MappingCallback<MapFunc, UnderlyingCallback, CallbackPayload, MappedPayload>
where
    MapFunc: Fn(CallbackPayload) -> MappedPayload,
    UnderlyingCallback: Callback<MappedPayload>,
{
    fn callback(&mut self, payload: CallbackPayload) {
        self.callback.callback((self.func)(payload))
    }
}
impl<MapFunc, UnderlyingCallback, CallbackPayload, MappedPayload> CallbackRef<CallbackPayload>
    for MappingCallback<MapFunc, UnderlyingCallback, CallbackPayload, MappedPayload>
where
    MapFunc: Fn(&CallbackPayload) -> MappedPayload,
    UnderlyingCallback: Callback<MappedPayload>,
{
    fn callback_ref(&mut self, payload: &CallbackPayload) {
        self.callback.callback((self.func)(payload))
    }
}

/// Encapsulate an underlying [`Session`] that can [`Receive`], mapping the received payload using the given map function.
///
/// This effectively transforms a recevier of one type into the receiver of another (mapped) type.
pub struct MappingReceiver<MapFunc, Sess, ReceivePayload>
where
    MapFunc: for<'a> Fn(Sess::ReceivePayload<'a>) -> ReceivePayload,
    Sess: Receive,
{
    func: MapFunc,
    session: Sess,
    _phantom: PhantomData<fn(&ReceivePayload)>,
}
impl<MapFunc, Sess, ReceivePayload> MappingReceiver<MapFunc, Sess, ReceivePayload>
where
    MapFunc: for<'a> Fn(Sess::ReceivePayload<'a>) -> ReceivePayload,
    Sess: Receive,
{
    pub fn new(func: MapFunc, session: Sess) -> Self {
        Self {
            func,
            session,
            _phantom: PhantomData,
        }
    }
}
impl<MapFunc, Sess, ReceivePayload> Session for MappingReceiver<MapFunc, Sess, ReceivePayload>
where
    MapFunc: for<'a> Fn(Sess::ReceivePayload<'a>) -> ReceivePayload,
    Sess: Receive,
{
    fn drive(&mut self) -> Result<DriveOutcome, Error> {
        self.session.drive()
    }
    fn status(&self) -> SessionStatus {
        self.session.status()
    }
}
impl<MapFunc, Sess, ReceivePayload> Receive for MappingReceiver<MapFunc, Sess, ReceivePayload>
where
    MapFunc: for<'a> Fn(Sess::ReceivePayload<'a>) -> ReceivePayload + 'static,
    Sess: Receive + Publish,
    ReceivePayload: 'static,
{
    type ReceivePayload<'a> = ReceivePayload
        where
            Self: 'a;
    fn receive<'a>(&'a mut self) -> Result<ReceiveOutcome<Self::ReceivePayload<'a>>, Error> {
        Ok(match self.session.receive()? {
            ReceiveOutcome::Idle => ReceiveOutcome::Idle,
            ReceiveOutcome::Active => ReceiveOutcome::Active,
            ReceiveOutcome::Payload(payload) => ReceiveOutcome::Payload((self.func)(payload)),
        })
    }
}
impl<MapFunc, Sess, ReceivePayload> Publish for MappingReceiver<MapFunc, Sess, ReceivePayload>
where
    MapFunc: for<'a> Fn(Sess::ReceivePayload<'a>) -> ReceivePayload + 'static,
    Sess: Receive + Publish,
    ReceivePayload: 'static,
{
    type PublishPayload<'a> = Sess::PublishPayload<'a>
        where
            Self: 'a;
    fn publish<'a>(
        &mut self,
        payload: Self::PublishPayload<'a>,
    ) -> Result<crate::PublishOutcome<Self::PublishPayload<'a>>, Error> {
        self.session.publish(payload)
    }
}
impl<MapFunc, Sess, ReceivePayload> Flush for MappingReceiver<MapFunc, Sess, ReceivePayload>
where
    MapFunc: for<'a> Fn(Sess::ReceivePayload<'a>) -> ReceivePayload + 'static,
    Sess: Receive + Flush,
    ReceivePayload: 'static,
{
    fn flush(&mut self) -> Result<(), Error> {
        self.session.flush()
    }
}
impl<MapFunc, Sess, ReceivePayload> Debug for MappingReceiver<MapFunc, Sess, ReceivePayload>
where
    MapFunc: for<'a> Fn(Sess::ReceivePayload<'a>) -> ReceivePayload,
    Sess: Receive,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappingReceiver")
            .field("session", &self.session)
            .finish()
    }
}

/// Encapsulates a [`Session`] and a [`Queue`], polling from the [`Queue`] to implement [`Receive`].
///
/// This is meant to be coupled with a [`QueueCallback`] or [`QueueCallbackRef`], transforming a push-oriented callback into a poll-oriented receiver.
/// Payloads dispatched to the [`QueueCallback`]/[`QueueCallbackRef`] are queued so that they may be polled via [`Receive::receive`].
///
/// [`Queue`] is a trait, giving users have the flexibility to choose an implementation that satisfies their [`Publish`]/[`Sync`] requirements.
pub struct QueueReceiver<Sess, Payload, QueueImpl>
where
    Sess: Session,
    QueueImpl: Queue<Payload>,
{
    session: Sess,
    queue: QueueImpl,
    _phantom: PhantomData<fn(&Payload)>,
}
impl<Sess, Payload, QueueImpl> QueueReceiver<Sess, Payload, QueueImpl>
where
    Sess: Session,
    QueueImpl: Queue<Payload>,
{
    /// Encapsulate the underlying session and queue
    pub fn new(session: Sess, queue: QueueImpl) -> Self {
        Self {
            session,
            queue,
            _phantom: PhantomData,
        }
    }
}
impl<Sess, Payload, QueueImpl> Receive for QueueReceiver<Sess, Payload, QueueImpl>
where
    Sess: Session,
    QueueImpl: Queue<Payload>,
{
    type ReceivePayload<'a> = Payload
    where
        Self: 'a;
    fn receive<'a>(&'a mut self) -> Result<ReceiveOutcome<Self::ReceivePayload<'a>>, Error> {
        match self.queue.pop() {
            None => Ok(ReceiveOutcome::Idle),
            Some(x) => Ok(ReceiveOutcome::Payload(x)),
        }
    }
}
impl<Sess, Payload, QueueImpl> Session for QueueReceiver<Sess, Payload, QueueImpl>
where
    Sess: Session,
    QueueImpl: Queue<Payload>,
{
    fn drive(&mut self) -> Result<DriveOutcome, Error> {
        self.session.drive()
    }
    fn status(&self) -> SessionStatus {
        self.session.status()
    }
}
impl<Sess, Payload, QueueImpl> Publish for QueueReceiver<Sess, Payload, QueueImpl>
where
    Sess: Publish,
    QueueImpl: Queue<Payload> + 'static,
    Payload: 'static,
{
    type PublishPayload<'a> = Sess::PublishPayload<'a>
        where
            Self: 'a;
    fn publish<'a>(
        &mut self,
        payload: Self::PublishPayload<'a>,
    ) -> Result<crate::PublishOutcome<Self::PublishPayload<'a>>, Error> {
        self.session.publish(payload)
    }
}
impl<Sess, Payload, QueueImpl> Flush for QueueReceiver<Sess, Payload, QueueImpl>
where
    Sess: Flush,
    QueueImpl: Queue<Payload> + 'static,
    Payload: 'static,
{
    fn flush(&mut self) -> Result<(), Error> {
        self.session.flush()
    }
}
impl<Sess, Payload, QueueImpl> Debug for QueueReceiver<Sess, Payload, QueueImpl>
where
    Sess: Session,
    QueueImpl: Queue<Payload>,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueueReceiver")
            .field("session", &self.session)
            .finish()
    }
}

/// A simple queue implementation. [`RefCellQueue`] and [`MutexQueue`] should satisfy most requirements.
pub trait Queue<T> {
    fn push(&self, x: T);
    fn pop(&self) -> Option<T>;
}

/// A [`Queue`] implemented with a `Arc<RefCell<LinkedList<T>>>`, which is not [`Sync`].
pub struct RefCellQueue<T> {
    queue: Arc<RefCell<LinkedList<T>>>,
}
impl<T> RefCellQueue<T> {
    pub fn new() -> Self {
        Self {
            queue: Arc::new(RefCell::new(LinkedList::new())),
        }
    }
}
impl<T> Queue<T> for RefCellQueue<T> {
    fn pop(&self) -> Option<T> {
        self.queue.borrow_mut().pop_front()
    }
    fn push(&self, x: T) {
        self.queue.borrow_mut().push_front(x)
    }
}

/// A [`Queue`] implemented with a `Arc<Mutex<LinkedList<T>>>`, which is [`Sync`].
pub struct MutexQueue<T> {
    queue: Arc<Mutex<LinkedList<T>>>,
}
impl<T> MutexQueue<T> {
    pub fn new() -> Self {
        Self {
            queue: Arc::new(Mutex::new(LinkedList::new())),
        }
    }
}
impl<T> Queue<T> for MutexQueue<T> {
    fn pop(&self) -> Option<T> {
        // mutex is internal, it is impossible to panic within the lock
        self.queue.lock().expect("MutexQueue lock").pop_front()
    }
    fn push(&self, x: T) {
        // mutex is internal, it is impossible to panic within the lock
        self.queue.lock().expect("MutexQueue lock").push_front(x)
    }
}

/// Generated by [`QueueReceiver`], dispatched payloads will be added to the shared queue so that they may be polled via
/// the [`QueueReceiver`] impl of [`Receive::receive`].
pub struct QueueCallback<CallbackPayload, QueuePayload, MapFunc>
where
    MapFunc: Fn(CallbackPayload) -> QueuePayload,
{
    queue: Arc<RefCell<LinkedList<QueuePayload>>>,
    map_func: MapFunc,
    _phantom: PhantomData<CallbackPayload>,
}
impl<CallbackPayload, QueuePayload, MapFunc> Callback<CallbackPayload>
    for QueueCallback<CallbackPayload, QueuePayload, MapFunc>
where
    MapFunc: Fn(CallbackPayload) -> QueuePayload,
{
    fn callback(&mut self, data: CallbackPayload) {
        self.queue.borrow_mut().push_back((self.map_func)(data));
    }
}

/// Generated by [`QueueReceiver`], dispatched payloads will be added to the shared queue so that they may be polled via
/// the [`QueueReceiver`] impl of [`Receive::receive`].
pub struct QueueCallbackRef<CallbackPayload, QueuePayload, MapFunc>
where
    MapFunc: Fn(&CallbackPayload) -> QueuePayload,
{
    queue: Arc<RefCell<LinkedList<QueuePayload>>>,
    map_func: MapFunc,
    _phantom: PhantomData<CallbackPayload>,
}
impl<CallbackPayload, QueuePayload, MapFunc> CallbackRef<CallbackPayload>
    for QueueCallbackRef<CallbackPayload, QueuePayload, MapFunc>
where
    MapFunc: Fn(&CallbackPayload) -> QueuePayload,
{
    fn callback_ref(&mut self, data: &CallbackPayload) {
        self.queue.borrow_mut().push_back((self.map_func)(data));
    }
}

/// This transforms any poll-oriented [`Receive`]-capable [`Session`] into a push-oriented [`Callback`].
///
/// This encapsulate a [`Callback`] and [`Session`] that can [`Receive`].
/// Calls to [`CallbackDriver::drive`] will [`Receive::receive`] and dispatch any received data to the underlying [`Callback`].'
pub struct CallbackDriver<R, C>
where
    R: Receive,
    C: for<'a> Callback<R::ReceivePayload<'a>> + 'static,
{
    session: R,
    callback: C,
}
impl<R, C> Session for CallbackDriver<R, C>
where
    R: Receive,
    C: for<'a> Callback<R::ReceivePayload<'a>> + 'static,
{
    fn drive(&mut self) -> Result<DriveOutcome, Error> {
        let mut outcome = self.session.drive()?;
        if let ReceiveOutcome::Payload(data) = self.session.receive()? {
            (self.callback).callback(data);
            outcome = DriveOutcome::Active;
        }
        Ok(outcome)
    }

    fn status(&self) -> SessionStatus {
        self.session.status()
    }
}
impl<S, C> Publish for CallbackDriver<S, C>
where
    S: Publish + Receive,
    C: for<'a> Callback<S::ReceivePayload<'a>> + 'static,
{
    type PublishPayload<'a> = S::PublishPayload<'a>
        where
            Self: 'a;
    fn publish<'a>(
        &mut self,
        data: Self::PublishPayload<'a>,
    ) -> Result<crate::PublishOutcome<Self::PublishPayload<'a>>, Error> {
        self.session.publish(data)
    }
}
impl<S, C> Flush for CallbackDriver<S, C>
where
    S: Flush + Receive,
    C: for<'a> Callback<S::ReceivePayload<'a>> + 'static,
{
    fn flush(&mut self) -> Result<(), Error> {
        self.session.flush()
    }
}
impl<R, C> Debug for CallbackDriver<R, C>
where
    R: Receive,
    C: for<'a> Callback<R::ReceivePayload<'a>> + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackDriver")
            .field("session", &self.session)
            .finish()
    }
}

/// This transforms any poll-oriented [`Receive`]-capable [`Session`] into a push-oriented [`CallbackRef`].
///
/// This encapsulate a [`Callback`] and [`Session`] that can [`Receive`].
/// Calls to [`CallbackDriver::drive`] will [`Receive::receive`] and dispatch any received data to the underlying [`CallbackRef`].'
pub struct CallbackRefDriver<R, C>
where
    R: Receive,
    C: for<'a> CallbackRef<R::ReceivePayload<'a>> + 'static,
{
    session: R,
    callback: C,
}
impl<R, C> Session for CallbackRefDriver<R, C>
where
    R: Receive,
    C: for<'a> CallbackRef<R::ReceivePayload<'a>> + 'static,
{
    fn drive(&mut self) -> Result<DriveOutcome, Error> {
        let mut outcome = self.session.drive()?;
        if let ReceiveOutcome::Payload(data) = self.session.receive()? {
            (self.callback).callback_ref(&data);
            outcome = DriveOutcome::Active;
        }
        Ok(outcome)
    }

    fn status(&self) -> SessionStatus {
        self.session.status()
    }
}
impl<S, C> Publish for CallbackRefDriver<S, C>
where
    S: Publish + Receive,
    C: for<'a> CallbackRef<S::ReceivePayload<'a>> + 'static,
{
    type PublishPayload<'a> = S::PublishPayload<'a>
        where
            Self: 'a;
    fn publish<'a>(
        &mut self,
        payload: Self::PublishPayload<'a>,
    ) -> Result<crate::PublishOutcome<Self::PublishPayload<'a>>, Error> {
        self.session.publish(payload)
    }
}
impl<S, C> Flush for CallbackRefDriver<S, C>
where
    S: Flush + Receive,
    C: for<'a> CallbackRef<S::ReceivePayload<'a>> + 'static,
{
    fn flush(&mut self) -> Result<(), Error> {
        self.session.flush()
    }
}
impl<R, C> Debug for CallbackRefDriver<R, C>
where
    R: Receive,
    C: for<'a> CallbackRef<R::ReceivePayload<'a>> + 'static,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallbackRefDriver")
            .field("session", &self.session)
            .finish()
    }
}
