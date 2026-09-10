//! Shared between the backend and (in a real stack) the wasm frontend.
//! Touching this file must invalidate every consumer.

pub fn greeting() -> &'static str {
    "hello from shared"
}
