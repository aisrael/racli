//! Server-side building blocks: [`Core`] and future service glue.

/// Holds stateless helpers shared by gRPC handlers (e.g. [`Core::version`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct Core {}

impl Core {
    /// Returns [`crate::VERSION`] as an owned string for protobuf responses.
    pub fn version(&self) -> String {
        crate::VERSION.to_string()
    }
}
