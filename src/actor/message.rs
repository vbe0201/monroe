/// A marker trait that represents an [`Actor`] message.
///
/// Every message is supposed to be an owned, sized type
/// that can safely be shared across thread boundaries.
///
/// This enables static validation that actors do not share
/// their exclusive state with the outer world, as intended
/// by the actor model.
///
/// This trait is implemented for eligible types by default,
/// so crate users should practically never have to do this.
pub trait Message: Sized + Send + 'static {}

impl<T: Sized + Send + 'static> Message for T {}
